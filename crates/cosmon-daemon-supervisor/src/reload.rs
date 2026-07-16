// SPDX-License-Identifier: AGPL-3.0-only

//! Hot-reload diff — partition `old ∪ new` into `{spawn, kill, keep, changed}`.
//!
//! When `~/.config/cosmon/daemons.toml` changes on disk, the supervisor must
//! decide, per daemon name:
//!
//! | old | new | old hash == new hash | diagnosis |
//! |-----|-----|-----------------------|-----------|
//! | —   | ✓   | n/a                   | [`DiffResult::spawn`]   |
//! | ✓   | —   | n/a                   | [`DiffResult::kill`]    |
//! | ✓   | ✓   | yes                   | [`DiffResult::keep`]    |
//! | ✓   | ✓   | no                    | [`DiffResult::changed`] |
//!
//! The *hash* is a [BLAKE3] digest over a canonical byte serialization of
//! the spec. We use BLAKE3 (and not `PartialEq`) for two reasons:
//!
//! 1. Cheap, stable content identity we can log ("tg-bot changed:
//!    `3f2a…` → `9d11…`") when the supervisor later ships audit logs.
//! 2. Forward-compat: two semantically identical specs must hash the same
//!    regardless of TOML key order, whitespace, or `env` map insertion
//!    order. The canonical form (sorted keys, no whitespace, lexicographic
//!    env) is what guarantees that.
//!
//! # Invariants (enforced by proptest)
//!
//! Given any two `HashMap<String, DaemonSpec>` inputs `old` and `new`, the
//! returned [`DiffResult`] satisfies:
//!
//! - **Partition**: the union of the four lists equals `old.keys() ∪ new.keys()`.
//! - **Disjoint**: every name appears in exactly one list.
//! - **Determinism**: each list is lexicographically sorted (stable output).
//!
//! The partition property is what lets the event loop reason safely about
//! the diff — you can't forget to handle a daemon, and you can't double-handle
//! one either.
//!
//! [BLAKE3]: https://github.com/BLAKE3-team/BLAKE3

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::BuildHasher;

use serde::Serialize;

use crate::config::DaemonSpec;

// ---------------------------------------------------------------------------
// DiffResult
// ---------------------------------------------------------------------------

/// Outcome of hot-reload diff. All four vectors are lexicographically sorted.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct DiffResult {
    /// Names that appear in `new` but not in `old` — must be spawned.
    pub spawn: Vec<String>,
    /// Names that appear in `old` but not in `new` — must be stopped.
    pub kill: Vec<String>,
    /// Names that appear in both and hash identically — leave alone.
    pub keep: Vec<String>,
    /// Names that appear in both but differ — stop + spawn (never silently
    /// mutate the running process).
    pub changed: Vec<String>,
}

impl DiffResult {
    /// Total number of daemons considered across all four buckets.
    #[must_use]
    pub fn total(&self) -> usize {
        self.spawn.len() + self.kill.len() + self.keep.len() + self.changed.len()
    }

    /// `true` iff every list is empty (no work to do).
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.spawn.is_empty()
            && self.kill.is_empty()
            && self.keep.is_empty()
            && self.changed.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Content hashing
// ---------------------------------------------------------------------------

/// Canonical form of a [`DaemonSpec`] used for content hashing. Matches the
/// semantic identity of the spec: same binary, args, env, throttle, log
/// paths, enabled flag, kill-switch → same hash, regardless of TOML
/// surface syntax.
///
/// `BTreeMap` gives us sorted-key JSON without pulling in `cosmon-hash`'s
/// canonical-JSON machinery (lighter dependency for this crate). `serde_json`
/// with `BTreeMap` produces deterministic output because `BTreeMap`'s
/// iteration order is its key order.
#[derive(Debug, Serialize)]
struct CanonicalSpec<'a> {
    name: &'a str,
    binary: &'a str,
    args: &'a [String],
    throttle_seconds: u64,
    env: &'a BTreeMap<String, String>,
    log_stdout: Option<&'a str>,
    log_stderr: Option<&'a str>,
    kill_switch: Option<&'a str>,
    enabled: bool,
}

/// BLAKE3 hash over the canonical form of the spec. Stable across TOML
/// reorderings; changes on every semantic difference.
///
/// # Panics
///
/// This function is infallible in practice: `CanonicalSpec` only contains
/// strings, numbers, booleans, and a `BTreeMap<String, String>` — all types
/// that `serde_json::to_vec` serializes without error. The `expect` is kept
/// rather than falling back silently so any future field that could break
/// serialization fails loudly in tests.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use cosmon_daemon_supervisor::config::DaemonSpec;
/// use cosmon_daemon_supervisor::reload::spec_content_hash;
///
/// let a = DaemonSpec {
///     name: "x".into(),
///     binary: "/bin/echo".into(),
///     args: vec![],
///     throttle_seconds: 30,
///     env: BTreeMap::new(),
///     log_stdout: None,
///     log_stderr: None,
///     kill_switch: None,
///     enabled: true,
/// };
/// let mut b = a.clone();
/// assert_eq!(spec_content_hash(&a), spec_content_hash(&b));
/// b.throttle_seconds = 31;
/// assert_ne!(spec_content_hash(&a), spec_content_hash(&b));
/// ```
#[must_use]
pub fn spec_content_hash(spec: &DaemonSpec) -> blake3::Hash {
    let canonical = CanonicalSpec {
        name: &spec.name,
        binary: &spec.binary,
        args: &spec.args,
        throttle_seconds: spec.throttle_seconds,
        env: &spec.env,
        log_stdout: spec.log_stdout.as_deref(),
        log_stderr: spec.log_stderr.as_deref(),
        kill_switch: spec.kill_switch.as_deref(),
        enabled: spec.enabled,
    };
    // `serde_json::to_vec` with a `BTreeMap` env field yields deterministic
    // bytes — the only field with any order ambiguity is `env`, and BTreeMap
    // iterates in key order. We deliberately *don't* sort struct fields
    // lexicographically: struct fields are serialized in declaration order,
    // which is stable across compilations.
    let bytes =
        serde_json::to_vec(&canonical).expect("CanonicalSpec contains only serde-safe leaf types");
    blake3::hash(&bytes)
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

/// Partition `old ∪ new` into `{spawn, kill, keep, changed}`.
///
/// See the module docs for the full semantics and invariants. The inputs
/// are taken by reference so the caller keeps ownership of the spec maps;
/// the returned [`DiffResult`] owns its name strings.
///
/// Generic over the maps' hashers so callers can pass the default
/// `HashMap<K,V>`, `indexmap::IndexMap`-style wrappers, or any custom
/// `BuildHasher` without a copy.
#[must_use]
pub fn diff<S1: BuildHasher, S2: BuildHasher>(
    old: &HashMap<String, DaemonSpec, S1>,
    new: &HashMap<String, DaemonSpec, S2>,
) -> DiffResult {
    let old_names: HashSet<&String> = old.keys().collect();
    let new_names: HashSet<&String> = new.keys().collect();

    let mut out = DiffResult::default();

    // spawn = new \ old
    for n in new_names.difference(&old_names) {
        out.spawn.push((*n).clone());
    }
    // kill = old \ new
    for n in old_names.difference(&new_names) {
        out.kill.push((*n).clone());
    }
    // keep / changed = old ∩ new, split by content hash
    for n in old_names.intersection(&new_names) {
        let h_old = spec_content_hash(&old[*n]);
        let h_new = spec_content_hash(&new[*n]);
        if h_old == h_new {
            out.keep.push((*n).clone());
        } else {
            out.changed.push((*n).clone());
        }
    }

    // Deterministic output — callers can diff two DiffResults freely.
    out.spawn.sort();
    out.kill.sort();
    out.keep.sort();
    out.changed.sort();

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn spec(name: &str, binary: &str, args: &[&str]) -> DaemonSpec {
        DaemonSpec {
            name: name.into(),
            binary: binary.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            throttle_seconds: 30,
            env: BTreeMap::new(),
            log_stdout: None,
            log_stderr: None,
            kill_switch: None,
            enabled: true,
        }
    }

    fn map(specs: Vec<DaemonSpec>) -> HashMap<String, DaemonSpec> {
        specs.into_iter().map(|s| (s.name.clone(), s)).collect()
    }

    #[test]
    fn spawn_when_new_introduces_names() {
        let old = HashMap::new();
        let new = map(vec![spec("a", "/bin/a", &[]), spec("b", "/bin/b", &[])]);
        let d = diff(&old, &new);
        assert_eq!(d.spawn, vec!["a", "b"]);
        assert!(d.kill.is_empty() && d.keep.is_empty() && d.changed.is_empty());
    }

    #[test]
    fn kill_when_old_loses_names() {
        let old = map(vec![spec("a", "/bin/a", &[]), spec("b", "/bin/b", &[])]);
        let new = HashMap::new();
        let d = diff(&old, &new);
        assert_eq!(d.kill, vec!["a", "b"]);
        assert!(d.spawn.is_empty());
    }

    #[test]
    fn keep_when_unchanged() {
        let a1 = spec("a", "/bin/a", &[]);
        let a2 = spec("a", "/bin/a", &[]);
        let old = map(vec![a1]);
        let new = map(vec![a2]);
        let d = diff(&old, &new);
        assert_eq!(d.keep, vec!["a"]);
        assert!(d.changed.is_empty());
    }

    #[test]
    fn changed_when_any_field_differs() {
        let a1 = spec("a", "/bin/a", &["--foo"]);
        let a2 = spec("a", "/bin/a", &["--bar"]);
        let old = map(vec![a1]);
        let new = map(vec![a2]);
        let d = diff(&old, &new);
        assert_eq!(d.changed, vec!["a"]);
        assert!(d.keep.is_empty());
    }

    #[test]
    fn changed_when_env_flips_value() {
        let mut a1 = spec("a", "/bin/a", &[]);
        let mut a2 = spec("a", "/bin/a", &[]);
        a1.env.insert("RUST_LOG".into(), "info".into());
        a2.env.insert("RUST_LOG".into(), "debug".into());
        let old = map(vec![a1]);
        let new = map(vec![a2]);
        let d = diff(&old, &new);
        assert_eq!(d.changed, vec!["a"]);
    }

    #[test]
    fn keep_when_env_reordered_but_same_content() {
        // `BTreeMap` insertion order doesn't matter: both variants should
        // produce identical canonical JSON and therefore identical hashes.
        let mut a1 = spec("a", "/bin/a", &[]);
        let mut a2 = spec("a", "/bin/a", &[]);
        a1.env.insert("A".into(), "1".into());
        a1.env.insert("B".into(), "2".into());
        a2.env.insert("B".into(), "2".into());
        a2.env.insert("A".into(), "1".into());
        let d = diff(&map(vec![a1]), &map(vec![a2]));
        assert_eq!(d.keep, vec!["a"]);
    }

    #[test]
    fn mixed_reload_buckets_correctly() {
        // old: {keep-me, retire-me, tweak-me}
        // new: {keep-me, tweak-me (changed args), new-one}
        // expected: spawn = [new-one], kill = [retire-me],
        //           keep = [keep-me], changed = [tweak-me]
        let old = map(vec![
            spec("keep-me", "/bin/k", &[]),
            spec("retire-me", "/bin/r", &[]),
            spec("tweak-me", "/bin/t", &["--v1"]),
        ]);
        let new = map(vec![
            spec("keep-me", "/bin/k", &[]),
            spec("tweak-me", "/bin/t", &["--v2"]),
            spec("new-one", "/bin/n", &[]),
        ]);
        let d = diff(&old, &new);
        assert_eq!(d.spawn, vec!["new-one"]);
        assert_eq!(d.kill, vec!["retire-me"]);
        assert_eq!(d.keep, vec!["keep-me"]);
        assert_eq!(d.changed, vec!["tweak-me"]);
        assert_eq!(d.total(), 4);
    }

    #[test]
    fn empty_old_and_new_is_noop() {
        let d = diff(&HashMap::new(), &HashMap::new());
        assert!(d.is_noop());
        assert_eq!(d.total(), 0);
    }

    // Strategy: build arbitrary spec maps from small alphabets so the
    // proptest is fast but still exercises every code path.
    fn arb_spec(name: String) -> impl Strategy<Value = DaemonSpec> {
        (
            "[a-z]{1,4}",
            prop::collection::vec("[a-z0-9]{0,3}", 0..3),
            0u64..120,
            any::<bool>(),
        )
            .prop_map(move |(bin, args, thr, enabled)| DaemonSpec {
                name: name.clone(),
                binary: format!("/bin/{bin}"),
                args,
                throttle_seconds: thr,
                env: BTreeMap::new(),
                log_stdout: None,
                log_stderr: None,
                kill_switch: None,
                enabled,
            })
    }

    fn arb_map() -> impl Strategy<Value = HashMap<String, DaemonSpec>> {
        // Small name alphabet (5 possible names) so overlaps between
        // old/new are frequent — otherwise we'd almost never exercise
        // the "keep" or "changed" buckets.
        prop::collection::hash_set("[a-e]", 0..5).prop_flat_map(|names| {
            let specs: Vec<_> = names.into_iter().map(|n| arb_spec(n.clone())).collect();
            specs.prop_map(|v| v.into_iter().map(|s| (s.name.clone(), s)).collect())
        })
    }

    proptest! {
        // # Partition invariant
        //
        // For any two spec maps, `diff` returns four lists whose union is
        // exactly `old.keys() ∪ new.keys()` with no duplicates. This is the
        // property the event loop relies on — miss it and the supervisor
        // could silently drop or double-handle a daemon on reload.
        #[test]
        fn diff_is_a_partition_of_the_union(old in arb_map(), new in arb_map()) {
            let d = diff(&old, &new);
            let mut union: HashSet<String> = HashSet::new();
            union.extend(old.keys().cloned());
            union.extend(new.keys().cloned());

            let mut seen: HashSet<String> = HashSet::new();
            for n in d.spawn.iter().chain(&d.kill).chain(&d.keep).chain(&d.changed) {
                prop_assert!(seen.insert(n.clone()), "name appeared in two buckets: {n}");
            }
            prop_assert_eq!(&seen, &union);
            prop_assert_eq!(d.total(), union.len());
        }

        // # Spawn / kill membership
        //
        // spawn = new \ old, kill = old \ new. This is the schema the
        // caller relies on — swap spawn and kill and the supervisor
        // immediately kills all the daemons it was supposed to start.
        #[test]
        fn spawn_and_kill_obey_set_difference(old in arb_map(), new in arb_map()) {
            let d = diff(&old, &new);
            for n in &d.spawn {
                prop_assert!(!old.contains_key(n));
                prop_assert!(new.contains_key(n));
            }
            for n in &d.kill {
                prop_assert!(old.contains_key(n));
                prop_assert!(!new.contains_key(n));
            }
        }

        // # keep ⇔ same content hash
        //
        // For every name in both maps, it is in `keep` iff the hashes match
        // and in `changed` otherwise. This keeps `spec_content_hash` honest:
        // any future change to the hashing rule must keep this property.
        #[test]
        fn keep_iff_content_hashes_match(old in arb_map(), new in arb_map()) {
            let d = diff(&old, &new);
            for n in &d.keep {
                let h_old = spec_content_hash(&old[n]);
                let h_new = spec_content_hash(&new[n]);
                prop_assert_eq!(h_old, h_new);
            }
            for n in &d.changed {
                let h_old = spec_content_hash(&old[n]);
                let h_new = spec_content_hash(&new[n]);
                prop_assert_ne!(h_old, h_new);
            }
        }

        // # Determinism
        //
        // Output lists are lexicographically sorted. Callers can compare
        // two DiffResults directly without sorting first.
        #[test]
        fn diff_output_is_sorted(old in arb_map(), new in arb_map()) {
            let d = diff(&old, &new);
            for list in [&d.spawn, &d.kill, &d.keep, &d.changed] {
                let mut sorted = list.clone();
                sorted.sort();
                prop_assert_eq!(list, &sorted);
            }
        }
    }
}
