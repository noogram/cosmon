// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #95 — stopping a molecule records abandonment but does not erase or
//! silently relocate its work. The collapse result must name the preserved
//! branch, recorded checkout, recovery command, and the audit-before-delete
//! obligation in text and JSON.

use std::collections::HashMap;
use std::process::Command;

use chrono::Utc;
use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::worker::WorkerStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};

fn running(id: &str, worker: &WorkerId) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status: MoleculeStatus::Running,
        variables: HashMap::new(),
        assigned_worker: Some(worker.clone()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 2,
        current_step: 1,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: Vec::new(),
        protected_paths: Vec::new(),
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: Some("feat/stopped-location".to_owned()),
        base_branch: None,
        pending_step: None,
        merged_at: None,
        non_integration: None,
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

#[test]
fn collapse_preserves_and_reports_the_recorded_location() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
    let state_dir = tmp.path().join("state");
    let mol_id = "task-20260101-c011";
    let worker_id = WorkerId::new("collapse-location-worker").expect("valid worker id");
    let molecule = running(mol_id, &worker_id);
    let mut fleet = Fleet::default();
    let worker = WorkerData::new(
        worker_id.clone(),
        AgentId::new("tackle").expect("valid agent id"),
        AgentRole::Implementation,
        Clearance::Write,
        WorkerStatus::Active,
    )
    .with_repo("overrides/collapse-location")
    .with_molecule(molecule.id.clone());
    fleet.workers.insert(worker_id, worker);
    let store = FileStore::new(&state_dir);
    store.save_fleet(&fleet).expect("save fleet");
    store
        .save_molecule(&molecule.id, &molecule)
        .expect("save molecule");

    let text = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp.path())
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "collapse",
            mol_id,
            "--reason",
            "stopped by operator",
            "--ops-dir",
            state_dir.to_str().expect("utf-8 state dir"),
        ])
        .output()
        .expect("run text cs collapse");
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("feat/stopped-location"), "{stdout}");
    assert!(
        stdout.contains(
            &root
                .join("overrides/collapse-location")
                .display()
                .to_string()
        ),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("cs done {mol_id}")), "{stdout}");
    assert!(
        stdout.contains("audit the branch before deletion"),
        "{stdout}"
    );

    let json = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp.path())
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--json",
            "collapse",
            mol_id,
            "--reason",
            "stopped by operator",
            "--ops-dir",
            state_dir.to_str().expect("utf-8 state dir"),
        ])
        .output()
        .expect("run JSON cs collapse");
    assert!(json.status.success());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON output");
    assert_eq!(value["branch"], "feat/stopped-location");
    assert_eq!(
        value["worktree"],
        root.join("overrides/collapse-location")
            .display()
            .to_string()
    );
    assert_eq!(value["harvest_command"], format!("cs done {mol_id}"));
    assert_eq!(value["branch_deletion_requires_audit"], true);
}
