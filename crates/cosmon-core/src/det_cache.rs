// SPDX-License-Identifier: AGPL-3.0-only

//! Content-addressable molecule cache for deterministic formulas (ADR-140 D5).
//!
//! This module absorbs `OxyMake`'s content-addressable build cache **into cs
//! as one ontology** rather than a second binary. A formula declared
//! [`deterministic`](crate::formula::Formula::deterministic) is a pure function
//! of its inputs: the same resolved variables plus the same upstream input
//! artifacts yield the same output bytes. Such a molecule is **cachable by
//! content** — the runtime computes a [`CacheKey`] and, if a prior run already
//! populated the cache, **skips execution** and links the cached artifact.
//!
//! # The key (ADR-140 D5, reusing ADR-043 hashing)
//!
//! ```text
//! cache_key = BLAKE3(formula_id || resolved_vars || sorted(input_artifact_hashes))
//! ```
//!
//! No new hashing primitive is introduced: the key is built with
//! [`cosmon_hash`]'s canonical serialization (the same machinery ADR-043 uses
//! for step input hashing) and BLAKE3. Two structurally equal sets of inputs
//! always produce the same key, regardless of variable iteration order or the
//! order in which input artifact hashes are presented.
//!
//! # Zero-I/O core
//!
//! The [`MoleculeCache`] trait is the seam. Filesystem-backed implementations
//! (the `.cosmon/cache/<cache_key>` store) live in the shell; this module ships
//! only the pure key derivation, the decision logic
//! ([`resolve`]), and an in-memory reference implementation
//! ([`InMemoryMoleculeCache`]) used by tests and callers that want a process-
//! local cache.
//!
//! # Determinism gate
//!
//! [`resolve`] couples the cache to the formula trait: an **agentic** formula
//! (`deterministic = false`) is **never** content-cached — [`resolve`] returns
//! [`CacheOutcome::Run`] without even consulting the cache. Only a
//! deterministic formula consults the store, skips on a hit, and runs (then
//! populates) on a miss.

use std::collections::BTreeMap;
use std::sync::Mutex;

use cosmon_hash::{hash_value, CanonicalError, Hash};
use serde::Serialize;

use crate::cas::ContentHash;
use crate::error::CosmonError;
use crate::formula::Formula;

/// The content-addressable identity of a deterministic molecule's work.
///
/// Derived by [`cache_key`] from the formula id, the resolved variables, and
/// the sorted input-artifact hashes. Equal inputs yield an equal `CacheKey`;
/// any change to a variable or an input artifact yields a different one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CacheKey(Hash);

impl CacheKey {
    /// The underlying 32-byte BLAKE3 digest.
    #[must_use]
    pub const fn digest(&self) -> &Hash {
        &self.0
    }

    /// Lowercase hex of the key (64 chars) — the on-disk cache directory name.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// Compute the content-addressable cache key for a deterministic molecule.
///
/// `cache_key = BLAKE3(formula_id || resolved_vars || sorted(input_artifact_hashes))`
///
/// The inputs are folded into a canonical JSON envelope (sorted object keys,
/// no whitespace) and hashed with BLAKE3 via [`cosmon_hash::hash_value`], so:
///
/// * `resolved_vars` is a [`BTreeMap`] — its serialization is order-stable.
/// * `input_artifact_hashes` is sorted here before hashing, so the caller may
///   present upstream artifact hashes in any order.
///
/// # Errors
///
/// Returns [`CanonicalError`] only if canonical serialization fails, which in
/// practice cannot happen for the plain string/`BTreeMap` envelope used here.
pub fn cache_key(
    formula_id: &str,
    resolved_vars: &BTreeMap<String, String>,
    input_artifact_hashes: &[Hash],
) -> Result<CacheKey, CanonicalError> {
    #[derive(Serialize)]
    struct KeyEnvelope<'a> {
        formula_id: &'a str,
        resolved_vars: &'a BTreeMap<String, String>,
        input_artifact_hashes: Vec<String>,
    }

    // Sort the input hashes so presentation order does not affect the key.
    let mut sorted: Vec<String> = input_artifact_hashes.iter().map(|h| h.to_hex()).collect();
    sorted.sort();

    let digest = hash_value(&KeyEnvelope {
        formula_id,
        resolved_vars,
        input_artifact_hashes: sorted,
    })?;
    Ok(CacheKey(digest))
}

/// A cached output: the content hash of the artifact a prior deterministic run
/// produced.
///
/// On a [`CacheOutcome::Skip`] the runtime links this artifact into the
/// molecule directory instead of re-executing the work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedOutput {
    /// Content hash of the cached output artifact, addressable in a
    /// [`crate::cas::CasStore`].
    pub artifact: ContentHash,
}

impl CachedOutput {
    /// Construct a cached output from a stored artifact's content hash.
    #[must_use]
    pub fn new(artifact: ContentHash) -> Self {
        Self { artifact }
    }
}

/// The content-addressable molecule cache seam.
///
/// Implementations map a [`CacheKey`] to the [`CachedOutput`] a prior
/// deterministic run produced. The trait is layout-agnostic: the on-disk
/// `.cosmon/cache/<cache_key>` store is a shell-side implementation detail.
///
/// # Idempotence
///
/// [`store`](MoleculeCache::store) overwrites any prior entry for the same key
/// with byte-identical content (deterministic formulas produce identical
/// output for identical inputs), so storing twice is a no-op in effect.
pub trait MoleculeCache {
    /// Look up the cached output for a key. Returns `None` on a miss.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on backend (I/O) failure.
    fn lookup(&self, key: &CacheKey) -> Result<Option<CachedOutput>, CosmonError>;

    /// Populate the cache for a key with a freshly produced output.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on backend (I/O) failure.
    fn store(&self, key: &CacheKey, output: CachedOutput) -> Result<(), CosmonError>;
}

/// The decision the runtime acts on after consulting the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheOutcome {
    /// A cache hit: skip execution and link the cached artifact.
    Skip(CachedOutput),
    /// A cache miss (or an agentic formula): run the work. The caller is
    /// expected to [`MoleculeCache::store`] the result afterwards **iff** the
    /// formula is deterministic (see [`resolve`]).
    Run,
}

/// Resolve whether a molecule's work can be skipped via the content cache.
///
/// This is the single place that couples the [`Formula::deterministic`] trait
/// to the cache (ADR-140 D5):
///
/// * **Agentic** formula (`deterministic = false`): returns
///   [`CacheOutcome::Run`] *without consulting the cache*. An LLM session is
///   not byte-reproducible, so it is never content-skipped — it is sealed and
///   re-executed.
/// * **Deterministic** formula with a cache **hit**: returns
///   [`CacheOutcome::Skip`] carrying the cached artifact.
/// * **Deterministic** formula with a cache **miss**: returns
///   [`CacheOutcome::Run`]; the caller runs the work and then calls
///   [`MoleculeCache::store`] to populate the cache for next time.
///
/// # Errors
///
/// Propagates any [`CosmonError`] raised by [`MoleculeCache::lookup`].
pub fn resolve(
    formula: &Formula,
    cache: &dyn MoleculeCache,
    key: &CacheKey,
) -> Result<CacheOutcome, CosmonError> {
    if !formula.deterministic {
        // Agentic formula: never content-cached.
        return Ok(CacheOutcome::Run);
    }
    match cache.lookup(key)? {
        Some(output) => Ok(CacheOutcome::Skip(output)),
        None => Ok(CacheOutcome::Run),
    }
}

/// A process-local, in-memory [`MoleculeCache`] reference implementation.
///
/// Pure (no I/O); suitable for tests, dry-runs, and callers that want a cache
/// scoped to a single process. The persistent `.cosmon/cache/<cache_key>`
/// backend lives in the shell.
#[derive(Debug, Default)]
pub struct InMemoryMoleculeCache {
    entries: Mutex<BTreeMap<String, CachedOutput>>,
}

impl InMemoryMoleculeCache {
    /// Construct an empty in-memory cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl MoleculeCache for InMemoryMoleculeCache {
    fn lookup(&self, key: &CacheKey) -> Result<Option<CachedOutput>, CosmonError> {
        let guard = self.entries.lock().map_err(|_| CosmonError::Runtime {
            reason: "molecule cache mutex poisoned".to_owned(),
        })?;
        Ok(guard.get(&key.to_hex()).cloned())
    }

    fn store(&self, key: &CacheKey, output: CachedOutput) -> Result<(), CosmonError> {
        let mut guard = self.entries.lock().map_err(|_| CosmonError::Runtime {
            reason: "molecule cache mutex poisoned".to_owned(),
        })?;
        guard.insert(key.to_hex(), output);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BLAKE3 of arbitrary bytes — a stand-in upstream artifact hash.
    fn h(bytes: &[u8]) -> Hash {
        Hash::of_bytes(bytes)
    }

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A fake content hash (64 lowercase hex chars) for tests.
    fn art(seed: u8) -> ContentHash {
        use std::fmt::Write as _;
        let mut hex = String::with_capacity(64);
        for _ in 0..32 {
            let _ = write!(hex, "{seed:02x}");
        }
        ContentHash::new(hex).unwrap()
    }

    fn deterministic_formula() -> Formula {
        Formula::parse(
            r#"
formula = "build-thing"
version = 1
description = "A pure build"
deterministic = true

[[steps]]
id = "build"
title = "Build"
description = "Compile."
"#,
        )
        .unwrap()
    }

    fn agentic_formula() -> Formula {
        Formula::parse(
            r#"
formula = "research-thing"
version = 1
description = "An LLM session"

[[steps]]
id = "think"
title = "Think"
description = "Reason."
"#,
        )
        .unwrap()
    }

    #[test]
    fn deterministic_defaults_to_false() {
        // A formula that omits `deterministic` is agentic by default — this is
        // the backward-compatible behavior every existing formula relies on.
        let f = agentic_formula();
        assert!(!f.deterministic);
        assert!(f.verify_requires_execution());
    }

    #[test]
    fn deterministic_true_parses_and_couples_verify() {
        // `deterministic = true` parses and couples verify_requires_execution
        // to its inverse (ADR-140 D5).
        let f = deterministic_formula();
        assert!(f.deterministic);
        assert!(!f.verify_requires_execution());
    }

    #[test]
    fn cache_hit_skips() {
        // Deterministic formula + a populated cache → Skip, carrying the
        // cached artifact. Execution is bypassed.
        let f = deterministic_formula();
        let cache = InMemoryMoleculeCache::new();
        let key = cache_key("build-thing", &vars(&[("target", "x86")]), &[h(b"in")]).unwrap();
        let out = CachedOutput::new(art(0xab));
        cache.store(&key, out.clone()).unwrap();

        match resolve(&f, &cache, &key).unwrap() {
            CacheOutcome::Skip(o) => assert_eq!(o, out),
            CacheOutcome::Run => panic!("expected a cache hit to skip execution"),
        }
    }

    #[test]
    fn cache_miss_runs_then_hits() {
        // First resolve on an empty cache → Run (miss). The caller runs the
        // work and populates the cache. The next resolve with the same key →
        // Skip (hit). This is the core memoization loop.
        let f = deterministic_formula();
        let cache = InMemoryMoleculeCache::new();
        let key = cache_key("build-thing", &vars(&[("target", "x86")]), &[h(b"in")]).unwrap();

        assert_eq!(resolve(&f, &cache, &key).unwrap(), CacheOutcome::Run);

        // Simulate running the work and populating the cache.
        let out = CachedOutput::new(art(0x11));
        cache.store(&key, out.clone()).unwrap();

        match resolve(&f, &cache, &key).unwrap() {
            CacheOutcome::Skip(o) => assert_eq!(o, out),
            CacheOutcome::Run => panic!("expected a hit after populating the cache"),
        }
    }

    #[test]
    fn agentic_never_cached() {
        // An agentic formula always Runs, even when the cache contains an
        // entry under its key. The cache must not be consulted, and a hit must
        // never short-circuit an LLM session.
        let f = agentic_formula();
        let cache = InMemoryMoleculeCache::new();
        let key = cache_key("research-thing", &vars(&[("q", "why")]), &[h(b"ctx")]).unwrap();
        cache.store(&key, CachedOutput::new(art(0x22))).unwrap();

        assert_eq!(
            resolve(&f, &cache, &key).unwrap(),
            CacheOutcome::Run,
            "agentic formula must never be content-skipped"
        );
    }

    #[test]
    fn hash_changes_on_var_change() {
        // Changing a resolved variable changes the cache key, so the cached
        // artifact for the old vars is not reused for the new vars.
        let k1 = cache_key("f", &vars(&[("target", "x86")]), &[h(b"in")]).unwrap();
        let k2 = cache_key("f", &vars(&[("target", "arm")]), &[h(b"in")]).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn hash_changes_on_input_artifact_change() {
        // Changing an upstream input artifact hash changes the key.
        let k1 = cache_key("f", &vars(&[("t", "x")]), &[h(b"in-a")]).unwrap();
        let k2 = cache_key("f", &vars(&[("t", "x")]), &[h(b"in-b")]).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn hash_changes_on_formula_id_change() {
        // The formula id participates in the key.
        let k1 = cache_key("alpha", &vars(&[("t", "x")]), &[h(b"in")]).unwrap();
        let k2 = cache_key("beta", &vars(&[("t", "x")]), &[h(b"in")]).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn input_artifact_order_does_not_affect_key() {
        // Input artifact hashes are sorted before hashing, so presentation
        // order is irrelevant — the same set yields the same key.
        let fwd = cache_key("f", &vars(&[("t", "x")]), &[h(b"a"), h(b"b"), h(b"c")]).unwrap();
        let rev = cache_key("f", &vars(&[("t", "x")]), &[h(b"c"), h(b"b"), h(b"a")]).unwrap();
        assert_eq!(fwd, rev);
    }

    proptest::proptest! {
        /// Hash-stability property: the same inputs (any var ordering, any
        /// input-hash ordering) always produce the same key. This is the
        /// determinism contract the content cache rests on.
        #[test]
        fn cache_key_is_stable_under_reordering(
            mut pairs in proptest::collection::vec(("[a-z]{1,6}", "[a-z0-9]{0,8}"), 0..6),
            seeds in proptest::collection::vec(proptest::collection::vec(proptest::num::u8::ANY, 0..8), 0..6),
        ) {
            // Dedup variable names so the BTreeMap is unambiguous.
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs.dedup_by(|a, b| a.0 == b.0);

            let var_map: BTreeMap<String, String> =
                pairs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let hashes: Vec<Hash> = seeds.iter().map(|s| Hash::of_bytes(s)).collect();

            let k1 = cache_key("f", &var_map, &hashes).unwrap();

            // Reverse the input-hash presentation order; the key must not move.
            let mut rev = hashes.clone();
            rev.reverse();
            let k2 = cache_key("f", &var_map, &rev).unwrap();

            proptest::prop_assert_eq!(k1, k2);

            // Re-deriving from the same canonical inputs is byte-identical.
            let k3 = cache_key("f", &var_map, &hashes).unwrap();
            proptest::prop_assert_eq!(k1, k3);
        }
    }
}
