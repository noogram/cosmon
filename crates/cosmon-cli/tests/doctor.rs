// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end integration tests for `cs doctor` security probes (sprint 1).
//!
//! Each probe gets one black-box test that drives the compiled `cs` binary
//! against a temporary git repository, asserting:
//!
//! - `cs doctor leaks` exits 1 when a fixture file contains a GitHub PAT.
//! - `cs doctor leaks --json` emits a JSON payload with the expected
//!   `severity=error` finding.
//! - `cs doctor worktrees --root <dir>` detects a world-writable directory.
//! - `cs doctor mcp --registry <path>` flags inline tokens in args[].
//! - `cs doctor deps --root <dir>` flags wildcard version pins.
//! - `cs doctor security` surfaces the blocking leak finding from the
//!   aggregated umbrella.
//! - `cs doctor --help` lists every subcommand.
//!
//! These tests replace no unit test — they guarantee the CLI wiring,
//! exit-code contract, and JSON envelope are correct from a user's
//! perspective.

use std::fs;
use std::path::Path;
use std::process::Command;

fn cs() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR");
    cmd
}

fn init_git_repo(dir: &Path) {
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["config", "user.email", "doctor@test"]);
    run_git(dir, &["config", "user.name", "Doctor Test"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
}

fn commit_all(dir: &Path, msg: &str) {
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn doctor_help_lists_every_probe() {
    let out = cs()
        .args(["doctor", "--help"])
        .output()
        .expect("cs doctor --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "help exited non-zero: {stdout}");
    for probe in ["whisper", "leaks", "worktrees", "mcp", "deps", "security"] {
        assert!(
            stdout.contains(probe),
            "`cs doctor --help` missing `{probe}`:\n{stdout}"
        );
    }
}

#[test]
fn leaks_blocks_on_committed_pat() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    fs::write(
        tmp.path().join("leaked.env"),
        "GITHUB_TOKEN=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef01\n",
    )
    .unwrap();
    commit_all(tmp.path(), "seed");

    let out = cs()
        .args(["doctor", "leaks"])
        .current_dir(tmp.path())
        .output()
        .expect("run cs doctor leaks");
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ERROR"));
    assert!(stdout.contains("GitHub"));
}

#[test]
fn leaks_json_mode_produces_structured_finding() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    fs::write(
        tmp.path().join("conf.toml"),
        "key = \"AKIAIOSFODNN7EXAMPLE\"\n",
    )
    .unwrap();
    commit_all(tmp.path(), "seed");

    let out = cs()
        .args(["--json", "doctor", "leaks"])
        .current_dir(tmp.path())
        .output()
        .expect("run cs doctor leaks --json");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("parse JSON");
    let reports = value["reports"].as_array().expect("reports array");
    assert_eq!(reports.len(), 1);
    let findings = reports[0]["findings"].as_array().expect("findings array");
    assert!(
        findings.iter().any(|f| f["severity"] == "error"),
        "no error finding in {findings:?}"
    );
}

#[test]
fn leaks_clean_repo_passes() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    fs::write(tmp.path().join("README.md"), "nothing to see here\n").unwrap();
    commit_all(tmp.path(), "clean");

    let out = cs()
        .args(["doctor", "leaks"])
        .current_dir(tmp.path())
        .output()
        .expect("run cs doctor leaks");
    assert!(
        out.status.success(),
        "clean repo should exit zero; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn worktrees_flags_world_writable_dir() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    commit_all(tmp.path(), "seed");
    let bad = tmp.path().join(".worktrees").join("rogue");
    fs::create_dir_all(&bad).unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o777)).unwrap();

    let out = cs()
        .args([
            "doctor",
            "worktrees",
            "--root",
            &tmp.path().display().to_string(),
        ])
        .output()
        .expect("run cs doctor worktrees");
    assert!(out.status.success(), "worktrees probe is warning-only");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("world-writable"), "stdout: {stdout}");
}

#[test]
fn mcp_audit_flags_inline_token_in_args() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("neurion.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(neurion_core::schema::SCHEMA_SQL)
        .unwrap();
    conn.execute_batch(neurion_core::schema::HYPERGRAPH_SQL)
        .unwrap();
    conn.execute_batch(
        "INSERT INTO mcp_servers (name, command, args) VALUES \
         ('evil', 'echo', '[\"--token\", \"ghp_LEAKEDTOKEN1234ABCD\"]');",
    )
    .unwrap();
    drop(conn);

    let out = cs()
        .args(["doctor", "mcp", "--registry", &db.display().to_string()])
        .output()
        .expect("run cs doctor mcp");
    assert!(!out.status.success(), "inline token should block");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("evil"), "stdout: {stdout}");
    assert!(stdout.contains("ERROR"), "stdout: {stdout}");
}

#[test]
fn mcp_audit_missing_registry_is_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let out = cs()
        .args([
            "doctor",
            "mcp",
            "--registry",
            &tmp.path().join("absent.db").display().to_string(),
        ])
        .output()
        .expect("run cs doctor mcp");
    assert!(
        out.status.success(),
        "missing registry must not fail CI; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn deps_flags_wildcard_version() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    fs::write(
        tmp.path().join("Cargo.toml"),
        r#"[package]
name = "leaky"
version = "0.1.0"
edition = "2021"

[dependencies]
wild = "*"
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
    commit_all(tmp.path(), "seed");

    let out = cs()
        .args([
            "doctor",
            "deps",
            "--root",
            &tmp.path().display().to_string(),
        ])
        .output()
        .expect("run cs doctor deps");
    assert!(out.status.success(), "deps probe is warning-only");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("wild"), "stdout: {stdout}");
    assert!(stdout.contains("`*`"), "stdout: {stdout}");
}

#[test]
fn security_umbrella_aggregates_blocking_leak() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    fs::write(tmp.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    fs::write(
        tmp.path().join("secret.txt"),
        "ghp_ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ\n",
    )
    .unwrap();
    commit_all(tmp.path(), "seed");

    let tmp_reg = tempfile::tempdir().unwrap();
    let out = cs()
        .args([
            "doctor",
            "security",
            "--root",
            &tmp.path().display().to_string(),
            "--registry",
            &tmp_reg.path().join("absent.db").display().to_string(),
        ])
        .current_dir(tmp.path())
        .output()
        .expect("run cs doctor security");
    assert!(
        !out.status.success(),
        "umbrella should propagate leak non-zero; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The umbrella output should include the leak title.
    assert!(
        stdout.to_lowercase().contains("github"),
        "expected github PAT finding in stdout: {stdout}"
    );
}
