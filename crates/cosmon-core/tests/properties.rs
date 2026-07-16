// SPDX-License-Identifier: AGPL-3.0-only

//! Property-based invariants for the cosmon-core lifecycle and ID types.
//!
//! This suite pins down the typestate machine and newtype validation so
//! refactors cannot silently violate:
//!
//! * serde round-trips are identity for `Molecule<Active>`'s status and for
//!   every validated ID newtype;
//! * `evolve` on an n-step molecule reaches `Completed` in exactly n steps
//!   and never before;
//! * `collapse` is terminal — once collapsed, a molecule exposes only its
//!   reason and collapsed step, with no path back to `Active`;
//! * `freeze` / `thaw` round-trip preserves step index and completed steps;
//! * `MoleculeId::new` accepts all and only strings of the form
//!   `PREFIX-YYYYMMDD-XXXX` with a plausible date.

use cosmon_core::id::{AgentId, FormulaId, IdError, MoleculeId, StepId, WorkerId};
use cosmon_core::molecule::{Active, EvolveOutcome, Molecule, MoleculeStatus};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn formula_id() -> FormulaId {
    FormulaId::new("mol-polecat-work").unwrap()
}

fn mol_id() -> MoleculeId {
    MoleculeId::new("test-20260401-abcd").unwrap()
}

fn step(n: usize) -> StepId {
    StepId::new(format!("step-{n}")).unwrap()
}

/// Nucleate + tackle — the Pending→Active path that the typestate
/// lift made mandatory for every property test that wants to evolve.
fn tackled(total: usize) -> Molecule<Active> {
    Molecule::new(mol_id(), formula_id(), total).tackle(WorkerId::new("onyx").unwrap())
}

fn arb_identifier() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,31}".prop_map(|s| s.clone())
}

fn arb_molecule_id_string() -> impl Strategy<Value = String> {
    (
        "[a-z0-9]{1,8}",
        2000u32..=2100,
        1u32..=12,
        1u32..=28,
        0u32..=0xffff,
    )
        .prop_map(|(prefix, y, m, d, suffix)| format!("{prefix}-{y:04}{m:02}{d:02}-{suffix:04x}"))
}

// ---------------------------------------------------------------------------
// Property 1 — evolve(n) terminates exactly at step n
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_evolve_completes_in_exactly_n_steps(total in 1usize..16) {
        let mut active = Some(tackled(total));
        for i in 0..total {
            let m = active.take().unwrap();
            prop_assert_eq!(m.current_step(), i);
            prop_assert_eq!(m.status(), MoleculeStatus::Running);
            match m.evolve(step(i)) {
                EvolveOutcome::Active(m2) => {
                    prop_assert!(i + 1 < total, "active but past last step");
                    active = Some(m2);
                }
                EvolveOutcome::Completed(done) => {
                    prop_assert_eq!(i + 1, total, "completed early");
                    prop_assert_eq!(done.status(), MoleculeStatus::Completed);
                    prop_assert_eq!(done.completed_steps().len(), total);
                    active = None;
                }
            }
        }
        prop_assert!(active.is_none(), "should have completed");
    }
}

// ---------------------------------------------------------------------------
// Property 2 — collapse is terminal (no thaw, no evolve)
// ---------------------------------------------------------------------------
//
// Compile-time guarantee via typestate: `Molecule<Collapsed>` has no
// `thaw` or `evolve` methods. This property asserts the *observable*
// part — reason and step are preserved verbatim regardless of when
// collapse occurred.

proptest! {
    #[test]
    fn prop_collapse_preserves_reason_and_step(
        total in 1usize..8,
        collapse_at in 0usize..8,
        reason in "[a-zA-Z0-9 ]{1,64}",
    ) {
        let collapse_at = collapse_at.min(total - 1);
        let mut mol = tackled(total);
        for i in 0..collapse_at {
            mol = match mol.evolve(step(i)) {
                EvolveOutcome::Active(m) => m,
                EvolveOutcome::Completed(_) => unreachable!(),
            };
        }
        let collapsed = mol.collapse(reason.clone());
        prop_assert_eq!(collapsed.status(), MoleculeStatus::Collapsed);
        prop_assert_eq!(collapsed.collapse_reason(), reason.as_str());
        prop_assert_eq!(collapsed.collapsed_step(), collapse_at);
        prop_assert_eq!(collapsed.completed_steps().len(), collapse_at);
    }
}

// ---------------------------------------------------------------------------
// Property 3 — freeze / thaw preserves step position and completed steps
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_freeze_thaw_roundtrip(total in 1usize..8, freeze_at in 0usize..8) {
        let freeze_at = freeze_at.min(total - 1);
        let mut mol = tackled(total);
        for i in 0..freeze_at {
            mol = match mol.evolve(step(i)) {
                EvolveOutcome::Active(m) => m,
                EvolveOutcome::Completed(_) => unreachable!(),
            };
        }
        let before_step = mol.current_step();
        let before_completed = mol.completed_steps().len();
        let frozen = mol.freeze();
        prop_assert_eq!(frozen.status(), MoleculeStatus::Frozen);
        let thawed = frozen.thaw();
        prop_assert_eq!(thawed.status(), MoleculeStatus::Running);
        prop_assert_eq!(thawed.current_step(), before_step);
        prop_assert_eq!(thawed.completed_steps().len(), before_completed);
    }
}

// ---------------------------------------------------------------------------
// Property 4 — MoleculeStatus serde roundtrip
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_molecule_status_serde_roundtrip(
        status in prop_oneof![
            Just(MoleculeStatus::Running),
            Just(MoleculeStatus::Frozen),
            Just(MoleculeStatus::Completed),
            Just(MoleculeStatus::Collapsed),
        ],
    ) {
        let json = serde_json::to_string(&status).unwrap();
        let back: MoleculeStatus = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, status);
        let display = status.to_string();
        let parsed: MoleculeStatus = display.parse().unwrap();
        prop_assert_eq!(parsed, status);
    }
}

// ---------------------------------------------------------------------------
// Property 5 — Simple ID newtypes: non-empty accepted, empty rejected
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_simple_ids_accept_nonempty(name in arb_identifier()) {
        prop_assume!(!name.is_empty());
        prop_assert!(AgentId::new(name.clone()).is_ok());
        prop_assert!(FormulaId::new(name.clone()).is_ok());
        prop_assert!(StepId::new(name).is_ok());
    }

    #[test]
    fn prop_simple_ids_reject_empty(_seed in any::<u8>()) {
        let agent_empty = matches!(AgentId::new(""), Err(IdError::Empty { .. }));
        let formula_empty = matches!(FormulaId::new(""), Err(IdError::Empty { .. }));
        let step_empty = matches!(StepId::new(""), Err(IdError::Empty { .. }));
        prop_assert!(agent_empty);
        prop_assert!(formula_empty);
        prop_assert!(step_empty);
    }
}

// ---------------------------------------------------------------------------
// Property 6 — MoleculeId parses well-formed strings and round-trips as_str
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_molecule_id_roundtrip(s in arb_molecule_id_string()) {
        let id = MoleculeId::new(s.clone()).unwrap();
        prop_assert_eq!(id.as_str(), s.as_str());
        // Re-parsing the canonical form must succeed.
        let reparsed = MoleculeId::new(id.as_str()).unwrap();
        prop_assert_eq!(&reparsed, &id);
        // Component accessors must match the input structure.
        let parts: Vec<&str> = s.splitn(3, '-').collect();
        prop_assert_eq!(id.prefix(), parts[0]);
        prop_assert_eq!(id.date(), parts[1]);
        prop_assert_eq!(id.suffix(), parts[2]);
    }
}

// ---------------------------------------------------------------------------
// Property 7 — MoleculeId rejects malformed strings (missing hyphens, bad date)
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_molecule_id_rejects_garbage(garbage in "[a-z]{0,10}") {
        // No hyphens → invalid.
        prop_assert!(MoleculeId::new(garbage.clone()).is_err());
    }

    #[test]
    fn prop_molecule_id_rejects_bad_date(
        prefix in "[a-z]{1,4}",
        bogus_date in "[a-z]{1,8}",
        suffix in "[0-9a-f]{4}",
    ) {
        let s = format!("{prefix}-{bogus_date}-{suffix}");
        prop_assert!(MoleculeId::new(s).is_err());
    }
}

// ---------------------------------------------------------------------------
// Property 8 — WorkerId ep-prefix discrimination
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_worker_id_ep_prefix(name in "[a-z][a-z0-9]{0,15}", prefixed in any::<bool>()) {
        let raw = if prefixed { format!("ep-{name}") } else { name.clone() };
        let w = WorkerId::new(raw.clone()).unwrap();
        prop_assert_eq!(w.as_str(), raw.as_str());
        prop_assert_eq!(w.has_ensemble_prefix(), prefixed);
        prop_assert_eq!(w.name(), name.as_str());
    }
}

// ---------------------------------------------------------------------------
// Property 9 — Serde roundtrip on MoleculeId is stable
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_molecule_id_serde_roundtrip(s in arb_molecule_id_string()) {
        let id = MoleculeId::new(s).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        let back: MoleculeId = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, id);
    }
}

// ---------------------------------------------------------------------------
// Property 10 — current_step monotone non-decreasing under evolve
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_current_step_monotone(total in 2usize..10) {
        let mut mol = tackled(total);
        let mut prev = mol.current_step();
        for i in 0..total - 1 {
            mol = match mol.evolve(step(i)) {
                EvolveOutcome::Active(m) => m,
                EvolveOutcome::Completed(_) => unreachable!(),
            };
            prop_assert!(mol.current_step() > prev);
            prev = mol.current_step();
        }
    }
}
