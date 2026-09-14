// SPDX-License-Identifier: AGPL-3.0-only

//! Issue #74, proposal (1): when `cs done`'s post-merge gate refuses because
//! **the merge itself** turned a trusted shell surface stale, the rollback
//! message must name the path that works — not `cs trust` alone, which loops.
//!
//! The four falsifiers, against the real `cs` binary and a real trust store:
//!
//! 1. trusted repo, branch adds a script to the shell surface → the refusal
//!    carries the manual sequence and names the added path;
//! 2. trusted repo, branch breaks the Rust build → the compile-failure
//!    message, never the trust sequence;
//! 3. never-trusted repo → the existing "not trusted" message, never the
//!    merge-staleness one;
//! 4. following the printed sequence verbatim lands the branch: `cs done`
//!    reports it already merged and tears down.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Marker line that opens the remedy block; its absence is the "no trust
/// sequence" verdict in falsifiers 2 and 3.
const REMEDY_MARKER: &str = "this merge changes the repository's trusted shell surface";

/// A `cs` command pinned to the fixture's state, config and trust store, with
/// the trust bypass REMOVED — the gate under test must really run.
fn cs(repo: &Path, trust_store: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    apply_env(&mut cmd, repo, trust_store);
    cmd.current_dir(repo.join(".cosmon/state"));
    cmd
}

fn apply_env(cmd: &mut Command, repo: &Path, trust_store: &Path) {
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        .env_remove("COSMON_SKIP_PRE_DONE_HOOK")
        .env_remove("COSMON_ASSUME_TRUSTED")
        .env("COSMON_STATE_DIR", repo.join(".cosmon/state"))
        .env("COSMON_CONFIG", repo.join(".cosmon/config.toml"))
        .env("COSMON_TRUST_DIR", trust_store)
        .env("GIT_PAGER", "cat");
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

/// A repository whose `.cosmon/config.toml` and formulas are TRACKED, so a
/// molecule branch can change the shell surface through an ordinary merge.
/// `config` is the whole config file; `files` are extra tracked files.
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

fn write_file(repo: &Path, path: &str, contents: &str) {
    let full = repo.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, contents).unwrap();
}

/// Nucleate a `task-work` molecule and collapse it, so `cs done` merges it.
fn nucleate_terminal(repo: &Path, trust_store: &Path) -> String {
    let nuc = cs(repo, trust_store)
        .args(["--json", "nucleate", "task-work", "--var", "topic=issue 74"])
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
            "issue 74 fixture",
        ])
        .output()
        .expect("cs collapse");
    assert!(col.status.success(), "collapse failed: {}", combined(&col));
    mol_id
}

/// Commit everything on main as the base, then create `feat/<mol_id>` holding
/// `changes` (path, contents). Returns the branch name; main stays checked out.
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

fn done(repo: &Path, trust_store: &Path, mol_id: &str) -> Output {
    cs(repo, trust_store)
        .args(["done", mol_id, "--no-auto-propel"])
        .output()
        .expect("cs done")
}

const GATED_CONFIG: &str =
    "[project]\nproject_id = \"issue-74\"\n\n[gates]\nintegrity_command = 'sh scripts/gate.sh'\n";

/// The shell-surface fixture: a polyglot repo whose declared integrity gate
/// delegates to `scripts/gate.sh`. The branch adds `scripts/lint.sh` and wires
/// it into the gate — exactly the shape of the 2026-09-14 observation.
fn surface_changing_fixture(repo: &Path, trust_store: &Path, trusted: bool) -> (String, String) {
    setup_repo(
        repo,
        GATED_CONFIG,
        &[
            ("scripts/gate.sh", "exit 0\n"),
            ("src/main.py", "print('hi')\n"),
        ],
    );
    let mol_id = nucleate_terminal(repo, trust_store);
    let branch = stage_branch(
        repo,
        &mol_id,
        &[
            ("scripts/lint.sh", "exit 0\n"),
            (
                ".cosmon/config.toml",
                "[project]\nproject_id = \"issue-74\"\n\n[gates]\nintegrity_command = 'sh scripts/gate.sh && sh scripts/lint.sh'\n",
            ),
        ],
    );
    if trusted {
        grant_trust(repo, trust_store);
    }
    (mol_id, branch)
}

/// Falsifier 1 + 4: the message names the added path and the sequence, and
/// running that sequence verbatim lands the branch through `already merged`.
#[test]
fn merge_that_invalidates_trust_prints_a_sequence_that_lands_the_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let (mol_id, branch) = surface_changing_fixture(repo, store.path(), true);
    let main_before = rev(repo, "main");

    let refused = done(repo, store.path(), &mol_id);
    let text = combined(&refused);
    assert!(!refused.status.success(), "the gate must refuse.\n{text}");
    assert_eq!(
        rev(repo, "main"),
        main_before,
        "the merge must be rolled back.\n{text}"
    );
    assert!(
        text.contains(REMEDY_MARKER),
        "the refusal must name the class.\n{text}"
    );
    assert!(
        text.contains("    • scripts/lint.sh"),
        "the refusal must name the shell-surface path the branch adds.\n{text}"
    );
    assert!(
        text.contains(&format!("$ cs done {mol_id}"))
            && text.contains(&format!("Merge branch '%s'\\n\" {branch}")),
        "the sequence must carry the molecule id and branch.\n{text}"
    );

    // Falsifier 4 — run the printed `$ ` lines as a script. The only edit is
    // dropping `-S`: the fixture has no signing key, and signing is not what
    // this test proves.
    let script: Vec<String> = text
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("$ "))
        .map(|l| l.replace(" -S ", " "))
        .collect();
    assert_eq!(script.len(), 6, "six commands expected: {script:#?}");
    let bin_dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_cs"), bin_dir.path().join("cs")).unwrap();
    let path: PathBuf = bin_dir.path().to_owned();
    let mut sh = Command::new("sh");
    apply_env(&mut sh, repo, store.path());
    let followed = sh
        .env(
            "PATH",
            format!(
                "{}:{}",
                path.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .arg("-ec")
        .arg(script.join("\n"))
        .output()
        .expect("sh");
    let followed_text = combined(&followed);
    assert!(
        followed.status.success(),
        "the printed sequence must run to completion.\nscript={script:#?}\n{followed_text}"
    );
    assert!(
        followed_text.contains("already_merged") && !followed_text.contains("empty_branch"),
        "the final `cs done` must see the branch already merged.\n{followed_text}"
    );
    assert!(
        !git(repo, &["rev-parse", "--verify", &branch])
            .status
            .success(),
        "`cs done` must tear the branch down.\n{followed_text}"
    );
    assert_eq!(
        String::from_utf8_lossy(&git(repo, &["log", "-1", "--format=%s", "main"]).stdout).trim(),
        format!("Merge branch '{branch}'"),
        "the hand merge must keep the provenance subject"
    );
    assert!(git(repo, &["cat-file", "-e", "main:scripts/lint.sh"])
        .status
        .success());
}

/// Falsifier 2: a trusted repo whose branch breaks the Rust build gets the
/// compile-failure message, not the trust sequence.
#[test]
fn compile_failure_keeps_its_own_message() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_repo(
        repo,
        "[project]\nproject_id = \"issue-74\"\n",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"issue-74-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("app/src/lib.rs", "pub fn healthy() {}\n"),
        ],
    );
    let mol_id = nucleate_terminal(repo, store.path());
    stage_branch(
        repo,
        &mol_id,
        &[("app/src/lib.rs", "pub fn healthy() -> u8 { \"no\" }\n")],
    );
    grant_trust(repo, store.path());

    let refused = done(repo, store.path(), &mol_id);
    let text = combined(&refused);
    assert!(
        !refused.status.success(),
        "a compile failure must refuse.\n{text}"
    );
    assert!(
        text.contains("POST-MERGE COMPILE GATE REFUSED") && text.contains("cargo check"),
        "the refusal must be the compile-failure one.\n{text}"
    );
    assert!(
        !text.contains(REMEDY_MARKER),
        "no trust sequence for a compile failure.\n{text}"
    );
    assert!(
        !text.contains("$ cs trust"),
        "no trust sequence for a compile failure.\n{text}"
    );
}

/// Falsifier 3: a repository never trusted keeps the "not trusted" message.
#[test]
fn never_trusted_repo_keeps_the_not_trusted_message() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let (mol_id, _branch) = surface_changing_fixture(repo, store.path(), false);

    let refused = done(repo, store.path(), &mol_id);
    let text = combined(&refused);
    assert!(
        !refused.status.success(),
        "an untrusted gate must refuse.\n{text}"
    );
    assert!(
        text.contains("repository not trusted"),
        "the refusal must be the existing not-trusted message.\n{text}"
    );
    assert!(!text.contains("repository trust is stale"), "{text}");
    assert!(
        !text.contains(REMEDY_MARKER),
        "no merge-staleness sequence.\n{text}"
    );
}
