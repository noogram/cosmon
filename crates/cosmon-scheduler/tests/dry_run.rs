// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end tests of `cosmon-scheduler tick` and `tick --dry-run`
//! against disk-backed fixtures. Covers both Step 1 (dry-run only) and
//! Step 2 (real dispatch + state.json) exit criteria from
//! [idea-20260417-b52d/plan.md].

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

/// Locate the `cosmon-scheduler` binary under `target/` by asking Cargo
/// where it lives via the CARGO env var set when running `cargo test`.
fn scheduler_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cosmon-scheduler"))
}

fn sample_toml() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("patrols.sample.toml")
}

#[test]
fn dry_run_prints_decisions_for_sample_toml() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(scheduler_bin())
        .arg("tick")
        .arg("--dry-run")
        .arg("--config")
        .arg(sample_toml())
        // Guarantee a deterministic environment for gated patrols.
        .env_remove("PATROL_REPLIES_READY")
        // Point HOME at a scratch dir so any `~/.cosmon/stand-down.lock`
        // belonging to the developer doesn't affect the test.
        .env("HOME", home.path())
        .output()
        .expect("spawn cosmon-scheduler");

    assert!(
        output.status.success(),
        "dry-run should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Every patrol in the sample must appear in the output.
    for name in [
        "chronicle-lint-weekly",
        "executor-pulse",
        "executor-pulse-replies",
        "legacy-noop",
    ] {
        assert!(
            stdout.contains(name),
            "missing {name} in dry-run output:\n{stdout}"
        );
    }

    // `executor-pulse` (interval, fresh state) must FIRE.
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("executor-pulse") && l.contains("FIRE") && !l.contains("replies")),
        "executor-pulse should FIRE on fresh state:\n{stdout}"
    );

    // `legacy-noop` is disabled ⇒ must be a SKIP with reason `disabled`.
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("legacy-noop") && l.contains("disabled")),
        "legacy-noop should skip with reason 'disabled':\n{stdout}"
    );

    // `executor-pulse-replies` requires an unset env var ⇒ SKIP.
    assert!(
        stdout.lines().any(|l| l.contains("executor-pulse-replies")
            && l.contains("required env var PATROL_REPLIES_READY")),
        "executor-pulse-replies should skip for missing env var:\n{stdout}"
    );

    // Dry-run must NOT create the state file.
    let state_path = home.path().join(".cosmon").join("scheduler.state.json");
    assert!(
        !state_path.exists(),
        "dry-run must not write state.json: {} exists",
        state_path.display()
    );
}

#[test]
fn dry_run_rejects_malformed_config() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad.toml");
    fs::write(
        &bad,
        r#"
        [scheduler]
        state_file = "s"
        log_file = "l"
        kill_switch = "k"
        tick_interval_seconds = 60

        [[patrol]]
        name = "typo"
        intervall_seconds = 300
        command = ["echo"]
        "#,
    )
    .unwrap();

    let output = Command::new(scheduler_bin())
        .arg("tick")
        .arg("--dry-run")
        .arg("--config")
        .arg(&bad)
        .env("HOME", tmp.path())
        .output()
        .expect("spawn cosmon-scheduler");

    assert!(!output.status.success(), "malformed config must fail");
    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 2, "expected exit code 2 for config error");
}

/// Real dispatch fires a patrol
/// and records `last_fired_at` + `fire_count` in `state.json` atomically.
#[test]
fn real_dispatch_spawns_patrol_and_records_state() {
    let home = tempfile::tempdir().unwrap();
    let log_path = home.path().join("dispatch.log");
    let state_path = home.path().join("scheduler.state.json");

    // A tiny config: one `dispatch = "wait"` patrol that runs `echo`.
    // Wait mode lets us assert exit_code = 0 deterministically before
    // the test process exits.
    let cfg_path = home.path().join("patrols.toml");
    fs::write(
        &cfg_path,
        format!(
            r#"
            [scheduler]
            state_file            = {state:?}
            log_file              = {log:?}
            kill_switch           = "/tmp/cosmon-scheduler-never-exists-{rand}"
            tick_interval_seconds = 60

            [[patrol]]
            name             = "smoke-echo"
            interval_seconds = 60
            command          = ["echo", "real-dispatch-smoke"]
            dispatch         = "wait"
            "#,
            state = state_path.to_string_lossy(),
            log = log_path.to_string_lossy(),
            rand = std::process::id(),
        ),
    )
    .unwrap();

    let output = Command::new(scheduler_bin())
        .arg("tick")
        .arg("--config")
        .arg(&cfg_path)
        .env("HOME", home.path())
        .output()
        .expect("spawn cosmon-scheduler");

    assert!(
        output.status.success(),
        "real dispatch should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The log file must contain the echoed line.
    let log_contents = fs::read_to_string(&log_path).expect("log must exist");
    assert!(
        log_contents.contains("real-dispatch-smoke"),
        "patrol stdout not captured in log: {log_contents}"
    );

    // state.json must exist and carry the fire record.
    let state_raw = fs::read_to_string(&state_path).expect("state.json must exist");
    let state: Value = serde_json::from_str(&state_raw).expect("state is json");
    let entry = state
        .get("patrols")
        .and_then(|p| p.get("smoke-echo"))
        .expect("patrols.smoke-echo present");
    assert!(
        entry.get("last_fired_at").and_then(Value::as_str).is_some(),
        "last_fired_at not recorded: {state_raw}"
    );
    assert_eq!(
        entry.get("last_exit_code").and_then(Value::as_i64),
        Some(0),
        "wait-mode exit_code must be 0: {state_raw}"
    );
    assert_eq!(
        entry.get("fire_count").and_then(Value::as_u64),
        Some(1),
        "fire_count must increment: {state_raw}"
    );
}

#[test]
fn real_dispatch_is_idempotent_within_interval() {
    // Two back-to-back ticks must fire once, not twice: the second tick
    // observes last_fired_at from the first and skips.
    let home = tempfile::tempdir().unwrap();
    let log_path = home.path().join("dispatch.log");
    let state_path = home.path().join("scheduler.state.json");

    let cfg_path = home.path().join("patrols.toml");
    fs::write(
        &cfg_path,
        format!(
            r#"
            [scheduler]
            state_file            = {state:?}
            log_file              = {log:?}
            kill_switch           = "/tmp/cosmon-scheduler-never-exists-{rand}"
            tick_interval_seconds = 60

            [[patrol]]
            name             = "once-per-interval"
            interval_seconds = 3600
            command          = ["echo", "fired"]
            dispatch         = "wait"
            "#,
            state = state_path.to_string_lossy(),
            log = log_path.to_string_lossy(),
            rand = std::process::id(),
        ),
    )
    .unwrap();

    for i in 0..2 {
        let output = Command::new(scheduler_bin())
            .arg("tick")
            .arg("--config")
            .arg(&cfg_path)
            .env("HOME", home.path())
            .output()
            .unwrap_or_else(|_| panic!("spawn tick #{i}"));
        assert!(
            output.status.success(),
            "tick #{i} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let state_raw = fs::read_to_string(&state_path).expect("state.json exists");
    let state: Value = serde_json::from_str(&state_raw).unwrap();
    let count = state
        .get("patrols")
        .and_then(|p| p.get("once-per-interval"))
        .and_then(|e| e.get("fire_count"))
        .and_then(Value::as_u64)
        .expect("fire_count present");
    assert_eq!(count, 1, "second tick must not re-fire inside interval");
}
