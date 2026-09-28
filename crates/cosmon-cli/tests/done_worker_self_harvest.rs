// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #109: a worker ran `cs done` on its own molecule from its own pane,
//! and the harvest merged a change to `.cosmon/config.toml` — part of the
//! trusted shell surface — into `main` with nobody reviewing it.
//!
//! Two guards, each pinned here against the real `cs` binary:
//!
//! 1. **Harvest belongs to the pilot.** `cs done <id>` run with the
//!    environment `cs tackle` gives the worker of `<id>` is refused and lands
//!    nothing; the same command from the pilot's environment proceeds. A
//!    worker harvesting a *different* molecule (the resident runtime driving
//!    its DAG) is not the worker harvesting itself and proceeds.
//! 2. **The trusted-surface refusal holds on every gate path.** Before the
//!    fix it fired only when the post-merge gate happened to run a delegated
//!    shell command (`integrity_command` / `build_command`), because only that
//!    rung consulted trust. A tree the cargo rung verifies, or one nobody
//!    declared a gate for, landed a shell-surface change silently.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Opening line of the issue-#74 landing sequence — the refusal that names
/// a merge which changes the trusted shell surface.
const REMEDY_MARKER: &str = "this merge changes the repository's trusted shell surface";

/// Which environment the `cs done` under test runs in.
#[derive(Clone, Copy)]
enum Caller<'a> {
    /// The operator's shell: no worker identity at all.
    Pilot,
    /// The worker `cs tackle` spawned for this molecule id.
    WorkerOf(&'a str),
}

fn apply_env(cmd: &mut Command, repo: &Path, trust_store: &Path) {
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("CB_SESSION_ROLE")
        .env_remove("CB_DEPTH")
        .env_remove("COSMON_SKIP_PRE_DONE_HOOK")
        .env_remove("COSMON_ASSUME_TRUSTED")
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env("COSMON_TRUST_DIR", trust_store)
        .env("GIT_PAGER", "cat");
}

fn cs(repo: &Path, trust_store: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    apply_env(&mut cmd, repo, trust_store);
    cmd.current_dir(repo.join(".cosmon/state"));
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
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

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn rev(repo: &Path, name: &str) -> String {
    String::from_utf8_lossy(&git(repo, &["rev-parse", name]).stdout)
        .trim()
        .to_owned()
}

fn write_file(repo: &Path, path: &str, contents: &str) {
    let full = repo.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, contents).unwrap();
}

/// A repository whose `.cosmon/config.toml` and formulas are tracked, so a
/// molecule branch can change the shell surface through an ordinary merge.
fn setup_repo(repo: &Path, config: &str, files: &[(&str, &str)]) {
    git_ok(repo, &["init", "-q", "-b", "main"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "Test"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
    let cosmon = repo.join(".cosmon");
    fs::create_dir_all(cosmon.join("state")).unwrap();
    fs::create_dir_all(cosmon.join("formulas")).unwrap();
    fs::write(cosmon.join("config.toml"), config).unwrap();
    fs::write(cosmon.join("state/fleet.json"), "{}\n").unwrap();
    let formula_src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas/task-work.formula.toml");
    fs::copy(&formula_src, cosmon.join("formulas/task-work.formula.toml")).unwrap();
    fs::write(repo.join(".gitignore"), ".cosmon/state/\n.worktrees/\n").unwrap();
    for (path, contents) in files {
        write_file(repo, path, contents);
    }
}

/// Nucleate a `task-work` molecule and collapse it, so `cs done` merges it.
fn nucleate_terminal(repo: &Path, trust_store: &Path) -> String {
    let nuc = cs(repo, trust_store)
        .args([
            "--json",
            "nucleate",
            "task-work",
            "--var",
            "topic=issue 109",
        ])
        .output()
        .expect("cs nucleate");
    assert!(nuc.status.success(), "nucleate failed: {}", combined(&nuc));
    let v: serde_json::Value = serde_json::from_slice(&nuc.stdout).unwrap();
    let mol_id = v["id"].as_str().expect("nucleate id").to_owned();
    let col = cs(repo, trust_store)
        .args([
            "--json",
            "collapse",
            &mol_id,
            "--reason",
            "issue 109 fixture",
        ])
        .output()
        .expect("cs collapse");
    assert!(col.status.success(), "collapse failed: {}", combined(&col));
    mol_id
}

/// Commit everything on main as the base, then create `feat/<mol_id>` holding
/// `changes`. Returns the branch name; main stays checked out.
fn stage_branch(repo: &Path, mol_id: &str, changes: &[(&str, &str)]) -> String {
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-qm", "base"]);
    let branch = format!("feat/{mol_id}");
    git_ok(repo, &["checkout", "-q", "-b", &branch]);
    for (path, contents) in changes {
        write_file(repo, path, contents);
    }
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-qm", "worker change"]);
    git_ok(repo, &["checkout", "-q", "main"]);
    branch
}

fn grant_trust(repo: &Path, trust_store: &Path) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    apply_env(&mut cmd, repo, trust_store);
    let out = cmd
        .current_dir(repo)
        .arg("trust")
        .output()
        .expect("cs trust");
    assert!(out.status.success(), "cs trust failed: {}", combined(&out));
}

/// The molecule's state directory, wherever the fleet layout put it — the
/// value `cs tackle` exports as `COSMON_MOL_DIR` to that molecule's worker.
fn molecule_dir(repo: &Path, mol_id: &str) -> PathBuf {
    fn find(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == name) {
                    return Some(path);
                }
                if let Some(hit) = find(&path, name) {
                    return Some(hit);
                }
            }
        }
        None
    }
    find(&repo.join(".cosmon/state"), mol_id).expect("molecule state dir")
}

fn done(repo: &Path, trust_store: &Path, mol_id: &str, caller: Caller<'_>) -> Output {
    let mut cmd = cs(repo, trust_store);
    if let Caller::WorkerOf(own) = caller {
        // Exactly what `cs tackle` exports to the worker of `own`.
        cmd.env("CB_SESSION_ROLE", "worker")
            .env("CB_DEPTH", "1")
            .env("COSMON_MOL_DIR", molecule_dir(repo, own))
            .env("COSMON_PARENT_MOL_ID", own);
    }
    cmd.args(["done", mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done")
}

/// A trusted repository with no declared gate and no Cargo workspace, whose
/// molecule branch changes an ordinary file only.
fn plain_fixture(repo: &Path, store: &Path) -> String {
    setup_repo(
        repo,
        "[project]\nproject_id = \"issue-109\"\n",
        &[("notes.txt", "base\n")],
    );
    let mol_id = nucleate_terminal(repo, store);
    stage_branch(repo, &mol_id, &[("notes.txt", "worker change\n")]);
    grant_trust(repo, store);
    mol_id
}

/// Guard 1: the worker of the molecule may not harvest it.
#[test]
fn worker_cannot_harvest_its_own_molecule() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let mol_id = plain_fixture(repo, store.path());
    let main_before = rev(repo, "main");

    let out = done(repo, store.path(), &mol_id, Caller::WorkerOf(&mol_id));
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "a worker's `cs done` on its own molecule must be refused.\n{text}"
    );
    assert_eq!(rev(repo, "main"), main_before, "nothing may land.\n{text}");
    assert!(
        text.contains("harvest belongs to the pilot"),
        "the refusal must say whose gesture harvest is.\n{text}"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", &format!("feat/{mol_id}")])
            .status
            .success(),
        "the branch must survive the refusal.\n{text}"
    );
}

/// Guard 1, other side: the pilot's `cs done` on the same molecule proceeds.
#[test]
fn pilot_harvest_of_the_same_molecule_proceeds() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let mol_id = plain_fixture(repo, store.path());
    let main_before = rev(repo, "main");

    let out = done(repo, store.path(), &mol_id, Caller::Pilot);
    let text = combined(&out);
    assert!(
        out.status.success(),
        "the pilot's harvest must land.\n{text}"
    );
    assert_ne!(
        rev(repo, "main"),
        main_before,
        "the merge must land.\n{text}"
    );
}

/// Guard 1 is about *self*-harvest: a worker session driving a DAG (the
/// resident runtime's `cs done <child>`) harvests another molecule.
#[test]
fn worker_harvesting_another_molecule_proceeds() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let mol_id = plain_fixture(repo, store.path());
    let driver = nucleate_terminal(repo, store.path());
    let main_before = rev(repo, "main");

    let out = done(repo, store.path(), &mol_id, Caller::WorkerOf(&driver));
    let text = combined(&out);
    assert!(
        out.status.success(),
        "another session's harvest of this molecule must land.\n{text}"
    );
    assert_ne!(
        rev(repo, "main"),
        main_before,
        "the merge must land.\n{text}"
    );
}

/// The branch change that reproduces the issue: a new `post_merge` hook.
const HOOKED_CONFIG: &str =
    "[project]\nproject_id = \"issue-109\"\n\n[hooks]\npost_merge = 'sh scripts/deploy.sh'\n";

/// Assert the refusal of a trusted-surface change: non-zero, rolled back,
/// named with the landing sequence, branch preserved.
fn assert_surface_change_refused(repo: &Path, main_before: &str, out: &Output, branch: &str) {
    let text = combined(out);
    assert!(
        !out.status.success(),
        "a merge that changes the trusted shell surface must be refused.\n{text}"
    );
    assert_eq!(
        rev(repo, "main"),
        main_before,
        "the merge must be rolled back.\n{text}"
    );
    assert!(
        text.contains(REMEDY_MARKER),
        "the refusal must name the trusted-surface change.\n{text}"
    );
    assert!(
        text.contains("    • .cosmon/config.toml"),
        "the refusal must name the changed config.\n{text}"
    );
    assert!(
        git(repo, &["rev-parse", "--verify", branch])
            .status
            .success(),
        "the branch must survive the refusal.\n{text}"
    );
}

/// Guard 2, the no-gate rung: nobody declared how to verify the tree, so the
/// gate is advisory — and the surface change used to land with it.
#[test]
fn surface_change_is_refused_when_no_gate_is_declared() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(
        repo,
        "[project]\nproject_id = \"issue-109\"\n",
        &[("notes.txt", "base\n")],
    );
    let mol_id = nucleate_terminal(repo, store.path());
    let branch = stage_branch(
        repo,
        &mol_id,
        &[
            (".cosmon/config.toml", HOOKED_CONFIG),
            ("scripts/deploy.sh", "exit 0\n"),
        ],
    );
    grant_trust(repo, store.path());
    let main_before = rev(repo, "main");

    let out = done(repo, store.path(), &mol_id, Caller::Pilot);
    assert_surface_change_refused(repo, &main_before, &out, &branch);
}

/// Guard 2, the cargo rung: the branch also touches Rust, so `cargo check`
/// verifies the tree and no delegated command — the only rung that asked
/// about trust — ever runs.
#[test]
fn surface_change_is_refused_when_cargo_verifies_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(
        repo,
        "[project]\nproject_id = \"issue-109\"\n",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"issue-109-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("app/src/lib.rs", "pub fn healthy() {}\n"),
        ],
    );
    let mol_id = nucleate_terminal(repo, store.path());
    let branch = stage_branch(
        repo,
        &mol_id,
        &[
            ("app/src/lib.rs", "pub fn healthy() -> u8 { 1 }\n"),
            (".cosmon/config.toml", HOOKED_CONFIG),
            ("scripts/deploy.sh", "exit 0\n"),
        ],
    );
    grant_trust(repo, store.path());
    let main_before = rev(repo, "main");

    let out = done(repo, store.path(), &mol_id, Caller::Pilot);
    assert_surface_change_refused(repo, &main_before, &out, &branch);
}
