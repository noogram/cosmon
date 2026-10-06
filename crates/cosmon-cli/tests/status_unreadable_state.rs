// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #178 — one unparseable `state.json` must not abort `cs status`.
//!
//! The listing used to fail on the first molecule whose `state.json` did not
//! parse, hiding the whole fleet. The property: the valid molecules are
//! listed, the damaged one is named (path and reason), and the exit status
//! says something was skipped.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};

const BAD: &str = "task-20260101-badd";

fn pending_molecule(id: &str) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status: MoleculeStatus::Pending,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
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

fn seed(state_dir: &Path) {
    let store = FileStore::new(state_dir);
    for id in ["task-20260101-aaaa", "task-20260101-bbbb"] {
        let m = pending_molecule(id);
        store.save_molecule(&m.id, &m).expect("save molecule");
    }
    let dir = state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(BAD);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("state.json"), "{ not json").expect("write bad state");
}

fn run_status(tmp: &Path, state_dir: &Path, json: bool) -> std::process::Output {
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
    cmd.output().expect("run cs status")
}

/// Two valid molecules and one damaged `state.json`: the two are counted,
/// the third is named with its path and a reason, and the exit status is
/// non-zero.
#[test]
fn unparseable_state_is_named_and_skipped_not_fatal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    seed(&state_dir);

    let out = run_status(tmp.path(), &state_dir, true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "status must print JSON despite the damaged file ({e}); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });

    assert_eq!(
        value["molecules"]["alive"], 2,
        "both valid molecules listed"
    );
    assert_eq!(value["unreadable"][0]["id"], BAD);
    assert!(
        value["unreadable"][0]["path"]
            .as_str()
            .is_some_and(|p| p.ends_with(&format!("{BAD}/state.json"))),
        "names the path: {value}"
    );
    assert!(
        value["unreadable"][0]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "gives a reason: {value}"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "exit status says something was skipped"
    );

    let text = run_status(tmp.path(), &state_dir, false);
    let text_out = String::from_utf8_lossy(&text.stdout);
    assert!(text_out.contains(BAD), "text view names it: {text_out}");
    assert_eq!(text.status.code(), Some(3));
}

/// A fleet without damage keeps exiting zero.
#[test]
fn healthy_fleet_exits_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let store = FileStore::new(&state_dir);
    let m = pending_molecule("task-20260101-aaaa");
    store.save_molecule(&m.id, &m).expect("save molecule");

    let out = run_status(tmp.path(), &state_dir, true);
    assert!(out.status.success());
}
