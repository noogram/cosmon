// SPDX-License-Identifier: AGPL-3.0-only

//! Convoy-cascade regression — ADR-048 integration test.
//!
//! Reproduces the 2026-04-17 *Opération Executor* incident as a minimal
//! fixture: five pending molecules older than 48 h, none tagged with a
//! `temp:*` key. The guard must refuse runtime bootstrap with the typed
//! [`BacklogGuardError::DirtyBacklog`] error (mapping to exit code 12 at
//! the CLI layer). Passing `force = true` must instead return the report
//! so the caller can emit the audit event and proceed.
//!
//! This test freezes the *behavior* of the guard — the exit-code mapping
//! is verified in `cosmon_cli::cmd::guard` unit tests — so that a future
//! refactor of the predicate cannot silently reintroduce the pathology.

use std::collections::{BTreeSet, HashMap};

use chrono::{Duration, Utc};
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_runtime::{check_backlog, BacklogGuardError, DEFAULT_STALE_THRESHOLD};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

fn sediment_mol(id: &str, age_hours: i64, tags: &[&str]) -> MoleculeData {
    let mut t = BTreeSet::new();
    for raw in tags {
        t.insert(Tag::new((*raw).to_owned()).unwrap());
    }
    let now = Utc::now();
    MoleculeData {
        id: MoleculeId::new(id).unwrap(),
        fleet_id: FleetId::new("default").unwrap(),
        formula_id: FormulaId::new("task-work").unwrap(),
        status: MoleculeStatus::Pending,
        variables: HashMap::default(),
        assigned_worker: None,
        created_at: now - Duration::hours(age_hours + 1),
        updated_at: now - Duration::hours(age_hours),
        total_steps: 2,
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
        tags: t,
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
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    }
}

#[test]
fn convoy_cascade_fixture_triggers_refusal() {
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());
    store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

    for i in 0..DEFAULT_STALE_THRESHOLD {
        let m = sediment_mol(&format!("task-20260414-{i:04x}"), 96, &[]);
        store.save_molecule(&m.id, &m).unwrap();
    }

    let err = check_backlog(&store, false).expect_err("dirty backlog must refuse");
    match err {
        BacklogGuardError::DirtyBacklog(report) => {
            assert_eq!(report.count, DEFAULT_STALE_THRESHOLD);
            assert_eq!(report.threshold, DEFAULT_STALE_THRESHOLD);
            assert!(report.is_dirty());
            // Sample is capped at 5; with exactly 5 sediment mols the
            // sample lists every one of them.
            assert_eq!(report.sample.len(), DEFAULT_STALE_THRESHOLD);
        }
        other @ BacklogGuardError::State(_) => panic!("expected DirtyBacklog, got {other}"),
    }
}

#[test]
fn convoy_cascade_fixture_with_force_returns_report() {
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());
    store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

    for i in 0..DEFAULT_STALE_THRESHOLD {
        let m = sediment_mol(&format!("task-20260414-{i:04x}"), 96, &[]);
        store.save_molecule(&m.id, &m).unwrap();
    }

    let report = check_backlog(&store, true).expect("force bypasses refusal");
    assert_eq!(report.count, DEFAULT_STALE_THRESHOLD);
    assert!(
        report.is_dirty(),
        "force must still report dirty so the audit event is emitted"
    );
}

#[test]
fn curated_backlog_never_refuses_even_when_stale() {
    // ADR-048 §2: curated `temp:cold`/`temp:frozen` pendings are
    // *inspected* and parked. They must not count as sediment, even at
    // arbitrary age. This is the anti-overfire invariant.
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());
    store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

    for i in 0..(DEFAULT_STALE_THRESHOLD * 4) {
        let tag = match i % 4 {
            0 => "temp:hot",
            1 => "temp:warm",
            2 => "temp:cold",
            _ => "temp:frozen",
        };
        let m = sediment_mol(&format!("task-20260410-{i:04x}"), 240, &[tag]);
        store.save_molecule(&m.id, &m).unwrap();
    }

    let report = check_backlog(&store, false).expect("curated backlog is clean");
    assert_eq!(report.count, 0);
}

#[test]
fn env_override_tightens_threshold() {
    // Safety-valve: an operator can tighten the threshold to refuse
    // earlier. Use a fresh env-var value to avoid colliding with the
    // process-wide environment (serde::test isolation is on opt-in in
    // this repo; we restore afterwards).
    let tmp = TempDir::new().unwrap();
    let store = FileStore::new(tmp.path());
    store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
    // Seed just one sediment mol.
    let m = sediment_mol("task-20260414-only", 72, &[]);
    store.save_molecule(&m.id, &m).unwrap();

    let prev = std::env::var("COSMON_RUNTIME_GUARD_STALE_THRESHOLD").ok();
    // SAFETY: single-threaded test run; no concurrent env mutation.
    unsafe { std::env::set_var("COSMON_RUNTIME_GUARD_STALE_THRESHOLD", "1") };
    let res = check_backlog(&store, false);
    // Restore env before asserting so a panic doesn't leak state.
    match prev {
        Some(v) => unsafe { std::env::set_var("COSMON_RUNTIME_GUARD_STALE_THRESHOLD", v) },
        None => unsafe { std::env::remove_var("COSMON_RUNTIME_GUARD_STALE_THRESHOLD") },
    }
    match res {
        Err(BacklogGuardError::DirtyBacklog(r)) => assert_eq!(r.count, 1),
        other => panic!("expected DirtyBacklog, got {other:?}"),
    }
}
