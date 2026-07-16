// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: the **crash-loop escape valve**.
//!
//! # The bug this pins
//!
//! The supervisor's `Exited → throttle → SpawnNow` respawn policy is
//! *correct* — it keeps a crashed child coming back. But "forever respawning
//! a child that crashes on every boot" is **silent give-up dressed as
//! diligence**: nothing the operator watches ever fires. That is the
//! failure mode (ADR-053 ~:220) — *a missing event nothing watches for*. A daemon whose
//! config parses but is semantically broken (or whose binary is missing)
//! crash-loops all night with no signal.
//!
//! # The fix
//!
//! After K crash-restarts of one child inside a rolling window, the
//! supervisor shells out to its configured `notify_command` (default
//! `cs notify`) to surface a `PropulsionDown` alert on the operator-visible
//! channel. Here we point `notify_command` at a recorder script and drive a
//! missing-binary daemon (the cheapest reproducer of "spawn fails forever")
//! through `step_once` until the valve trips, then assert exactly one alert
//! landed — the latch makes it fire once per episode, not once per crash.

use std::fs;
use std::path::Path;

use cosmon_daemon_supervisor::Supervisor;

/// Write a recorder script that appends its argv to `marker` on each call,
/// plus a supervisor config whose `notify_command` invokes it. The single
/// daemon points at a missing binary so every spawn fails (exit 127) — a
/// deterministic crash with no process-exit race to wait on.
fn write_config(dir: &Path, marker: &Path) -> std::path::PathBuf {
    let recorder = dir.join("recorder.sh");
    fs::write(
        &recorder,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\n",
            marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&recorder).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&recorder, perms).unwrap();
    }

    let cfg = format!(
        r#"
[supervisor]
state_file = "{state}"
log_file = "{log}"
kill_switch = "{ks}"
crash_loop_threshold = 3
crash_loop_window_seconds = 300
notify_command = ["{recorder}"]

[[daemon]]
name = "doomed-runtime"
binary = "/no/such/binary/cosmon-propulsion-test-{pid}"
args = []
throttle_seconds = 0
enabled = true
"#,
        state = dir.join("state.json").display(),
        log = dir.join("supervisor.log").display(),
        ks = dir.join("kill.lock").display(),
        recorder = recorder.display(),
        pid = std::process::id(),
    );
    let path = dir.join("daemons.toml");
    fs::write(&path, cfg).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_loop_emits_one_propulsion_down_alert() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("propulsion-down.log");
    let config_path = write_config(tmp.path(), &marker);

    let mut supervisor = Supervisor::new(
        config_path,
        &tmp.path().join("state.json"),
        tmp.path().join("kill.lock"),
    )
    .expect("new supervisor");

    // With throttle = 0 and a missing binary, each step_once is exactly one
    // crash-restart. Threshold is 3, so by the third step the valve trips.
    // We run six steps to also prove the alert is *latched* (fires once per
    // episode, not once per crash past the threshold).
    for _ in 0..6 {
        supervisor
            .step_once()
            .expect("step_once stays non-fatal through a crash loop");
    }

    let body = fs::read_to_string(&marker).expect("the PropulsionDown alert must have landed");
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();

    assert_eq!(
        lines.len(),
        1,
        "exactly one alert per crash-loop episode (latched), got {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.contains("PROPULSION DOWN"),
        "alert body must name the failure, got: {line}"
    );
    assert!(
        line.contains("doomed-runtime"),
        "alert must name the crash-looping daemon, got: {line}"
    );
    assert!(
        line.contains("--title PropulsionDown"),
        "alert must carry the PropulsionDown title for the notify channel, got: {line}"
    );
    assert!(
        line.contains("--level alert"),
        "alert must be dispatched at alert level, got: {line}"
    );

    supervisor.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn below_threshold_stays_silent() {
    // Two crashes with a threshold of 3 must NOT alert — the valve only
    // fires for a genuine loop, not an isolated restart.
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("propulsion-down.log");
    let config_path = write_config(tmp.path(), &marker);

    let mut supervisor = Supervisor::new(
        config_path,
        &tmp.path().join("state.json"),
        tmp.path().join("kill.lock"),
    )
    .expect("new supervisor");

    // Only two crash-restarts.
    for _ in 0..2 {
        supervisor.step_once().expect("step_once non-fatal");
    }

    assert!(
        !marker.exists() || fs::read_to_string(&marker).unwrap().trim().is_empty(),
        "no alert below the crash-loop threshold"
    );

    supervisor.shutdown().await.expect("shutdown");
}
