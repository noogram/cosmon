// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end tests for `cs migrate to solo` — *solo = TOTAL*.
//!
//! These tests reproduce the noesis fixture:
//!   - a git repo with nine `.cosmon/*` structural files pre-tracked
//!     (`config.toml`, `surfaces.toml`, `.gitignore`, and six
//!     `formulas/*.toml`);
//!   - `cs migrate to solo` must untrack **all** of them, not just
//!     `.cosmon/state/`;
//!   - `cs migrate verify` must then catch the *solo partial* state as
//!     a residence-invariant violation.
//!
//! The three named scenarios from the task brief:
//!
//!   1. `fixture_solo_total_untrack` — 9 structural files
//!      pre-tracked, migrate to solo, assert `git ls-files .cosmon/ =
//!      0` + `.cosmon/` in `.git/info/exclude` + verify PASS.
//!   2. `fixture_solo_idempotent` — migrate to solo twice, second run
//!      must not duplicate the exclude line and must not error.
//!   3. `fixture_verify_catches_partial` — synthetic pre-partial
//!      state (exclude has `.cosmon/state/` but structural files
//!      still tracked): verify must FAIL with an actionable message.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cs_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // Suppress operator.* event emission so events.jsonl
        // remains byte-stable across cs invocations — the migrate
        // verify path seals events.jsonl as an orphan and re-runs
        // the cs binary, so any append-only telemetry from the
        // CLI itself would break the seal. See
        // crates/cosmon-cli/src/operator_event.rs::emission_disabled.
        .env("COSMON_NO_OPERATOR_EVENTS", "1");
    cmd
}

fn cs_in_galaxy(state_dir: &Path) -> Command {
    let cosmon_dir = state_dir.parent().expect("state_dir has a parent");
    let config_path = cosmon_dir.join("config.toml");
    let mut cmd = cs_bin();
    cmd.env("COSMON_STATE_DIR", state_dir)
        .env("COSMON_CONFIG", config_path)
        .current_dir(state_dir);
    cmd
}

fn run_git(repo_root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .status()
        .expect("git invocation failed in test setup");
    assert!(status.success(), "git {args:?} failed");
}

fn git_ls_files(repo_root: &Path, rel: &str) -> Vec<String> {
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(["ls-files", "--", rel])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Seed a git repo at `root` that mirrors the noesis pre-fa82 state:
/// nine tracked `.cosmon/*` structural files + a small tracked state
/// subtree. The nine structural files are exactly what the operator
/// reported: `config.toml`, `surfaces.toml`, `.cosmon/.gitignore`, and
/// six `formulas/*.toml`. Returns the `.cosmon/state` absolute path.
fn seed_noesis_fixture(root: &Path) -> PathBuf {
    run_git(root, &["init", "-q", "-b", "main"]);
    run_git(root, &["config", "user.email", "test@cosmon.local"]);
    run_git(root, &["config", "user.name", "Cosmon Test"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("README.md"), "# noesis fixture\n").unwrap();
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-q", "-m", "initial code"]);

    let cosmon_dir = root.join(".cosmon");
    fs::create_dir_all(&cosmon_dir).unwrap();

    // Structural files (9 total — mirrors noesis):
    fs::write(
        cosmon_dir.join("config.toml"),
        "[project]\nproject_id = \"noesis-fixture\"\n",
    )
    .unwrap();
    fs::write(cosmon_dir.join("surfaces.toml"), "# surfaces\n").unwrap();
    fs::write(cosmon_dir.join(".gitignore"), "state.next/\nstate.prev/\n").unwrap();
    let formulas_dir = cosmon_dir.join("formulas");
    fs::create_dir_all(&formulas_dir).unwrap();
    for name in [
        "task-work",
        "deep-think",
        "temp-review",
        "idea-to-plan",
        "mission-plan",
        "mission-controller",
    ] {
        fs::write(
            formulas_dir.join(format!("{name}.formula.toml")),
            format!("name = \"{name}\"\n"),
        )
        .unwrap();
    }

    // A minimal state subtree so cs migrate has artifacts to seal.
    let state = cosmon_dir.join("state");
    let mol_dir = state.join("fleets/default/molecules/task-20260420-a1b2");
    fs::create_dir_all(&mol_dir).unwrap();
    fs::write(mol_dir.join("state.json"), r#"{"id":"task-20260420-a1b2"}"#).unwrap();
    fs::write(mol_dir.join("prompt.md"), "operator intent\n").unwrap();

    // Track EVERYTHING under .cosmon/ — this is the pre-fa82 state.
    run_git(root, &["add", ".cosmon"]);
    run_git(
        root,
        &["commit", "-q", "-m", "track .cosmon/ (pre-fa82 legacy)"],
    );

    state
}

#[test]
fn fixture_solo_total_untrack() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let state = seed_noesis_fixture(repo);

    // Precondition: the nine structural files + state subtree are
    // tracked. This is the input condition the task brief pins down.
    let before = git_ls_files(repo, ".cosmon");
    assert!(
        before.len() >= 9,
        "precondition: expected at least 9 tracked files under .cosmon/, got {}: {before:?}",
        before.len(),
    );

    // Act: cs migrate to solo.
    let out = cs_in_galaxy(&state)
        .arg("--json")
        .args(["migrate", "to", "solo"])
        .output()
        .expect("cs migrate to solo failed to spawn");
    assert!(
        out.status.success(),
        "migrate to solo exited non-zero: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Solo TOTAL: zero tracked files under .cosmon/ afterwards.
    let after = git_ls_files(repo, ".cosmon");
    assert!(
        after.is_empty(),
        "solo TOTAL failed: {} file(s) still tracked under .cosmon/: {after:?}",
        after.len(),
    );

    // .git/info/exclude now contains `.cosmon/` (the whole subtree).
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert!(
        exclude.lines().any(|l| l.trim() == ".cosmon/"),
        ".git/info/exclude must contain `.cosmon/`, got: {exclude:?}",
    );

    // Shared .gitignore must NOT carry cosmon rules — solo leaks nothing.
    let root_gitignore = repo.join(".gitignore");
    if root_gitignore.exists() {
        let shared = fs::read_to_string(&root_gitignore).unwrap();
        assert!(
            !shared.contains(".cosmon"),
            "solo must not add .cosmon rules to shared .gitignore, got: {shared:?}",
        );
    }

    // cs migrate verify now returns PASS.
    let verify = cs_in_galaxy(&state)
        .args(["migrate", "verify"])
        .output()
        .expect("cs migrate verify failed to spawn");
    assert!(
        verify.status.success(),
        "verify after solo must PASS: stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr),
    );
    let verify_stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(
        verify_stdout.contains("PASS"),
        "verify output must contain PASS: {verify_stdout}",
    );
}

#[test]
fn fixture_solo_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let state = seed_noesis_fixture(repo);

    let mk = || {
        cs_in_galaxy(&state)
            .arg("--json")
            .args(["migrate", "to", "solo"])
            .output()
            .expect("cs migrate to solo failed to spawn")
    };

    let first = mk();
    assert!(
        first.status.success(),
        "first run must succeed: {}",
        String::from_utf8_lossy(&first.stderr),
    );

    let second = mk();
    assert!(
        second.status.success(),
        "second run must succeed (idempotent): {}",
        String::from_utf8_lossy(&second.stderr),
    );

    // No duplicate exclude line.
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    let count = exclude.lines().filter(|l| l.trim() == ".cosmon/").count();
    assert_eq!(
        count, 1,
        "`.cosmon/` line must appear exactly once in .git/info/exclude, got {count}: {exclude:?}",
    );

    // Still zero tracked files.
    assert!(git_ls_files(repo, ".cosmon").is_empty());
}

#[test]
fn fixture_verify_catches_partial() {
    // Reproduces the noesis false-positive: exclude contains only
    // `.cosmon/state/`, the structural files are still tracked. Verify
    // must FAIL with a message that names the problem and points at a
    // remediation.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let state = seed_noesis_fixture(repo);

    // Synthesise the "solo partial" state: exclude .cosmon/state/
    // only, remove just the state subtree from the index. Structural
    // files stay tracked — mirrors noesis before fa82.
    let exclude_path = repo.join(".git/info/exclude");
    fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
    let mut exclude = fs::read_to_string(&exclude_path).unwrap_or_default();
    if !exclude.ends_with('\n') && !exclude.is_empty() {
        exclude.push('\n');
    }
    exclude.push_str(".cosmon/state/\n");
    fs::write(&exclude_path, exclude).unwrap();
    run_git(repo, &["rm", "-r", "--cached", "--quiet", ".cosmon/state"]);
    run_git(
        repo,
        &["commit", "-q", "-m", "solo partial: exclude state only"],
    );

    // Seal a manifest claiming this is solo — this is what the earlier
    // (partial) `cs migrate to solo` would have produced without the
    // fa82 fix. We use `--no-git` to keep the git index untouched by
    // the migrate command itself; the partial state above is what we
    // are asserting verify against.
    let out = cs_in_galaxy(&state)
        .args(["migrate", "to", "solo", "--no-git", "--no-commit"])
        .output()
        .expect("cs migrate failed to spawn");
    assert!(
        out.status.success(),
        "migrate --no-git must still seal: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Precondition: structural files are still tracked (partial state).
    let tracked = git_ls_files(repo, ".cosmon");
    assert!(
        !tracked.is_empty(),
        "precondition: expected structural files still tracked, got none",
    );

    // Verify must FAIL with a residence-invariant violation.
    let verify = cs_in_galaxy(&state)
        .args(["migrate", "verify"])
        .output()
        .expect("cs migrate verify failed to spawn");
    assert!(
        !verify.status.success(),
        "verify must FAIL on solo partial: stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr),
    );
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(stdout.contains("FAIL"), "expected FAIL in output: {stdout}");
    assert!(
        stdout.contains("solo residence"),
        "output must flag solo residence violation: {stdout}",
    );
    assert!(
        stdout.contains("git rm") || stdout.contains("cs migrate to solo"),
        "output must include actionable remediation: {stdout}",
    );
}
