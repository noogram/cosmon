// SPDX-License-Identifier: AGPL-3.0-only

//! The integration base is a property of the **molecule**, not of the session.
//!
//! `cs tackle --base <branch>` persists the branch on the molecule; `cs done`
//! reads it back and merges there — with no `COSMON_BASE_BRANCH` exported and
//! without the operator checking out anything by hand first. These tests run
//! the **real `cs` binary** so they pin the wiring, not a helper:
//!
//! * a molecule tackled with `--base release/2.0` harvests onto
//!   `release/2.0`, and `main` does not move;
//! * harvesting it from a `main` checkout is refused (`NotOnBase`) and names
//!   the molecule's own base, not `main`;
//! * a molecule with **no** persisted base still harvests onto `main` —
//!   the backward-compatibility contract;
//! * `--base` naming a branch that does not exist is refused at tackle time,
//!   before any worktree is created.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // The base under test must come from the molecule, never from the
        // developer's shell — strip the ambient override entirely.
        .env_remove("COSMON_BASE_BRANCH");
    cmd
}

/// `cs` pinned to an isolated state dir and run from inside the temp repo, so
/// `find_repo_root()` resolves there.
fn cs_isolated(repo: &Path) -> Command {
    let mut cmd = cs();
    cmd.env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .current_dir(repo.join(".cosmon/state"));
    cmd
}

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    let mut full: Vec<&str> = vec!["-C", repo.to_str().unwrap()];
    full.extend_from_slice(args);
    Command::new("git")
        .args(&full)
        .output()
        .expect("git spawn failed")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev(repo: &Path, refname: &str) -> String {
    String::from_utf8_lossy(&git(repo, &["rev-parse", refname]).stdout)
        .trim()
        .to_owned()
}

/// A git repo with a `.cosmon` project whose state is gitignored, one base
/// commit on `main`, and no `origin` — so the only base `cs done` can resolve
/// is the molecule's own (or the `main` fallback).
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
        "[project]\nproject_id = \"test-molecule-base\"\n",
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

/// Nucleate a `task-work` molecule and return its id (status `Pending`, so
/// `cs tackle` still accepts it).
fn nucleate(repo: &Path, topic: &str) -> String {
    let nuc = cs_isolated(repo)
        .args(["--json", "nucleate", "task-work", "--var"])
        .arg(format!("topic={topic}"))
        .output()
        .expect("cs nucleate");
    assert!(
        nuc.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&nuc.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    v["id"].as_str().expect("nucleate id").to_owned()
}

/// Drive the molecule to a terminal state so `cs done` attempts the merge
/// without `--force`.
fn collapse(repo: &Path, mol_id: &str) {
    let col = cs_isolated(repo)
        .args(["--json", "collapse", mol_id, "--reason", "integration test"])
        .output()
        .expect("cs collapse");
    assert!(
        col.status.success(),
        "collapse failed: {}",
        String::from_utf8_lossy(&col.stderr)
    );
}

/// Stamp the molecule's base the way `cs tackle --base <branch>` does, then
/// read it back to prove the field actually persisted.
///
/// The dispatch itself is deliberately doomed — `--adapter anthropic` with no
/// `ANTHROPIC_API_KEY` fails at the spawn step, *after* the base has been
/// resolved, validated and written. That gives these tests the persistence
/// half of `cs tackle` without a live model, an API key, or a tmux server.
fn tackle_with_base(repo: &Path, mol_id: &str, base: &str) {
    let out = cs_isolated(repo)
        .env_remove("ANTHROPIC_API_KEY")
        .args([
            "tackle",
            mol_id,
            "--base",
            base,
            "--adapter",
            "anthropic",
            "--force",
        ])
        .output()
        .expect("cs tackle");
    assert!(
        !out.status.success(),
        "precondition: the doomed dispatch must fail at spawn, not succeed"
    );

    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(
            repo.join(".cosmon/state/fleets/default/molecules")
                .join(mol_id)
                .join("state.json"),
        )
        .expect("read molecule state"),
    )
    .expect("parse molecule state");
    assert_eq!(
        state["base_branch"].as_str(),
        Some(base),
        "`cs tackle --base {base}` must persist the base on the molecule; \
         state was {state}"
    );
}

/// Give the molecule a branch carrying one commit, cut from `base`.
fn commit_work_on_branch(repo: &Path, mol_id: &str, base: &str) {
    let branch = format!("feat/{mol_id}");
    git_ok(repo, &["checkout", "-q", "-b", &branch, base]);
    fs::write(repo.join("worker.txt"), "worker\n").unwrap();
    git_ok(repo, &["add", "worker.txt"]);
    git_ok(repo, &["commit", "-qm", "worker output"]);
    git_ok(repo, &["checkout", "-q", base]);
}

/// The headline contract: tackled with `--base release/2.0`, harvested onto
/// `release/2.0`, and `main` never moves.
#[test]
fn done_merges_into_the_molecules_own_base_and_leaves_main_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    git_ok(repo, &["branch", "release/2.0"]);

    let mol_id = nucleate(repo, "molecule-owned base branch");
    tackle_with_base(repo, &mol_id, "release/2.0");
    collapse(repo, &mol_id);
    commit_work_on_branch(repo, &mol_id, "release/2.0");

    let main_before = rev(repo, "main");

    // No COSMON_BASE_BRANCH anywhere: the molecule is the only thing that
    // knows where this work belongs.
    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(
        done.status.success(),
        "cs done must merge onto the molecule's base.\nstdout={stdout}\nstderr={stderr}"
    );

    // 1. The work landed on the molecule's base.
    assert!(
        git(repo, &["cat-file", "-e", "release/2.0:worker.txt"])
            .status
            .success(),
        "worker.txt must be on release/2.0 after the harvest"
    );

    // 2. `main` did not move — not by a merge commit, not by anything.
    assert_eq!(
        main_before,
        rev(repo, "main"),
        "main must NOT move when the molecule's base is release/2.0"
    );
    assert!(
        !git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "the worker's file must NOT have leaked onto main"
    );
}

/// The `NotOnBase` guard is preserved and *retargeted*: harvesting from a
/// `main` checkout a molecule based on `release/2.0` is refused, and the
/// message names the molecule's base.
#[test]
fn done_from_the_wrong_checkout_refuses_and_names_the_molecule_base() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);
    git_ok(repo, &["branch", "release/2.0"]);

    let mol_id = nucleate(repo, "not-on-base guard keeps its teeth");
    tackle_with_base(repo, &mol_id, "release/2.0");
    collapse(repo, &mol_id);
    commit_work_on_branch(repo, &mol_id, "release/2.0");

    // Harvest from `main` — the wrong trunk for this molecule.
    git_ok(repo, &["checkout", "-q", "main"]);
    let release_before = rev(repo, "release/2.0");
    let main_before = rev(repo, "main");

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stderr = String::from_utf8_lossy(&done.stderr);

    assert!(
        !done.status.success(),
        "cs done must refuse to merge from a checkout that is not the base"
    );
    assert!(
        stderr.contains("release/2.0"),
        "the refusal must name the molecule's own base, not `main`: {stderr}"
    );
    assert_eq!(main_before, rev(repo, "main"), "main must not move");
    assert_eq!(
        release_before,
        rev(repo, "release/2.0"),
        "the refused merge must not have landed on the base either"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", &format!("feat/{mol_id}")])
            .status
            .success(),
        "the branch is the only copy of the work — it must survive a refusal"
    );
}

/// Backward compatibility: a molecule with no persisted base behaves exactly
/// as it always did — `main`, resolved from the ambient chain.
#[test]
fn molecule_without_a_persisted_base_still_harvests_onto_main() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let mol_id = nucleate(repo, "no persisted base, legacy shape");
    collapse(repo, &mol_id);
    commit_work_on_branch(repo, &mol_id, "main");

    let done = cs_isolated(repo)
        .args(["--json", "done", &mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done");
    let stdout = String::from_utf8_lossy(&done.stdout);
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert!(
        done.status.success(),
        "a base-less molecule must harvest exactly as before.\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        git(repo, &["cat-file", "-e", "main:worker.txt"])
            .status
            .success(),
        "worker.txt must be on main"
    );
}

/// `--base` naming a branch that does not exist is refused up front — before
/// a worktree is created and long before a worker does any work that would
/// have nowhere to land.
#[test]
fn tackle_refuses_a_base_that_does_not_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(repo);

    let mol_id = nucleate(repo, "unknown base is refused");

    let out = cs_isolated(repo)
        .args(["tackle", &mol_id, "--base", "release/nope", "--force"])
        .output()
        .expect("cs tackle");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "tackle must refuse an unknown base");
    assert!(
        stderr.contains("release/nope"),
        "the refusal must name the branch it could not find: {stderr}"
    );
    assert!(
        !repo.join(".worktrees").join(&mol_id).exists(),
        "no worktree may be created for a refused base"
    );
}
