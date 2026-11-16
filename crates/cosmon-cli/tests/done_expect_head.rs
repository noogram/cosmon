// SPDX-License-Identifier: AGPL-3.0-only

//! `cs done --expect-head <oid>` pins what lands to what was gated (issue #213).
//!
//! An operator who gates a batch of merges outside cosmon and then closes each
//! molecule with `cs done` needs `cs done` to refuse a branch that moved after
//! the gate. These tests run the real `cs` binary and pin the contract on both
//! paths: the branch is merged by `cs done`, and the branch was already merged
//! by hand.

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
        "[project]\nproject_id = \"test-expect-head\"\n",
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

/// Nucleate a `task-work` molecule and complete it, so `cs done` runs on a
/// `Completed` molecule without `--force`.
fn completed_molecule(repo: &Path) -> String {
    let out = cs_isolated(repo)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=pin the head",
        ])
        .output()
        .expect("cs nucleate");
    assert!(
        out.status.success(),
        "nucleate failed: {}",
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
        "complete failed: {}",
        String::from_utf8_lossy(&done.stderr)
    );
    mol_id
}

/// Commit one file on `feat/<mol>` and return to `main`; returns the new tip.
fn commit_on_branch(repo: &Path, mol_id: &str, file: &str) -> String {
    let branch = format!("feat/{mol_id}");
    let exists = git(repo, &["rev-parse", "--verify", &branch])
        .status
        .success();
    if exists {
        git_ok(repo, &["checkout", "-q", &branch]);
    } else {
        git_ok(repo, &["checkout", "-q", "-b", &branch, "main"]);
    }
    fs::write(repo.join(file), format!("{file}\n")).unwrap();
    git_ok(repo, &["add", file]);
    git_ok(repo, &["commit", "-qm", &format!("feat: {file}")]);
    let tip = rev(repo, "HEAD");
    git_ok(repo, &["checkout", "-q", "main"]);
    tip
}

fn state(repo: &Path, mol_id: &str) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(
            repo.join(".cosmon/state/fleets/default/molecules")
                .join(mol_id)
                .join("state.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

fn done(repo: &Path, mol_id: &str, extra: &[&str]) -> Output {
    cs_isolated(repo)
        .args(["done", mol_id, "--no-auto-propel"])
        .args(extra)
        .output()
        .expect("cs done")
}

fn is_ancestor(repo: &Path, commit: &str, of: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", commit, of])
        .status
        .success()
}

/// The issue's case: the head was pinned at H1, a commit H2 was added after
/// the gate. `cs done` refuses, names both ids, and lands and stamps nothing.
#[test]
fn a_branch_that_moved_after_the_pin_is_refused_on_the_merge_path() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    let h1 = commit_on_branch(repo, &mol_id, "gated.txt");
    let h2 = commit_on_branch(repo, &mol_id, "after-the-gate.txt");
    let main_before = rev(repo, "main");

    let refused = done(repo, &mol_id, &["--expect-head", &h1]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "a moved head must be refused");
    assert!(
        stderr.contains(&h1) && stderr.contains(&h2),
        "the refusal must name the pinned and the actual head: {stderr}"
    );
    assert_eq!(main_before, rev(repo, "main"), "main must not move");
    assert!(
        !is_ancestor(repo, &h2, "main"),
        "the ungated commit must not reach the base"
    );
    let after = state(repo, &mol_id);
    assert_eq!(after["status"], "completed");
    assert!(
        after["merged_at"].is_null(),
        "merged_at must not be stamped: {after}"
    );
    assert_ne!(
        after["archived"], true,
        "the molecule must stay un-archived"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", &format!("feat/{mol_id}")])
            .status
            .success(),
        "the branch must survive the refusal"
    );
}

/// The right head — full or abbreviated — merges exactly the gated commit.
#[test]
fn the_pinned_head_merges_on_the_merge_path() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    let h1 = commit_on_branch(repo, &mol_id, "gated.txt");

    let merged = done(repo, &mol_id, &["--expect-head", &h1[..10]]);
    assert!(
        merged.status.success(),
        "the pinned head must merge: {}",
        String::from_utf8_lossy(&merged.stderr)
    );
    assert!(is_ancestor(repo, &h1, "main"), "the gated commit landed");
    assert!(!state(repo, &mol_id)["merged_at"].is_null());
}

/// Already-merged path: the branch was merged by hand. A wrong pin refuses
/// without stamping `merged_at`; the right pin closes the molecule.
#[test]
fn the_pin_is_checked_on_the_already_merged_path() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    let h1 = commit_on_branch(repo, &mol_id, "gated.txt");
    let base = rev(repo, "main");
    git_ok(
        repo,
        &[
            "merge",
            "--no-ff",
            "-q",
            "-m",
            "train",
            &format!("feat/{mol_id}"),
        ],
    );
    assert!(is_ancestor(repo, &h1, "main"));

    let refused = done(repo, &mol_id, &["--expect-head", &base]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a head other than the branch tip must be refused even when already merged"
    );
    assert!(stderr.contains(&base) && stderr.contains(&h1), "{stderr}");
    let after = state(repo, &mol_id);
    assert_eq!(after["status"], "completed");
    assert!(
        after["merged_at"].is_null(),
        "merged_at must not be stamped on refusal: {after}"
    );
    assert_ne!(after["archived"], true);

    let closed = done(repo, &mol_id, &["--expect-head", &h1]);
    assert!(
        closed.status.success(),
        "the right head must close the already-merged molecule: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(!state(repo, &mol_id)["merged_at"].is_null());
}

/// An id git cannot resolve is a refusal, not a silently ignored pin.
#[test]
fn an_unresolvable_pin_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    let mol_id = completed_molecule(repo);
    commit_on_branch(repo, &mol_id, "gated.txt");
    let main_before = rev(repo, "main");

    let refused = done(repo, &mol_id, &["--expect-head", "0123456789abcdef"]);
    assert!(!refused.status.success());
    assert_eq!(main_before, rev(repo, "main"));
    assert!(state(repo, &mol_id)["merged_at"].is_null());
}
