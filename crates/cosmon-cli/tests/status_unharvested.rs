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
use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, ProjectId, WorkerId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::worker::WorkerStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};

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

/// The old rendering capped itself at three ids. The queue is an action list,
/// so every item must carry its own branch, recorded checkout, and command.
#[test]
fn status_lists_every_item_and_honors_a_worktree_override() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
    let state_dir = tmp.path().join("state");
    let ids = [
        "task-20260101-1001",
        "task-20260101-1002",
        "task-20260101-1003",
        "task-20260101-1004",
    ];
    let mut molecules: Vec<_> = ids.iter().map(|id| completed(id, false)).collect();
    let worker_id = WorkerId::new("status-location-worker").expect("valid worker id");
    molecules[3].assigned_worker = Some(worker_id.clone());
    molecules[3].originating_branch = Some("feat/recorded-location".to_owned());

    let mut fleet = Fleet::default();
    let worker = WorkerData::new(
        worker_id.clone(),
        AgentId::new("tackle").expect("valid agent id"),
        AgentRole::Implementation,
        Clearance::Write,
        WorkerStatus::Active,
    )
    .with_repo("overrides/status-location")
    .with_molecule(molecules[3].id.clone());
    fleet.workers.insert(worker_id, worker);
    let store = FileStore::new(&state_dir);
    store.save_fleet(&fleet).expect("save fleet");
    for molecule in &molecules {
        store
            .save_molecule(&molecule.id, molecule)
            .expect("save molecule");
    }

    let value = status_json(tmp.path(), &state_dir);
    assert_eq!(value["harvestable"]["count"], 4);
    let items = value["harvestable"]["items"]
        .as_array()
        .expect("items array");
    assert_eq!(items.len(), 4, "JSON must not cap the action list");
    let overridden = items
        .iter()
        .find(|item| item["molecule"] == ids[3])
        .expect("overridden item");
    assert_eq!(overridden["branch"], "feat/recorded-location");
    assert_eq!(
        overridden["worktree"],
        root.join("overrides/status-location").display().to_string()
    );
    assert_eq!(overridden["harvest_command"], format!("cs done {}", ids[3]));

    let text = status_text(tmp.path(), &state_dir);
    for id in ids {
        assert!(
            text.contains(&format!("cs done {id}")),
            "missing {id}: {text}"
        );
    }
    assert!(text.contains("feat/recorded-location"), "{text}");
    assert!(
        text.contains(&root.join("overrides/status-location").display().to_string()),
        "{text}"
    );
}

/// A pending dependent must name an absent blocker and the gesture that
/// repairs its edge in both operator and machine status.
#[test]
fn status_surfaces_missing_blocker_with_repair_guidance() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let mut dependent = completed("task-20260101-dddd", false);
    dependent.status = MoleculeStatus::Pending;
    dependent.typed_links.push(MoleculeLink::BlockedBy {
        source: MoleculeId::new("task-20260101-aaaa").expect("valid blocker id"),
    });
    seed(&state_dir, &[dependent]);

    let value = status_json(tmp.path(), &state_dir);
    assert_eq!(
        value["missing_blockers"][0]["dependent"],
        "task-20260101-dddd"
    );
    assert_eq!(
        value["missing_blockers"][0]["blocker"],
        "task-20260101-aaaa"
    );
    let text = status_text(tmp.path(), &state_dir);
    assert!(text.contains("task-20260101-dddd"), "{text}");
    assert!(text.contains("task-20260101-aaaa"), "{text}");
    assert!(text.contains("missing molecule"), "{text}");
    assert!(text.contains("cs collapse task-20260101-dddd"), "{text}");
    assert!(text.contains("re-nucleate"), "{text}");
}

/// A referenced pre-project-id predecessor belongs to this dependency
/// closure, while an unrelated foreign-project record does not.
#[test]
fn ensemble_resolves_referenced_legacy_blocker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::write(
        state_dir.join("config.toml"),
        "[project]\nproject_id = \"current-aaaa\"\n",
    )
    .expect("config");
    let mut legacy = completed("task-20260101-aaaa", true);
    legacy.merged_at = Some(Utc::now());
    let mut dependent = completed("task-20260101-bbbb", false);
    dependent.status = MoleculeStatus::Pending;
    dependent.project_id = Some(ProjectId::new("current-aaaa").expect("project id"));
    dependent.typed_links.push(MoleculeLink::BlockedBy {
        source: legacy.id.clone(),
    });
    let mut foreign = completed("task-20260101-cccc", true);
    foreign.project_id = Some(ProjectId::new("foreign-bbbb").expect("project id"));
    let mut outsider_dependent = completed("task-20260101-dddd", false);
    outsider_dependent.status = MoleculeStatus::Pending;
    outsider_dependent.project_id = dependent.project_id.clone();
    outsider_dependent
        .typed_links
        .push(MoleculeLink::BlockedBy {
            source: foreign.id.clone(),
        });
    seed(
        &state_dir,
        &[legacy, dependent, foreign, outsider_dependent],
    );

    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp.path())
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--json",
            "--config",
            state_dir.to_str().expect("state path"),
            "ensemble",
        ])
        .output()
        .expect("run ensemble");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("ensemble JSON");
    let ids: Vec<&str> = value["molecule_states"]
        .as_array()
        .expect("states")
        .iter()
        .filter_map(|state| state["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "task-20260101-aaaa",
            "task-20260101-bbbb",
            "task-20260101-dddd"
        ]
    );
    use cosmon_runtime::{Decision, EnsembleSnapshot, ReadyFrontierScheduler, ResidentScheduler};
    let snapshot = EnsembleSnapshot::from_json(&String::from_utf8_lossy(&out.stdout))
        .expect("resident snapshot");
    let decisions = ReadyFrontierScheduler::default().next_decisions(&snapshot);
    assert!(decisions.iter().any(|decision| matches!(decision, Decision::Tackle { molecule_id, .. } if molecule_id == "task-20260101-bbbb")), "legacy merged blocker releases its dependent: {decisions:?}");
    assert!(!decisions.iter().any(|decision| matches!(decision, Decision::Tackle { molecule_id, .. } if molecule_id == "task-20260101-dddd")), "foreign blocker keeps its dependent held: {decisions:?}");
    let status = status_json(tmp.path(), &state_dir);
    assert_eq!(
        status["missing_blockers"][0]["blocker"],
        "task-20260101-cccc"
    );
    assert_eq!(
        status["missing_blockers"][0]["reason"],
        "outside current project"
    );
    assert_eq!(status["missing_blockers"].as_array().map(Vec::len), Some(1));
}
