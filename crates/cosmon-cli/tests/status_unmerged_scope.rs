// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #97 — the "to merge" count must cover molecule branches only.
//!
//! `discover_contributions` (`cs status`) used to run `git branch --no-merged
//! main` unfiltered: any local branch not merged into `main` counted toward
//! `unmerged.branches`, including operator refs such as `backup/*` or spore
//! branches that are never harvested by `cs done` — so the number could never
//! reach zero by harvesting alone. A molecule branch is always `feat/<id>`
//! (see `cs done`/`cs collapse`/`cs stitch`); only those should count.

use std::path::Path;
use std::process::Command;

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
