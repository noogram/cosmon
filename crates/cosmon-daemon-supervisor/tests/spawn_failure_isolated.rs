// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: a single daemon's spawn failure must not take down
//! the whole supervisor.
//!
//! Regression coverage for a silent exit-and-respawn loop. Before the fix,
//! `step_once` propagated the first failed
//! `tokio::process::Command::spawn()` up through `run()` to `main`, which
//! exited with code 5. launchd's `KeepAlive=true` then respawned the
//! supervisor every 5 s (its `ThrottleInterval`); every healthy child
//! that the previous incarnation had just spawned died as a side effect
//! of the parent process exit, and the supervisor's own `state.json`
//! stopped being updated because `step_once` never reached its final
//! `persist()` call.
//!
//! The fix in `event_loop::Supervisor::spawn_child` swallows per-daemon
//! spawn errors, logs them to stderr, and marks the affected child as
//! `Exited` with `last_exit_code = Some(127)`. The throttle policy then
//! parks it for `spec.throttle_seconds` while every other daemon stays
//! alive. This test pins the new behaviour so it does not regress.

use std::fs;
use std::path::Path;
use std::time::Duration;

use cosmon_daemon_supervisor::{ChildStatus, Supervisor};

/// Two daemons: one with a definitely-missing binary, one with `/bin/sleep`.
///
/// We pick `/no/such/binary/cosmon-supervisor-test` deliberately so the path
/// can never be a partial match for something on the host PATH.
fn write_config(dir: &Path) -> std::path::PathBuf {
    let cfg = format!(
        r#"
[supervisor]
state_file = "{state}"
log_file = "{log}"
kill_switch = "{ks}"

[[daemon]]
name = "broken-binary"
binary = "/no/such/binary/cosmon-supervisor-test-{pid}"
args = []
throttle_seconds = 30
enabled = true

[[daemon]]
name = "healthy-sleeper"
binary = "/bin/sleep"
args = ["600"]
throttle_seconds = 30
enabled = true
"#,
        state = dir.join("state.json").display(),
        log = dir.join("supervisor.log").display(),
        ks = dir.join("kill.lock").display(),
        pid = std::process::id(),
    );
    let path = dir.join("daemons.toml");
    fs::write(&path, cfg).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_binary_does_not_crash_supervisor() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path());

    let mut supervisor = Supervisor::new(
        config_path,
        &tmp.path().join("state.json"),
        tmp.path().join("kill.lock"),
    )
    .expect("new supervisor");

    // step_once must succeed even though `broken-binary` cannot be spawned.
    // Pre-fix: this returned `Err(SupervisorError::Process(...))` and main
    // would exit with code 5, which under a `KeepAlive=true` LaunchAgent
    // turned into a 5-second crash loop and flatlined the supervisor's
    // ability to keep the *healthy* siblings alive.
    supervisor
        .step_once()
        .expect("step_once must succeed despite a per-daemon spawn failure");

    // A second step_once must also remain non-fatal. This is the heart of
    // the regression: the bug repeated every iteration, not just once.
    supervisor
        .step_once()
        .expect("subsequent step_once must remain non-fatal");

    // Give the OS a moment to register the healthy sleeper's pid (if it
    // was due to spawn this pass).
    tokio::time::sleep(Duration::from_millis(200)).await;

    let snap = supervisor.snapshot();

    // The healthy sibling must have a *valid* record — never collateral
    // damage. Whether it is `Throttling` (first-boot grace window — a
    // separate design quirk for fresh children with no exit history) or
    // `Running` is irrelevant to this regression. What matters is that
    // the broken sibling did not delete it from the table or crash the
    // supervisor before it could be evaluated.
    let healthy = snap
        .iter()
        .find(|(n, _, _)| n == "healthy-sleeper")
        .expect("healthy-sleeper record must remain present");
    assert!(
        matches!(
            healthy.1,
            ChildStatus::Throttling | ChildStatus::Running | ChildStatus::Exited
        ),
        "healthy-sleeper should be in a valid state, got {:?}",
        healthy.1
    );

    let broken = snap
        .iter()
        .find(|(n, _, _)| n == "broken-binary")
        .expect("broken-binary record present");
    // Broken: must end up Exited (just-failed) or Throttling (already in
    // its retry window). Either way no pid — we never had a child to
    // hold one.
    assert!(
        matches!(broken.1, ChildStatus::Exited | ChildStatus::Throttling),
        "broken-binary should be Exited or Throttling, got {:?}",
        broken.1
    );
    assert!(
        broken.2.is_none(),
        "broken-binary must not have a pid (the spawn failed)"
    );

    // Clean up any sleep child the supervisor managed to spawn so the
    // test doesn't leak processes.
    supervisor.shutdown().await.expect("shutdown");
}

/// Pure-config edge case: every daemon broken. The supervisor must still
/// stay alive — there is nothing for it to do but throttle each one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_daemons_broken_supervisor_stays_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = format!(
        r#"
[supervisor]
state_file = "{state}"
log_file = "{log}"
kill_switch = "{ks}"

[[daemon]]
name = "broken-one"
binary = "/no/such/binary/one-{pid}"
args = []
throttle_seconds = 5
enabled = true

[[daemon]]
name = "broken-two"
binary = "/no/such/binary/two-{pid}"
args = []
throttle_seconds = 5
enabled = true
"#,
        state = tmp.path().join("state.json").display(),
        log = tmp.path().join("supervisor.log").display(),
        ks = tmp.path().join("kill.lock").display(),
        pid = std::process::id(),
    );
    let config_path = tmp.path().join("daemons.toml");
    fs::write(&config_path, cfg).unwrap();

    let mut supervisor = Supervisor::new(
        config_path,
        &tmp.path().join("state.json"),
        tmp.path().join("kill.lock"),
    )
    .expect("new supervisor");

    // Several iterations must remain non-fatal even when every spawn fails.
    for i in 0..3 {
        supervisor
            .step_once()
            .unwrap_or_else(|e| panic!("step_once #{i} crashed: {e}"));
    }

    // State file must be persisted (the bug also stopped persistence).
    let raw = fs::read_to_string(tmp.path().join("state.json"))
        .expect("state.json must be written even when every spawn fails");
    assert!(
        raw.contains("broken-one") && raw.contains("broken-two"),
        "state.json must contain both daemons, got: {raw}"
    );
}
