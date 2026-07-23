// SPDX-License-Identifier: AGPL-3.0-only

//! Property tests for the phantom-worker-free invariant
//! (the fold-in of `Worker` state into `Molecule`).
//!
//! Before this fold-in the trio
//! (`MoleculeData::assigned_worker` + `MoleculeData::session_name` +
//! `WorkerData::current_molecule`) was three writers for one fact:
//! "is a worker bound to this molecule, and where". Disagreement
//! between the three was the phantom-worker class.
//!
//! The fold-in introduces [`MoleculeData::process`] as the inline
//! single-source-of-truth and provides [`MoleculeData::bind_process`]
//! / [`MoleculeData::release_process`] as the only writer paths. These
//! tests pin the structural invariants of that contract:
//!
//! * `bind_process(p)` mirrors `p.worker_id` and `p.tmux_session`
//!   onto the legacy fields, so a legacy reader sees the same answer
//!   as a new reader (no silent regression during the migration
//!   window).
//! * `release_process()` clears both the inline slot *and* the
//!   `session_name` legacy mirror, so a phantom session string cannot
//!   outlive the worker.
//! * `bind_process(p) → release_process()` is idempotent on the
//!   inline slot: the post-state has `process == None` regardless of
//!   what the pre-state was.
//! * Serde roundtrip preserves the inline `process` slot across
//!   load/save, including absence (`None` does not promote to
//!   `Some(default)` after a roundtrip).
//! * Legacy state files (no `process` field on disk) deserialize to
//!   `process == None` and the helpers
//!   ([`MoleculeData::worker`], [`MoleculeData::tmux_session`]) fall
//!   back to the legacy fields.

use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use proptest::prelude::*;

use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::process::MoleculeProcess;
use cosmon_state::MoleculeData;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Construct a minimal pending `MoleculeData` for property tests.
fn pending_mol(id: &str) -> MoleculeData {
    MoleculeData {
        id: MoleculeId::new(id).unwrap(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: MoleculeStatus::Pending,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 1,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: Vec::new(),
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        pending_step: None,
        merged_at: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
        adapter: None,
    }
}

fn arb_worker_id() -> impl Strategy<Value = WorkerId> {
    // WorkerId rejects names that start or end with a hyphen; constrain
    // the strategy so every value is parser-valid.
    "[a-z][a-z0-9]{0,15}".prop_map(|s| WorkerId::new(&s).unwrap())
}

fn arb_session_name() -> impl Strategy<Value = String> {
    "[a-z0-9-]{1,32}"
}

fn arb_mol_id() -> impl Strategy<Value = String> {
    // The MoleculeId parser validates the date component (`MMDD` after
    // the year), so generate only a small set of known-valid dates.
    let valid_month_day = prop_oneof![
        Just("0101"),
        Just("0228"),
        Just("0331"),
        Just("0426"),
        Just("0731"),
        Just("1031"),
        Just("1231"),
    ];
    (valid_month_day, "[0-9a-f]{4}").prop_map(|(md, suffix)| format!("task-2026{md}-{suffix}"))
}

// ---------------------------------------------------------------------------
// Hand-rolled tests (regressions on the writer contract)
// ---------------------------------------------------------------------------

#[test]
fn bind_process_mirrors_legacy_fields() {
    let mut mol = pending_mol("task-20260426-aaaa");
    let wid = WorkerId::new("worker-mirror").unwrap();
    let proc = MoleculeProcess::new(wid.clone(), "session-mirror");
    mol.bind_process(proc.clone());

    assert_eq!(mol.process, Some(proc.clone()));
    assert_eq!(mol.assigned_worker, Some(wid.clone()));
    assert_eq!(mol.session_name.as_deref(), Some("session-mirror"));
    assert_eq!(mol.worker(), Some(&wid));
    assert_eq!(mol.tmux_session(), Some("session-mirror"));
    assert!(mol.has_live_process());
}

#[test]
fn release_process_clears_inline_slot_and_session_mirror() {
    let mut mol = pending_mol("task-20260426-bbbb");
    let wid = WorkerId::new("w").unwrap();
    mol.bind_process(MoleculeProcess::new(wid.clone(), "s"));

    mol.release_process();

    // Inline slot cleared.
    assert!(mol.process.is_none());
    // Legacy session_name mirror cleared too — phantom session would
    // otherwise outlive the worker.
    assert!(mol.session_name.is_none());
    // assigned_worker preserved as a historical trace of who completed.
    assert_eq!(mol.assigned_worker, Some(wid));
    assert!(!mol.has_live_process());
}

#[test]
fn release_process_is_idempotent() {
    let mut mol = pending_mol("task-20260426-cccc");
    mol.bind_process(MoleculeProcess::new(WorkerId::new("w").unwrap(), "s"));
    mol.release_process();
    let after_first = mol.clone();
    mol.release_process();
    assert_eq!(mol.process, after_first.process);
    assert_eq!(mol.session_name, after_first.session_name);
}

#[test]
fn legacy_load_with_no_process_field_deserializes_to_none() {
    // Serialised form predating the fold-in: no `process` key.
    let legacy_json = serde_json::json!({
        "id": "task-20260426-dddd",
        "fleet_id": "default",
        "formula_id": "task-work",
        "status": "pending",
        "variables": {},
        "assigned_worker": "legacy-worker",
        "created_at": "2026-04-25T10:00:00Z",
        "updated_at": "2026-04-25T10:00:00Z",
        "total_steps": 1,
        "current_step": 0,
        "completed_steps": [],
        "collapse_reason": null,
        "collapsed_step": null,
        "links": [],
        "session_name": "legacy-session",
    });
    let mol: MoleculeData = serde_json::from_value(legacy_json).unwrap();
    assert!(
        mol.process.is_none(),
        "legacy load must yield process: None"
    );
    // Helpers fall back to the legacy fields.
    assert_eq!(mol.worker().unwrap().as_str(), "legacy-worker");
    assert_eq!(mol.tmux_session(), Some("legacy-session"));
}

#[test]
fn process_field_skipped_in_serialization_when_none() {
    let mol = pending_mol("task-20260426-eeee");
    let json = serde_json::to_string(&mol).unwrap();
    assert!(
        !json.contains("\"process\""),
        "process: None must be skipped in serialization (legacy reader compat); got: {json}"
    );
}

// ---------------------------------------------------------------------------
// Property tests — the phantom-free invariant under random inputs
// ---------------------------------------------------------------------------

proptest! {
    /// After `bind_process`, the inline slot agrees with the legacy
    /// mirrors on both fields exposed by the helpers.
    #[test]
    fn prop_bind_process_keeps_helpers_in_sync(
        mol_id in arb_mol_id(),
        worker_id in arb_worker_id(),
        session in arb_session_name(),
    ) {
        let mut mol = pending_mol(&mol_id);
        let proc = MoleculeProcess::new(worker_id.clone(), session.clone());
        mol.bind_process(proc);

        // Inline truth.
        let inline = mol.process.as_ref().unwrap();
        prop_assert_eq!(&inline.worker_id, &worker_id);
        prop_assert_eq!(&inline.tmux_session, &session);

        // Helpers must surface the inline values, not the legacy mirrors,
        // when the slot is present.
        prop_assert_eq!(mol.worker(), Some(&worker_id));
        prop_assert_eq!(mol.tmux_session(), Some(session.as_str()));

        // Legacy mirrors agree with the inline truth.
        prop_assert_eq!(mol.assigned_worker.as_ref(), Some(&worker_id));
        prop_assert_eq!(mol.session_name.as_deref(), Some(session.as_str()));
    }

    /// `bind_process(p) → release_process()` always lands in a state
    /// that reports `has_live_process() == false`.
    #[test]
    fn prop_release_process_eliminates_phantom_pointer(
        mol_id in arb_mol_id(),
        worker_id in arb_worker_id(),
        session in arb_session_name(),
    ) {
        let mut mol = pending_mol(&mol_id);
        mol.bind_process(MoleculeProcess::new(worker_id, session));
        prop_assert!(mol.has_live_process());

        mol.release_process();

        // The structural invariant: after release there is no live-process
        // record on the molecule, and the session_name mirror is also
        // cleared so a stale string cannot be mistaken for a live binding.
        prop_assert!(!mol.has_live_process());
        prop_assert!(mol.process.is_none());
        prop_assert!(mol.session_name.is_none());
        prop_assert_eq!(mol.tmux_session(), None);
    }

    /// JSON roundtrip preserves the inline `process` slot identically,
    /// regardless of whether it is `Some` or `None` before the trip.
    #[test]
    fn prop_serde_roundtrip_preserves_process_slot(
        mol_id in arb_mol_id(),
        bind in any::<bool>(),
        worker_id in arb_worker_id(),
        session in arb_session_name(),
    ) {
        let mut mol = pending_mol(&mol_id);
        if bind {
            mol.bind_process(MoleculeProcess::new(worker_id, session));
        }
        let json = serde_json::to_string(&mol).unwrap();
        let back: MoleculeData = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back.process.clone(), mol.process.clone());
        prop_assert_eq!(back.has_live_process(), mol.has_live_process());
    }
}
