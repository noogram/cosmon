// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #95 — un-harvested work must never be silent.
//!
//! A molecule that finished (`Completed`) but was never merged by `cs done`
//! nor dropped by `cs collapse` exists only on `feat/<id>` until one of those
//! runs. Before this test, `cs status` had nothing to say about it: the
//! molecule was invisible in the compact one-liner, and the `--json` output
//! carried no field a script could poll. `cs peek --phase harvestable`
//! already named this set (`Completed` and un-archived); `cs status` must
//! report the same set, unprompted, because a pulse is read far more often
//! than a deliberate `cs peek --phase harvestable` query.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore};

fn completed(id: &str, archived: bool) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status: MoleculeStatus::Completed,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 2,
        current_step: 2,
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
        archived,
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

fn seed(state_dir: &Path, molecules: &[MoleculeData]) {
    let store = FileStore::new(state_dir);
    store.save_fleet(&Fleet::default()).expect("save fleet");
    for mol in molecules {
        store.save_molecule(&mol.id, mol).expect("save molecule");
    }
}

fn status_json(tmp: &Path, state_dir: &Path) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--json",
            "--config",
            state_dir.to_str().expect("utf-8 state dir"),
            "status",
        ])
        .output()
        .expect("run cs status");
    assert!(
        out.status.success(),
        "cs status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cs status --json emits JSON")
}

fn status_text(tmp: &Path, state_dir: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--config",
            state_dir.to_str().expect("utf-8 state dir"),
            "status",
        ])
        .output()
        .expect("run cs status");
    assert!(
        out.status.success(),
        "cs status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A `Completed`, un-archived molecule is un-harvested work: `--json` must
/// name it, and an archived one (already harvested by `cs done`) must not.
#[test]
fn harvestable_json_counts_completed_unarchived_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed(
        &state_dir,
        &[
            completed("task-20260101-aaaa", false),
            completed("task-20260101-bbbb", true),
        ],
    );

    let out = status_json(tmp.path(), &state_dir);
    assert_eq!(
        out["harvestable"]["count"], 1,
        "only the un-archived one counts"
    );
    assert_eq!(out["harvestable"]["ids"][0], "task-20260101-aaaa");
}

/// The compact pulse — what an operator actually reads — must name the
/// un-harvested molecule and point at both terminal verbs, not just one:
/// `cs done` keeps the work, `cs collapse` drops it (issue #95 Expected §1).
#[test]
fn compact_status_surfaces_the_harvest_queue_and_both_verbs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed(&state_dir, &[completed("task-20260101-cccc", false)]);

    let text = status_text(tmp.path(), &state_dir);
    assert!(
        text.contains("task-20260101-cccc"),
        "the un-harvested molecule id must be named, not just counted: {text}"
    );
    assert!(
        text.contains("cs done"),
        "must point at cs done to keep the work: {text}"
    );
    assert!(
        text.contains("cs collapse"),
        "must point at cs collapse to drop it: {text}"
    );
}

/// An empty harvest queue must not print anything — an always-on line is
/// how a real one stops being read (the same discipline `render_unmerged_token`
/// already applies).
#[test]
fn empty_harvest_queue_is_silent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed(&state_dir, &[completed("task-20260101-dddd", true)]);

    let out = status_json(tmp.path(), &state_dir);
    assert_eq!(out["harvestable"]["count"], 0);

    let text = status_text(tmp.path(), &state_dir);
    assert!(
        !text.to_lowercase().contains("harvest"),
        "nothing to harvest must not print a harvest line: {text}"
    );
}
