// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #172 — `cs status` must not call a dead worker alive or the galaxy
//! clean.
//!
//! A `Running` molecule whose recorded tmux session no longer exists is a
//! dead-pane ghost: `cs ensemble` already reports it (`EFFECTIVE dead`,
//! `⚠ DEAD`). `cs status` counted it among the alive molecules and printed
//! `clean`, so the one command read to decide "is anything wrong?" said no.
//! These tests drive the real binary against a fixture whose session cannot
//! exist and assert the property the issue asks for.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::worker::WorkerStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};

const MOL: &str = "task-20260101-dead";
const WORKER: &str = "worker-whose-host-vanished";

fn running_molecule(id: &str, worker: &WorkerId) -> MoleculeData {
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
        originating_branch: None,
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

fn seed_dead_worker(state_dir: &Path) {
    let worker_id = WorkerId::new(WORKER).expect("valid worker id");
    let molecule = running_molecule(MOL, &worker_id);
    let mut fleet = Fleet::default();
    fleet.workers.insert(
        worker_id.clone(),
        WorkerData::new(
            worker_id,
            AgentId::new("tackle").expect("valid agent id"),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        )
        .with_molecule(molecule.id.clone()),
    );
    let store = FileStore::new(state_dir);
    store.save_fleet(&fleet).expect("save fleet");
    store
        .save_molecule(&molecule.id, &molecule)
        .expect("save molecule");
}

fn run_status(tmp: &Path, state_dir: &Path, json: bool) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.current_dir(tmp)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    if json {
        cmd.arg("--json");
    }
    cmd.args([
        "--config",
        state_dir.to_str().expect("utf-8 state dir"),
        "status",
    ]);
    let out = cmd.output().expect("run cs status");
    assert!(
        out.status.success(),
        "cs status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The property of the issue: a running molecule without a live session is
/// counted as dead, is not counted as healthy alive work, and makes the
/// galaxy not clean.
#[test]
fn running_molecule_without_a_session_is_dead_not_alive_and_not_clean() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed_dead_worker(&state_dir);

    let value: serde_json::Value =
        serde_json::from_str(&run_status(tmp.path(), &state_dir, true)).expect("status JSON");

    assert_eq!(value["hygiene"]["clean"], false, "a dead worker is residue");
    assert_eq!(value["hygiene"]["dead_workers"], 1);
    assert_eq!(value["molecules"]["dead_workers"], 1);
    assert_eq!(value["molecules"]["dead"][0]["molecule"], MOL);
    // `alive` keeps its historical meaning (every non-terminal molecule);
    // the healthy count is the additive key.
    assert_eq!(value["molecules"]["alive"], 1);
    assert_eq!(value["molecules"]["alive_healthy"], 0);
}

/// The headline names the dead worker and its reclaim gesture, and does not
/// print `clean`.
#[test]
fn compact_headline_names_the_dead_worker_and_its_reclaim_gesture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed_dead_worker(&state_dir);

    let text = run_status(tmp.path(), &state_dir, false);

    assert!(text.contains("dead"), "headline must say dead: {text}");
    assert!(text.contains(WORKER), "names the worker: {text}");
    assert!(
        text.contains(&format!("cs purge {WORKER}")),
        "names the reclaim gesture: {text}"
    );
    assert!(!text.contains("clean"), "must not print clean: {text}");
    assert!(!text.contains("0 dead"), "{text}");
}
