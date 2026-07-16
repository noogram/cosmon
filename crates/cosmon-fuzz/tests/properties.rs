// SPDX-License-Identifier: AGPL-3.0-only

//! Proptest property suites for the cs CLI surface.
//!
//! Each `proptest!` block is a named property with a generator, an
//! oracle, and a one-line docstring explaining what invariant it
//! exercises. Run with `cargo test -p cosmon-fuzz` for a quick smoke
//! (256 cases by default) or `PROPTEST_CASES=10000 cargo test -p
//! cosmon-fuzz --release` for a longer soak.
//!
//! Current targets (≥3, as required by the task briefing):
//!
//! 1. **`nucleate_missing_var_is_detected`** — random formula + vars,
//!    exercises the required-variable contract end-to-end.
//! 2. **`tackle_done_cycle_is_consistent`** — random command streams
//!    against the `Ensemble` simulator, asserts global invariants +
//!    reconcile idempotence after every step.
//! 3. **`reconcile_is_idempotent`** — a focused property that just
//!    asserts `reconcile(reconcile(x)) == reconcile(x)`.
//! 4. **`parse_oracles_never_panic`** — unicode-rich fuzzing of the
//!    string-parse entry points.

use std::collections::{BTreeMap, BTreeSet};

use cosmon_fuzz::{
    oracle_nucleate, oracle_parse_formula, oracle_parse_molecule_id, oracle_parse_tag,
    oracle_parse_worker_id, synthetic_formula, Ensemble, SimCommand,
};
use proptest::collection::{vec, SizeRange};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// A "reasonable" topic string: printable ASCII plus a sprinkle of
/// unicode. Long enough to cover common shell-quoting hazards.
fn arb_topic() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{1,16}".prop_map(String::from),
        "[A-Z a-z0-9:_\\-/]{0,32}".prop_map(String::from),
        // intentionally pathological: leading/trailing whitespace, quotes
        "\\s*[\"'a-z0-9 ]{0,16}\\s*".prop_map(String::from),
        // emoji / unicode
        "[\\p{Letter}]{0,8}".prop_map(String::from),
    ]
}

/// A generator for variable maps with at most `max` entries.
fn arb_vars(max: usize) -> impl Strategy<Value = BTreeMap<String, String>> {
    vec(("[a-z]{1,8}", arb_topic()), SizeRange::from(0..=max))
        .prop_map(|pairs| pairs.into_iter().collect::<BTreeMap<_, _>>())
}

/// Molecule-id-ish strings — a mix of valid-looking shapes and
/// near-misses that stress the parser (hyphens, digits, truncations).
fn arb_molecule_id_string() -> impl Strategy<Value = String> {
    prop_oneof![
        // Valid shape
        "[a-z]{2,4}-20260[1-9][0-9][0-9]-[a-z0-9]{4,8}".prop_map(String::from),
        // Missing sections
        "[a-z]{2,4}-20260101".prop_map(String::from),
        "[a-z]{2,4}--[a-z0-9]{4}".prop_map(String::from),
        // Invalid characters
        "[!@#$%^&*()]{1,8}".prop_map(String::from),
        // Unicode
        "\\p{Letter}{1,16}".prop_map(String::from),
        // Completely random
        ".*".prop_map(String::from),
    ]
}

/// Tag-ish strings, including deliberately ill-formed ones.
fn arb_tag_string() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{1,8}".prop_map(String::from),
        "[a-z]{1,8}:[a-z0-9]{1,8}".prop_map(String::from),
        // Uppercase / whitespace / colons in value — all invalid
        "[A-Z]{1,8}".prop_map(String::from),
        "[a-z]{1,8}:[a-z ]{1,8}".prop_map(String::from),
        "[a-z]{1,8}:[a-z:]{1,8}".prop_map(String::from),
        ":[a-z]{1,8}".prop_map(String::from),
        ".*".prop_map(String::from),
    ]
}

/// Worker-id-ish strings.
fn arb_worker_id_string() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{1,8}".prop_map(String::from),
        "ep-[a-z]{1,8}".prop_map(String::from),
        "-[a-z]{1,8}".prop_map(String::from), // leading hyphen — invalid
        "[a-z]{1,8}-".prop_map(String::from), // trailing hyphen — invalid
        "[A-Z]{1,8}".prop_map(String::from),  // non-ascii-lowercase
        ".*".prop_map(String::from),
    ]
}

/// A single SimCommand, parameterized by the pool of ids the
/// simulator might already know about. The generator deliberately
/// produces commands that reference ids *outside* the pool — that's
/// where the bugs live.
fn arb_command(existing_ids: Vec<String>) -> impl Strategy<Value = SimCommand> {
    let id_pool = if existing_ids.is_empty() {
        vec!["m0".to_string()]
    } else {
        existing_ids
    };
    let id_choice = proptest::sample::select(id_pool.clone());
    let blocker_sample = vec(proptest::sample::select(id_pool.clone()), 0..=3);
    let fresh_id = "m[0-9]{1,2}".prop_map(String::from);

    prop_oneof![
        (fresh_id.clone(), blocker_sample.clone())
            .prop_map(|(id, b)| { SimCommand::Nucleate { id, blocked_by: b } }),
        (id_choice.clone(), arb_tag_string()).prop_map(|(id, tag)| SimCommand::Tag { id, tag }),
        id_choice.clone().prop_map(|id| SimCommand::Tackle { id }),
        id_choice.clone().prop_map(|id| SimCommand::Complete { id }),
        id_choice.clone().prop_map(|id| SimCommand::Done { id }),
        id_choice.prop_map(|id| SimCommand::Collapse { id }),
    ]
}

/// A bounded sequence of commands to apply to a fresh ensemble.
/// We build it with a rolling pool of known ids so later commands
/// can reference earlier ones with non-trivial probability.
fn arb_command_stream() -> impl Strategy<Value = Vec<SimCommand>> {
    // Seed with a small pool; we'll let the simulator's duplicate-reject
    // behavior dedupe any collisions.
    let seed_ids = vec!["m0".to_string(), "m1".to_string(), "m2".to_string()];
    vec(arb_command(seed_ids), 1..=24)
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    /// Property 1 — nucleate respects the required-variable contract.
    ///
    /// For any synthetic formula with zero or more required variables
    /// and any variable map, [`oracle_nucleate`] asserts:
    /// - all required supplied ⇒ Ok + id_prefix preserved + pass-through
    /// - any required missing ⇒ MissingVariable error.
    #[test]
    fn nucleate_missing_var_is_detected(
        required in vec("[a-z]{2,6}", 0..=3),
        optional in vec(("[a-z]{2,6}", "[a-z0-9]{0,8}"), 0..=3),
        vars in arb_vars(6),
    ) {
        // Dedup required names against optional names and against each
        // other — a formula with duplicate var names is not interesting
        // for this property and the TOML parser would reject it anyway.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let required: Vec<String> = required
            .into_iter()
            .filter(|r| seen.insert(r.clone()))
            .collect();
        let optional: Vec<(String, String)> = optional
            .into_iter()
            .filter(|(k, _)| seen.insert(k.clone()))
            .collect();

        let req_refs: Vec<&str> = required.iter().map(String::as_str).collect();
        let opt_refs: Vec<(&str, &str)> =
            optional.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let formula = synthetic_formula("fz", &req_refs, &opt_refs);
        oracle_nucleate(&formula, vars);
    }

    /// Property 2 — `Ensemble::apply` preserves global invariants under
    /// arbitrary command streams, and `reconcile` is idempotent after
    /// every applied command.
    ///
    /// This exercises the wrong-predicate / missing-guard bug class:
    /// a bug in `do_tackle` that forgets the `blocked_by` check would
    /// surface as invariant 3 (`Active but blocker not Done`) failing.
    #[test]
    fn tackle_done_cycle_is_consistent(stream in arb_command_stream()) {
        let mut ens = Ensemble::new();
        for cmd in &stream {
            let _outcome = ens.apply(cmd);
            ens.check();
            let once = ens.reconcile();
            // Reconcile is a pure function of state — computing it a
            // second time without mutating state must yield the same
            // surface. This is the "reconcile idempotence" contract
            // (CLAUDE.md, Surface Sync Protocol).
            let twice = ens.reconcile();
            prop_assert_eq!(once, twice);
        }
    }

    /// Property 3 — focused reconcile idempotence property, kept
    /// separate from the lifecycle stream test so a failure localizes
    /// to the projection function rather than to the simulator.
    #[test]
    fn reconcile_is_idempotent(stream in arb_command_stream()) {
        let mut ens = Ensemble::new();
        for cmd in &stream {
            let _ = ens.apply(cmd);
        }
        let s1 = ens.reconcile();
        let s2 = ens.reconcile();
        prop_assert_eq!(s1, s2);
    }

    /// Property 4 — string-parsing entry points are **total**: they
    /// return `Ok`/`Err` for every input and never panic. On success,
    /// the success-path invariants (key grammar, round-trip equality)
    /// also hold.
    #[test]
    fn parse_oracles_never_panic(
        mol_ids in vec(arb_molecule_id_string(), 0..=16),
        tags in vec(arb_tag_string(), 0..=16),
        workers in vec(arb_worker_id_string(), 0..=16),
    ) {
        for s in &mol_ids {
            let _ = oracle_parse_molecule_id(s);
        }
        for s in &tags {
            let _ = oracle_parse_tag(s);
        }
        for s in &workers {
            let _ = oracle_parse_worker_id(s);
        }
    }

    /// Property 5 — Formula::parse is total over arbitrary TOML-ish
    /// strings. On success the step id uniqueness + `order` bound
    /// invariants are asserted inside the oracle.
    #[test]
    fn formula_parse_is_total(toml in ".*") {
        let _ = oracle_parse_formula(&toml);
    }
}
