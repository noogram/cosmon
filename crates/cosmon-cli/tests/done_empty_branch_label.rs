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
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .output()
        .expect("git spawn failed")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev(repo: &Path, refname: &str) -> String {
    String::from_utf8_lossy(&git(repo, &["rev-parse", refname]).stdout)
        .trim()
        .to_owned()
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
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();
    let done = cs_isolated(repo)
        .args(["complete", &mol_id, "--reason", "test", "--ignore-mindguard"])
        .output()
        .expect("cs complete");
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    mol_id
}

fn actions_of_done(repo: &Path, mol_id: &str) -> Vec<String> {
    let out = cs_isolated(repo)
        .args(["--json", "done", mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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
