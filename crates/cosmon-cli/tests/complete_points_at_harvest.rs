// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #95 — when a worker finishes, cosmon must say where its work is and
//! how to bring it back. `cs complete` transitions a molecule to `Completed`
//! without merging it: the commits stay on `feat/<id>` until `cs done` or
//! `cs collapse` runs. Before this test, the success message named only the
//! new status, never the branch or the harvest command.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore};

fn running(id: &str) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status: MoleculeStatus::Running,
        variables: HashMap::new(),
        assigned_worker: None,
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

fn seed(state_dir: &Path, molecule: &MoleculeData) {
    let store = FileStore::new(state_dir);
    store.save_fleet(&Fleet::default()).expect("save fleet");
    store
        .save_molecule(&molecule.id, molecule)
        .expect("save molecule");
}

#[test]
fn complete_names_the_branch_and_the_harvest_command() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let mol_id = "task-20260101-eeee";
    seed(&state_dir, &running(mol_id));

    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp.path())
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("COSMON_STATE_DIR", &state_dir)
        .args(["complete", mol_id, "--ignore-mindguard"])
        .output()
        .expect("run cs complete");
    assert!(
        out.status.success(),
        "cs complete failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("feat/{mol_id}")),
        "must name the branch the work is on: {stdout}"
    );
    assert!(
        stdout.contains(&format!("cs done {mol_id}")),
        "must name the harvest command: {stdout}"
    );
    assert!(
        stdout.contains("cs collapse"),
        "must also name the drop-it alternative: {stdout}"
    );
}
