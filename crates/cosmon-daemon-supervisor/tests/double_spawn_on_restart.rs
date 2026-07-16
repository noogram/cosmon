// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: R3 mitigation — double-spawn on supervisor restart.
//!
//! Scenario:
//! 1. Start supervisor, spawn a child, persist state with its pid.
//! 2. Abandon the supervisor *without* killing the child (simulating a
//!    supervisor crash while the `LaunchAgent` restart is still pending).
//! 3. Start a fresh supervisor with the same config + state path.
//! 4. Assert: the new supervisor inherits the existing pid (no second
//!    spawn) — the child is counted as `Running`.
//!
//! The underlying mechanism is the `pid_is_alive(pid)` probe in
//! [`cosmon_daemon_supervisor::adapters::tokio_process::pid_is_alive`] —
//! if the recorded pid still refers to a live process, the new supervisor
//! reuses it rather than spawning a duplicate.

use std::fs;
use std::time::Duration;

use cosmon_daemon_supervisor::adapters::tokio_process::pid_is_alive;
use cosmon_daemon_supervisor::{ChildStatus, Supervisor};

use nix::sys::signal::{self as sig, Signal as NixSig};
use nix::unistd::Pid;

fn write_config(dir: &std::path::Path, name: &str, script: &str) -> std::path::PathBuf {
    let cfg = format!(
        r#"
[supervisor]
state_file = "{state}"
log_file = "{log}"
kill_switch = "{ks}"

[[daemon]]
name = "{name}"
binary = "/bin/sh"
args = ["-c", "{script}"]
throttle_seconds = 0
enabled = true
"#,
        state = dir.join("state.json").display(),
        log = dir.join("supervisor.log").display(),
        ks = dir.join("kill.lock").display(),
        name = name,
        script = script.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let path = dir.join("daemons.toml");
    fs::write(&path, cfg).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_reuses_live_child_pid() {
    let tmp = tempfile::tempdir().unwrap();
    // Long-running, doesn't care about signals we don't send.
    let script = "while :; do sleep 10; done";
    let config_path = write_config(tmp.path(), "resident", script);
    let state_path = tmp.path().join("state.json");
    let kill_switch = tmp.path().join("kill.lock");

    // First supervisor incarnation.
    let initial_pid = {
        let mut s1 =
            Supervisor::new(config_path.clone(), &state_path, kill_switch.clone()).expect("new s1");
        s1.step_once().expect("step");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pid = s1
            .snapshot()
            .into_iter()
            .find(|(n, _, _)| n == "resident")
            .and_then(|(_, _, pid)| pid)
            .expect("pid");
        assert!(pid_is_alive(pid), "child should be alive");

        // Persist before "crashing" (drop supervisor without calling shutdown).
        s1.persist().expect("persist");
        pid
    };

    // Verify state on disk.
    let raw = fs::read_to_string(&state_path).unwrap();
    assert!(raw.contains("resident"));
    assert!(raw.contains(&initial_pid.to_string()));

    // Child should still be alive (we dropped s1, which does NOT kill
    // because kill_on_drop = false).
    assert!(
        pid_is_alive(initial_pid),
        "abandoned child must survive supervisor drop"
    );

    // Second supervisor incarnation — same config + state path.
    let mut s2 = Supervisor::new(config_path, &state_path, kill_switch).expect("new s2");

    // Before any step, the snapshot should already show the inherited pid.
    let snap = s2.snapshot();
    let rec = snap
        .iter()
        .find(|(n, _, _)| n == "resident")
        .expect("resident in snap");
    assert_eq!(rec.1, ChildStatus::Running, "inherited pid must be Running");
    assert_eq!(
        rec.2,
        Some(initial_pid),
        "pid must be reused, not respawned"
    );

    // step_once must NOT spawn a duplicate (the pid is alive and counted
    // as Running). We can verify by checking the snapshot again.
    s2.step_once().expect("step s2");
    let snap2 = s2.snapshot();
    let rec2 = snap2.iter().find(|(n, _, _)| n == "resident").unwrap();
    assert_eq!(
        rec2.2,
        Some(initial_pid),
        "step_once must not replace the inherited pid"
    );

    // Clean up: SIGKILL the child so the test harness isn't left with an
    // orphan sleep. We don't use shutdown() here because it reaps via the
    // internal Child handle, and s2 never owned this child.
    let pid_i32 = i32::try_from(initial_pid).expect("pid fits in i32");
    let _ = sig::kill(Pid::from_raw(pid_i32), NixSig::SIGKILL);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_respawns_if_pid_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "exit 0"; // child exits immediately
    let config_path = write_config(tmp.path(), "transient", script);
    let state_path = tmp.path().join("state.json");
    let kill_switch = tmp.path().join("kill.lock");

    // Seed state file as if a prior supervisor left a stale pid.
    let stale_state = r#"{
        "version": 1,
        "children": {
            "transient": {
                "name": "transient",
                "status": "running",
                "pid": 999999999,
                "last_exit_code": null,
                "last_spawn_at": null,
                "last_exit_at": null,
                "respawn_count": 7
            }
        }
    }"#;
    fs::write(&state_path, stale_state).unwrap();

    let mut s = Supervisor::new(config_path, &state_path, kill_switch).expect("new");

    // Before stepping: stale pid must be rejected.
    let snap = s.snapshot();
    let rec = snap.iter().find(|(n, _, _)| n == "transient").unwrap();
    assert_ne!(rec.2, Some(999_999_999), "stale pid must not be inherited");
    assert_ne!(rec.1, ChildStatus::Running);

    // step_once may spawn a fresh child.
    s.step_once().expect("step");
}
