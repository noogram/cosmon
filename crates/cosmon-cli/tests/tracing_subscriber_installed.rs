// SPDX-License-Identifier: AGPL-3.0-only

//! COSMON #26 — `cs` must not discard its own warnings.
//!
//! Every `tracing::warn!` in `cosmon-cli` was emitted into a process with no
//! subscriber installed, so all of them went nowhere at any `RUST_LOG` level.
//! The external tester measured it on the release binary: `RUST_LOG=info`,
//! `debug` and `trace` each produced zero stderr lines.
//!
//! # Why this test runs the real binary
//!
//! Asserting that `tracing_subscriber::fmt()` is *called* proves the call, not
//! the delivery — and delivery is the whole defect. So these tests spawn the
//! actual `cs` executable on a code path that emits a real `warn!` and read its
//! streams, which is the only shape that fails on the unfixed binary.
//!
//! # The path chosen
//!
//! `cs local-worker --job <file>` (hidden transport plumbing) snapshots the
//! worktree before running its agent loop. When that snapshot cannot be taken —
//! here because the job names a worktree that does not exist, the same shape as
//! a worktree removed underneath a detached worker — `WorktreeBaseline::capture`
//! emits `could not snapshot pre-run worktree state; this turn will publish
//! nothing`. It is a genuine production warning on a genuine production path,
//! and it is reachable without tmux, without a git fixture, and without a model
//! server.
//!
//! The agent loop that runs afterwards is expected to fail (it is pointed at a
//! closed port), so these tests never assert on the exit status — only on where
//! the warning went.

use std::path::Path;
use std::process::Command;

/// The exact production warning this test drives out of the real binary.
const WARNING: &str = "could not snapshot pre-run worktree state";

fn cosmon_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cs"));
    cmd.env_remove("COSMON_PARENT_MOL_ID")
        .env_remove("COSMON_MOL_DIR")
        // Inherited worker env poisons `cs`-spawning tests; strip it so the
        // binary under test starts from a clean floor.
        .env_remove("CB_DEPTH")
        .env_remove("ANTHROPIC_MODEL")
        .env_remove("RUST_LOG")
        // Fail the agent loop fast: a closed port refuses immediately, so the
        // test does not wait on a model server it never wanted.
        .env("COSMON_LOCAL_BASE_URL", "http://127.0.0.1:1")
        .env("COSMON_LOCAL_TIMEOUT", "5");
    cmd
}

/// Nucleate one molecule and write a `local-worker` job that points at a
/// worktree which does not exist. Returns the tempdir (kept alive by the
/// caller) and the job-file path.
fn setup_job() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let formulas_dir = tmp.path().join("formulas");
    std::fs::create_dir_all(&formulas_dir).unwrap();

    let formula_toml = r#"
formula = "tracing-subscriber-test"
version = 1
description = "One-step formula for the COSMON #26 subscriber test"
id_prefix = "trc"

[[steps]]
id = "step-1"
title = "Step 1"
description = "Solo step — never actually executed by this test."
acceptance = "Done"
"#;
    std::fs::write(
        formulas_dir.join("tracing-subscriber-test.formula.toml"),
        formula_toml,
    )
    .unwrap();

    let output = cosmon_bin()
        .current_dir(tmp.path())
        .args([
            "--json",
            "nucleate",
            "tracing-subscriber-test",
            "--store-dir",
            state_dir.to_str().unwrap(),
            "--formulas-dir",
            formulas_dir.to_str().unwrap(),
        ])
        .output()
        .expect("nucleate failed to spawn");
    assert!(
        output.status.success(),
        "nucleate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let nucleate_json: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    let molecule_id = nucleate_json["id"].as_str().unwrap().to_owned();

    let molecule_dir = tmp.path().join("molecule");
    std::fs::create_dir_all(&molecule_dir).unwrap();

    // Deliberately never created: the snapshot must fail, and a missing
    // directory fails identically on every machine, whereas an existing
    // non-git directory depends on whether the tempdir's ancestry happens to
    // sit inside somebody's repository.
    let worktree_path = tmp.path().join("worktree-that-does-not-exist");

    let job = serde_json::json!({
        "adapter_name": "local",
        "worker_id": "tracing-subscriber-test",
        "session_name": "tracing-subscriber-test",
        "worktree_path": worktree_path,
        "prompt": "unused — the agent loop is pointed at a closed port",
        "molecule_id": molecule_id,
        "molecule_dir": molecule_dir,
        "state_dir": state_dir,
        "adapter_entry": serde_json::Value::Null,
        "preferred_model": serde_json::Value::Null,
    });
    let job_path = tmp.path().join("job.json");
    std::fs::write(&job_path, serde_json::to_vec(&job).unwrap()).unwrap();

    (tmp, job_path)
}

fn run_local_worker(cwd: &Path, job_path: &Path, rust_log: Option<&str>) -> std::process::Output {
    let mut cmd = cosmon_bin();
    cmd.current_dir(cwd)
        .args(["local-worker", "--job", job_path.to_str().unwrap()]);
    if let Some(value) = rust_log {
        cmd.env("RUST_LOG", value);
    }
    cmd.output().expect("cs local-worker failed to spawn")
}

/// The defect itself: with no `RUST_LOG` at all, a production `warn!` must
/// reach the operator's stderr. On the unfixed binary this assertion fails —
/// stderr carries the command's own error text and not one line of the
/// warning.
#[test]
fn warning_reaches_stderr_with_no_rust_log() {
    let (tmp, job_path) = setup_job();
    let output = run_local_worker(tmp.path(), &job_path, None);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(WARNING),
        "`cs` must not discard its own warnings (COSMON #26); \
         no subscriber installed means this text goes nowhere.\nstderr:\n{stderr}"
    );
}

/// `cs` has `--json` on every command and its stdout is parsed. A subscriber
/// on stdout would corrupt machine-readable output — a worse defect than the
/// silence being fixed — so the warning must appear on stderr and nowhere else.
#[test]
fn warning_never_touches_stdout() {
    let (tmp, job_path) = setup_job();
    let output = run_local_worker(tmp.path(), &job_path, None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(WARNING),
        "diagnostics on stdout would corrupt `--json` output.\nstdout:\n{stdout}"
    );
}

/// `RUST_LOG` is what an operator reaches for, and it is what the external
/// report measured. It must be honoured as a genuine override — including when
/// it asks for *less* than the `warn` default, which is the direction that
/// distinguishes "the filter is read" from "the filter is ignored and the
/// default happens to print".
#[test]
fn rust_log_overrides_the_default_floor() {
    let (tmp, job_path) = setup_job();

    let quiet = run_local_worker(tmp.path(), &job_path, Some("error"));
    assert!(
        !String::from_utf8_lossy(&quiet.stderr).contains(WARNING),
        "RUST_LOG=error must silence a warn-level event; it is an override, \
         not an addition.\nstderr:\n{}",
        String::from_utf8_lossy(&quiet.stderr)
    );

    let loud = run_local_worker(tmp.path(), &job_path, Some("info"));
    assert!(
        String::from_utf8_lossy(&loud.stderr).contains(WARNING),
        "RUST_LOG=info must still carry warn-level events — this is the exact \
         invocation the external report measured at zero lines.\nstderr:\n{}",
        String::from_utf8_lossy(&loud.stderr)
    );
}
