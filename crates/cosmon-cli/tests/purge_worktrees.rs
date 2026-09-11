// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end tests for `cs purge --worktrees` — the issue-61 reclamation
//! surface, driven through the compiled binary.
//!
//! The three properties under test are the ones an operator's bytes depend on:
//!
//! * the **default is dry** — `--worktrees` alone reports and removes nothing;
//! * the **withheld register is concrete** — every entry names a reason, and
//!   the assertions below read the rendered sentence, not a count, because a
//!   count stays right while the sentence rots;
//! * **no path removes a worktree** — the executing run takes the derived
//!   payload and leaves the directory, the lock anchor and the durable files
//!   exactly where they were (ADR-178).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cs(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cs"))
        .env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(repo)
        .arg("--config")
        .arg(state)
        .args(args)
        .output()
        .expect("spawn cs")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with one molecule-less `.worktrees/` directory carrying a
/// Cargo build tree — the class the pre-P3 roster could not see at all.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    orphan: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&state).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "maintainers@noogram.org"]);
    git(&repo, &["config", "user.name", "Noogram"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join(".gitignore"), ".worktrees/\ntarget/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "test: seed"]);

    let orphan = repo.join(".worktrees").join("task-20260101-dead");
    fs::create_dir_all(orphan.join("target/debug")).unwrap();
    fs::write(orphan.join("target/debug/.cargo-lock"), "").unwrap();
    fs::write(orphan.join("target/debug/payload.o"), "rebuildable").unwrap();
    fs::write(orphan.join("NOTES.md"), "the only copy of something").unwrap();

    Fixture {
        _tmp: tmp,
        repo,
        state,
        orphan,
    }
}

/// Falsifier 2 — `cs purge --worktrees` with no further flag removes nothing
/// and prints the plan.
#[test]
fn worktrees_pass_is_dry_by_default() {
    let f = fixture();
    let out = cs(&f.repo, &f.state, &["purge", "--worktrees"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("dry run — nothing will be removed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Repeat with --allow-unharvested"),
        "the plan must say what would execute it:\n{stdout}"
    );
    // The plan named the reclaimable root…
    assert!(stdout.contains("task-20260101-dead"), "{stdout}");
    // …and took nothing.
    assert!(f.orphan.join("target/debug/payload.o").is_file());
    assert!(f.orphan.join("NOTES.md").is_file());
}

/// Falsifier 3 — the withheld register names a concrete reason per entry.
///
/// Asserted on the rendered sentence. "1 worktree withheld" is not actionable;
/// "git does not know this directory as a worktree" is.
#[test]
fn withheld_register_names_a_concrete_reason() {
    let f = fixture();
    let out = cs(&f.repo, &f.state, &["purge", "--worktrees"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("worktree(s) withheld"), "{stdout}");
    assert!(
        stdout.contains("on disk, unregistered"),
        "the register must say how the candidate was found:\n{stdout}"
    );
    assert!(
        stdout.contains("durable: git does not know this directory as a worktree"),
        "the register must name *why*, in the concrete register:\n{stdout}"
    );
    assert!(
        stdout.contains("No path in this command removes a worktree"),
        "the surface must state its own limit (ADR-178):\n{stdout}"
    );
}

/// `--dry-run` overrides the operator's execute gesture, for the whole
/// command. A flag that says "change nothing" and then changes something is
/// worse than no flag.
#[test]
fn dry_run_overrides_allow_unharvested() {
    let f = fixture();
    let out = cs(
        &f.repo,
        &f.state,
        &["purge", "--worktrees", "--allow-unharvested", "--dry-run"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("dry run — nothing will be removed"),
        "{stdout}"
    );
    assert!(f.orphan.join("target/debug/payload.o").is_file());
}

/// The executing run takes the derived payload — and only that.
///
/// The worktree directory, its durable file and the lock anchor all survive:
/// selecting `target` names its payload, never the directory's identity, and
/// no flag on this command reaches durable content.
#[test]
fn executing_run_takes_derived_payload_and_leaves_everything_else() {
    let f = fixture();
    let out = cs(
        &f.repo,
        &f.state,
        &["purge", "--worktrees", "--allow-unharvested"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}\n{stdout}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("executing — derived output only"),
        "{stdout}"
    );
    assert!(
        !f.orphan.join("target/debug/payload.o").exists(),
        "the rebuildable payload should be gone:\n{stdout}"
    );
    assert!(
        f.orphan.join("target/debug/.cargo-lock").is_file(),
        "the lock anchor is preserved, or another process can take its name"
    );
    assert!(
        f.orphan.join("NOTES.md").is_file(),
        "durable content is never reclaimed by this command"
    );
    assert!(f.orphan.is_dir(), "no automatic path removes a worktree");
}

/// A configured root cosmon cannot establish exclusion over is reported and
/// left alone — the acquired lock is Cargo's, and Cargo is all it excludes.
#[test]
fn non_cargo_evict_root_is_withheld_with_its_reason() {
    let f = fixture();
    fs::create_dir_all(f.state.join(".")).unwrap();
    fs::write(
        f.state.join("config.toml"),
        "[worktree_reclaim]\nevict = [\"target\", \"build/ios\"]\n",
    )
    .unwrap();
    fs::create_dir_all(f.orphan.join("build/ios")).unwrap();
    fs::write(f.orphan.join("build/ios/lib.a"), "archive").unwrap();

    let out = cs(
        &f.repo,
        &f.state,
        &["purge", "--worktrees", "--allow-unharvested"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("no exclusion protocol"),
        "the withheld root must carry its reason:\n{stdout}"
    );
    assert!(stdout.contains("build/ios"), "{stdout}");
    assert!(
        f.orphan.join("build/ios/lib.a").is_file(),
        "a root without established exclusion must not be reclaimed"
    );
}
