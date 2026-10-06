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
    dependent
        .tags
        .insert(cosmon_core::tag::Tag::new("selected").expect("valid tag"));
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

    let tagged = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(tmp.path())
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--json",
            "--config",
            state_dir.to_str().expect("state path"),
            "ensemble",
            "--tag",
            "selected",
        ])
        .output()
        .expect("run tagged ensemble");
    assert!(
        tagged.status.success(),
        "{}",
        String::from_utf8_lossy(&tagged.stderr)
    );
    let tagged_value: serde_json::Value =
        serde_json::from_slice(&tagged.stdout).expect("tagged ensemble JSON");
    let tagged_ids: Vec<&str> = tagged_value["molecule_states"]
        .as_array()
        .expect("tagged states")
        .iter()
        .filter_map(|state| state["id"].as_str())
        .collect();
    assert_eq!(tagged_ids, vec!["task-20260101-bbbb"]);
}

fn git_in(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `cs status --json`, run from inside `repo` so branch discovery sees it.
fn status_json_in_repo(repo: &Path, state_dir: &Path) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(repo)
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

/// Issue #174 — a `Completed` molecule whose `feat/<id>` branch was merged
/// into `main` and then deleted has been harvested, even when no harvest
/// record (`archived`, `merged_at`) was written. It must not be listed as
/// harvestable. Two controls keep the rule narrow: a completed molecule with
/// no trace on `main`, and one whose branch still exists unmerged, stay in
/// the queue because their work may not be on the trunk.
#[test]
fn merged_and_deleted_branch_is_not_harvestable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["config", "user.email", "test@example.com"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "root"]);

    // Merged then deleted.
    git_in(&repo, &["checkout", "-q", "-b", "feat/task-20260101-e001"]);
    git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "work"]);
    git_in(&repo, &["checkout", "-q", "main"]);
    git_in(
        &repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge branch 'feat/task-20260101-e001'",
            "feat/task-20260101-e001",
        ],
    );
    git_in(&repo, &["branch", "-q", "-d", "feat/task-20260101-e001"]);

    // Control: branch still exists, unmerged.
    git_in(&repo, &["checkout", "-q", "-b", "feat/task-20260101-e002"]);
    git_in(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "unmerged work"],
    );
    git_in(&repo, &["checkout", "-q", "main"]);

    let state_dir = tmp.path().join("state");
    seed(
        &state_dir,
        &[
            completed("task-20260101-e001", false),
            completed("task-20260101-e002", false),
            // Control: no branch, no merge on main.
            completed("task-20260101-e003", false),
        ],
    );

    let out = status_json_in_repo(&repo, &state_dir);
    let ids: Vec<&str> = out["harvestable"]["ids"]
        .as_array()
        .expect("ids array")
        .iter()
        .filter_map(|id| id.as_str())
        .collect();
    assert!(
        !ids.contains(&"task-20260101-e001"),
        "merged-and-deleted must not be harvestable: {ids:?}"
    );
    assert!(ids.contains(&"task-20260101-e002"), "{ids:?}");
    assert!(ids.contains(&"task-20260101-e003"), "{ids:?}");
    assert_eq!(out["harvestable"]["count"], 2);
}

/// A recorded merge (`merged_at`) is a harvest record in its own right: the
/// legacy-state case from #153, where `archived` was never written.
#[test]
fn merged_at_molecule_is_not_harvestable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let mut merged = completed("task-20260101-f001", false);
    merged.merged_at = Some(Utc::now());
    seed(
        &state_dir,
        &[merged, completed("task-20260101-f002", false)],
    );

    let out = status_json(tmp.path(), &state_dir);
    assert_eq!(
        out["harvestable"]["ids"],
        serde_json::json!(["task-20260101-f002"])
    );
}

/// A repo whose `main` merged and deleted the branches of the ids in
/// `merged`, and still holds an unmerged `feat/<id>` for each of `unmerged`.
fn repo_with_merged_branches(tmp: &Path, merged: &[&str], unmerged: &[&str]) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["config", "user.email", "test@example.com"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "root"]);
    for id in merged {
        let branch = format!("feat/{id}");
        git_in(&repo, &["checkout", "-q", "-b", &branch]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "work"]);
        git_in(&repo, &["checkout", "-q", "main"]);
        git_in(
            &repo,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                &format!("Merge branch '{branch}'"),
                &branch,
            ],
        );
        git_in(&repo, &["branch", "-q", "-d", &branch]);
    }
    for id in unmerged {
        git_in(&repo, &["checkout", "-q", "-b", &format!("feat/{id}")]);
        git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "unmerged"]);
        git_in(&repo, &["checkout", "-q", "main"]);
    }
    repo
}

/// A completed molecule in the project `cs peek` is scoped to.
fn completed_in_project(id: &str) -> MoleculeData {
    let mut m = completed(id, false);
    m.project_id = Some(ProjectId::new("test-174").expect("valid project id"));
    m
}

/// `cs peek --json --phase harvestable`, as an id list.
fn peek_harvestable_ids(repo: &Path, state_dir: &Path) -> Vec<String> {
    std::fs::write(
        state_dir.join("config.toml"),
        "[project]\nproject_id = \"test-174\"\n",
    )
    .expect("write config");
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(repo)
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .args([
            "--json",
            "--config",
            state_dir.to_str().expect("utf-8 state dir"),
            "peek",
            "--phase",
            "harvestable",
        ])
        .output()
        .expect("run cs peek");
    assert!(
        out.status.success(),
        "cs peek failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("peek JSON");
    let mut ids: Vec<String> = value["molecules"]
        .as_array()
        .expect("molecules array")
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids
}

fn status_ids(out: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = out["harvestable"]["ids"]
        .as_array()
        .expect("ids array")
        .iter()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids
}

fn harvest_command_of(out: &serde_json::Value, id: &str) -> String {
    out["harvestable"]["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|item| item["molecule"] == id)
        .and_then(|item| item["harvest_command"].as_str())
        .unwrap_or_else(|| panic!("{id} is not listed: {out}"))
        .to_owned()
}

/// Issue #174, merged work with nothing left to tear down is harvested: neither
/// `cs status` nor `cs peek --phase harvestable` lists it.
#[test]
fn merged_without_residue_is_listed_by_neither_status_nor_peek() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_with_merged_branches(tmp.path(), &["task-20260101-a001"], &[]);
    let state_dir = tmp.path().join("state");
    let mut merged_at = completed_in_project("task-20260101-a002");
    merged_at.merged_at = Some(Utc::now());
    // Control: unmerged, so the queue is not vacuously empty.
    seed(
        &state_dir,
        &[
            completed_in_project("task-20260101-a001"),
            merged_at,
            completed_in_project("task-20260101-a003"),
        ],
    );

    assert_eq!(
        status_ids(&status_json_in_repo(&repo, &state_dir)),
        ["task-20260101-a003"]
    );
    assert_eq!(
        peek_harvestable_ids(&repo, &state_dir),
        ["task-20260101-a003"]
    );
}

/// Issue #174, hiding residue is worse than the original bug: merged work that
/// still has a worktree on disk, or a worker on the roster, stays listed and
/// names `cs done <id> --no-merge`, the gesture that tears it down without
/// merging a second time. Both surfaces list it.
#[test]
fn merged_with_residue_stays_listed_and_names_the_no_merge_gesture() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo =
        repo_with_merged_branches(tmp.path(), &["task-20260101-b001"], &["task-20260101-b004"]);
    let state_dir = tmp.path().join("state");

    // b001: merged and branch gone, worktree directory left behind.
    std::fs::create_dir_all(repo.join(".worktrees/task-20260101-b001")).expect("worktree dir");

    // b002: `merged_at` recorded, worker still on the roster.
    let worker_id = WorkerId::new("b002-worker").expect("valid worker id");
    let mut on_roster = completed_in_project("task-20260101-b002");
    on_roster.merged_at = Some(Utc::now());
    on_roster.assigned_worker = Some(worker_id.clone());
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
        .with_molecule(on_roster.id.clone()),
    );

    // b003: merged, no residue. b004: not merged.
    let store = FileStore::new(&state_dir);
    store.save_fleet(&fleet).expect("save fleet");
    for m in [
        completed_in_project("task-20260101-b001"),
        on_roster,
        completed_in_project("task-20260101-b003"),
        completed_in_project("task-20260101-b004"),
    ] {
        store.save_molecule(&m.id, &m).expect("save molecule");
    }
    // b003 is merged but only through `merged_at`.
    let mut b003 = completed_in_project("task-20260101-b003");
    b003.merged_at = Some(Utc::now());
    store.save_molecule(&b003.id, &b003).expect("save b003");

    let out = status_json_in_repo(&repo, &state_dir);
    assert_eq!(
        status_ids(&out),
        [
            "task-20260101-b001",
            "task-20260101-b002",
            "task-20260101-b004"
        ],
        "b003 has no residue and drops out; the rest stay: {out}"
    );
    assert_eq!(
        harvest_command_of(&out, "task-20260101-b001"),
        "cs done task-20260101-b001 --no-merge"
    );
    assert_eq!(
        harvest_command_of(&out, "task-20260101-b002"),
        "cs done task-20260101-b002 --no-merge"
    );
    assert_eq!(
        harvest_command_of(&out, "task-20260101-b004"),
        "cs done task-20260101-b004",
        "work that has not landed is still merged by plain `cs done`"
    );
    assert_eq!(peek_harvestable_ids(&repo, &state_dir), status_ids(&out));
}

/// Unmerged work is listed as before, with the merging command.
#[test]
fn unmerged_completed_work_is_listed_as_before() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = repo_with_merged_branches(tmp.path(), &[], &["task-20260101-c001"]);
    let state_dir = tmp.path().join("state");
    seed(
        &state_dir,
        &[
            completed_in_project("task-20260101-c001"),
            completed_in_project("task-20260101-c002"),
        ],
    );
    let out = status_json_in_repo(&repo, &state_dir);
    assert_eq!(
        status_ids(&out),
        ["task-20260101-c001", "task-20260101-c002"]
    );
    assert_eq!(
        harvest_command_of(&out, "task-20260101-c001"),
        "cs done task-20260101-c001"
    );
    assert_eq!(peek_harvestable_ids(&repo, &state_dir), status_ids(&out));
}
