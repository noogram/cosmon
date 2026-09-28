// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #97 — the "to merge" count must cover molecule branches only.
//!
//! `discover_contributions` (`cs status`) used to run `git branch --no-merged
//! main` unfiltered: any local branch not merged into `main` counted toward
//! `unmerged.branches`, including operator refs such as `backup/*` or spore
//! branches that are never harvested by `cs done` — so the number could never
//! reach zero by harvesting alone. A molecule branch is always `feat/<id>`
//! (see `cs done`/`cs collapse`/`cs stitch`); only those should count.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{Fleet, MoleculeData, StateStore};

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    let mut full: Vec<&str> = vec!["-C", repo.to_str().expect("utf-8 repo path")];
    full.extend_from_slice(args);
    Command::new("git")
        .args(&full)
        .output()
        .expect("git command failed to spawn")
}

fn init_repo(repo: &Path) {
    assert!(git(repo, &["init", "-q", "-b", "main"]).status.success());
    assert!(git(repo, &["config", "user.email", "test@example.com"])
        .status
        .success());
    assert!(git(repo, &["config", "user.name", "Test"]).status.success());
    assert!(git(repo, &["config", "commit.gpgsign", "false"])
        .status
        .success());
    std::fs::write(repo.join("README.md"), "init\n").unwrap();
    assert!(git(repo, &["add", "."]).status.success());
    assert!(git(repo, &["commit", "-q", "-m", "init"]).status.success());
}

fn branch_ahead(repo: &Path, name: &str, file: &str) {
    assert!(git(repo, &["switch", "-q", "-c", name, "main"])
        .status
        .success());
    std::fs::write(repo.join(file), "work\n").unwrap();
    assert!(git(repo, &["add", "."]).status.success());
    assert!(
        git(repo, &["commit", "-q", "-m", &format!("work on {name}")])
            .status
            .success()
    );
    assert!(git(repo, &["switch", "-q", "main"]).status.success());
}

fn status_json(repo: &Path, state_dir: &Path) -> serde_json::Value {
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

fn status_text(repo: &Path, state_dir: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_cs"))
        .current_dir(repo)
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
    String::from_utf8(out.stdout).expect("status text is utf-8")
}

fn molecule(id: &str, status: MoleculeStatus, reason: Option<&str>) -> MoleculeData {
    MoleculeData {
        harvest_reason: None,
        id: MoleculeId::new(id).expect("valid fixture id"),
        fleet_id: FleetId::new("default").expect("valid fleet id"),
        formula_id: FormulaId::new("task-work").expect("valid formula id"),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 2,
        current_step: usize::from(status.is_terminal()),
        completed_steps: Vec::new(),
        collapse_reason: reason.map(str::to_owned),
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
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: Some(format!("feat/{id}")),
        base_branch: Some("main".to_owned()),
        protected_paths: Vec::new(),
        pending_step: None,
        merged_at: None,
        non_integration: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: status == MoleculeStatus::Collapsed,
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
    for molecule in molecules {
        store
            .save_molecule(&molecule.id, molecule)
            .expect("save molecule");
    }
}

/// A non-molecule ref (`backup/*`) unmerged into `main` must not inflate the
/// "to merge" count: only `feat/<molecule-id>` branches are molecule work.
#[test]
fn unmerged_count_excludes_non_molecule_branches() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    branch_ahead(&repo, "backup/before-migration", "backup.txt");
    branch_ahead(&repo, "spore/math-attack", "spore.txt");

    let state_dir = repo.join(".cosmon-state");
    let out = status_json(&repo, &state_dir);
    assert_eq!(
        out["unmerged"]["branches"], 0,
        "operator refs are not molecule work: {out}"
    );
}

/// A `feat/<id>` branch is molecule work and must be counted.
#[test]
fn unmerged_count_includes_molecule_branches() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    branch_ahead(&repo, "feat/task-20260101-aaaa", "work.txt");
    branch_ahead(&repo, "backup/before-migration", "backup.txt");

    let state_dir = repo.join(".cosmon-state");
    let out = status_json(&repo, &state_dir);
    assert_eq!(
        out["unmerged"]["branches"], 1,
        "only the molecule branch counts: {out}"
    );
}

/// The clean predicate is about unresolved residue, not every branch. The
/// human and JSON projections must distinguish active, retained-audited, and
/// actionable branches using the existing molecule record.
#[test]
fn status_names_each_branch_disposition_and_counts_only_unresolved_residue() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);

    let active = "task-20260101-a111";
    let retained = "task-20260101-b222";
    let unresolved = "task-20260101-c333";
    branch_ahead(&repo, &format!("feat/{active}"), "active.txt");
    branch_ahead(&repo, &format!("feat/{retained}"), "retained.txt");
    branch_ahead(&repo, &format!("feat/{unresolved}"), "unresolved.txt");

    let state_dir = repo.join(".cosmon-state");
    seed(
        &state_dir,
        &[
            molecule(active, MoleculeStatus::Running, None),
            molecule(
                retained,
                MoleculeStatus::Collapsed,
                Some("superseded after branch review"),
            ),
            molecule(unresolved, MoleculeStatus::Completed, None),
        ],
    );

    let json = status_json(&repo, &state_dir);
    assert_eq!(json["unmerged"]["branches"], 1, "{json}");
    assert_eq!(json["hygiene"]["active_branches"], 1, "{json}");
    assert_eq!(json["hygiene"]["retained_audited_branches"], 1, "{json}");
    assert_eq!(json["hygiene"]["unmerged_branches"], 1, "{json}");
    assert_eq!(json["contributions"][0]["disposition"], "active", "{json}");
    assert_eq!(
        json["contributions"][1]["disposition"], "retained-audited",
        "{json}"
    );
    assert_eq!(
        json["contributions"][2]["disposition"], "unresolved",
        "{json}"
    );

    let text = status_text(&repo, &state_dir);
    assert!(
        text.contains(&format!("feat/{active} — active (running) · not residue")),
        "{text}"
    );
    assert!(
        text.contains("audit disposition: collapsed — superseded after branch review"),
        "{text}"
    );
    assert!(
        text.contains(&format!("run `cs done {unresolved}` after resolving it")),
        "{text}"
    );
}
