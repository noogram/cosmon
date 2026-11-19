// SPDX-License-Identifier: AGPL-3.0-only

//! The action label of `cs done` on a branch with nothing left to merge
//! (issue #213).
//!
//! A branch whose commits reached the base by fast-forward is zero commits
//! ahead of base, like a branch that never had a commit. Only the second is an
//! `empty_branch`; the first is landed work and is labelled `already_merged`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cs_isolated(repo: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_BASE_BRANCH")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .current_dir(repo.join(".cosmon/state"));
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Output {
    git_command(repo)
        .args(args)
        .output()
        .expect("git spawn failed")
}

fn git_command(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL");
    cmd
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn setup_repo(repo: &Path) {
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "Test"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(
        cosmon.join("config.toml"),
        "[project]\nproject_id = \"test-ff-label\"\n",
    )
    .unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(repo.join(".gitignore"), ".cosmon/\n.worktrees/\n").unwrap();
    fs::write(repo.join("base.txt"), "base\n").unwrap();
    git_ok(repo, &["add", ".gitignore", "base.txt"]);
    git_ok(repo, &["commit", "-q", "-m", "base"]);
}

fn completed_molecule(repo: &Path) -> String {
    let out = cs_isolated(repo)
        .args(["--json", "nucleate", "task-work", "--var", "topic=label"])
        .output()
        .expect("cs nucleate");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();
    let done = cs_isolated(repo)
        .args([
            "complete",
            &mol_id,
            "--reason",
            "test",
            "--ignore-mindguard",
        ])
        .output()
        .expect("cs complete");
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    mol_id
}

fn actions_of_done(repo: &Path, mol_id: &str) -> Vec<String> {
    let out = cs_isolated(repo)
        .args(["--json", "done", mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    v["actions"]
        .as_array()
        .expect("actions array")
        .iter()
        .filter_map(|a| a.as_str().map(str::to_owned))
        .collect()
}

/// Fork, add two commits, fast-forward the base to the branch: landed work.
#[test]
fn fast_forwarded_branch_is_labelled_already_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    let branch = format!("feat/{mol_id}");
    git_ok(repo, &["checkout", "-q", "-b", &branch, "main"]);
    for f in ["one.txt", "two.txt"] {
        fs::write(repo.join(f), "x\n").unwrap();
        git_ok(repo, &["add", f]);
        git_ok(repo, &["commit", "-qm", f]);
    }
    git_ok(repo, &["checkout", "-q", "main"]);
    git_ok(repo, &["merge", "-q", "--ff-only", &branch]);

    let actions = actions_of_done(repo, &mol_id);
    assert!(
        actions.iter().any(|a| a == "already_merged"),
        "landed work must be already_merged: {actions:?}"
    );
    assert!(
        !actions.iter().any(|a| a == "empty_branch"),
        "landed work is not an empty branch: {actions:?}"
    );
}

/// A branch that never had a commit of its own is still an `empty_branch`.
#[test]
fn a_branch_that_never_had_a_commit_is_still_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    git_ok(repo, &["branch", &format!("feat/{mol_id}"), "main"]);

    let actions = actions_of_done(repo, &mol_id);
    assert!(
        actions.iter().any(|a| a == "empty_branch"),
        "a never-committed branch is empty: {actions:?}"
    );
}

/// With the branch's reflog gone (expired, pruned, or never written) the
/// creation point is unknown, so `cs done` does not claim `empty_branch`, even
/// though the branch has no commit of its own. It reports `already_merged`,
/// the label that promises nothing about emptiness.
#[test]
fn a_branch_whose_reflog_is_gone_is_never_reported_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    let branch = format!("feat/{mol_id}");
    git_ok(repo, &["branch", &branch, "main"]);
    let log = repo.join(".git/logs/refs/heads").join(&branch);
    assert!(log.exists(), "fixture assumes git wrote a branch reflog");
    fs::remove_file(log).unwrap();

    let actions = actions_of_done(repo, &mol_id);
    assert!(
        !actions.iter().any(|a| a == "empty_branch"),
        "an unknown creation point must not be reported as empty: {actions:?}"
    );
    assert!(
        actions.iter().any(|a| a == "already_merged"),
        "the fallback label is already_merged: {actions:?}"
    );
}

#[test]
fn a_reflog_without_its_creation_entry_does_not_prove_emptiness_or_the_landing_range() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    let branch = format!("feat/{mol_id}");
    let dated_git_ok = |date: &str, args: &[&str]| {
        let out = git_command(repo)
            .env("GIT_COMMITTER_DATE", date)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
    };
    dated_git_ok("2000-01-01T00:00:00Z", &["checkout", "-qb", &branch]);
    fs::write(repo.join("work.txt"), "landed\n").unwrap();
    git_ok(repo, &["add", "work.txt"]);
    dated_git_ok("2020-01-01T00:00:00Z", &["commit", "-qm", "landed work"]);
    git_ok(repo, &["checkout", "-q", "main"]);
    git_ok(repo, &["merge", "-q", "--ff-only", &branch]);
    git_ok(
        repo,
        &["reflog", "expire", "--expire=2010-01-01T00:00:00Z", &branch],
    );
    let log = git(repo, &["reflog", "show", "--format=%gs", &branch]);
    assert!(log.status.success(), "{:?}", log.stderr);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout),
        "commit: landed work\n"
    );

    let out = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let actions = v["actions"].as_array().unwrap();
    assert!(actions.iter().any(|a| a == "already_merged"), "{actions:?}");
    assert!(!actions.iter().any(|a| a == "empty_branch"), "{actions:?}");
    let warnings = v["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .filter_map(|w| w.as_str())
            .any(|w| w.contains("could not establish the branch's fork point")),
        "{warnings:?}"
    );
}
