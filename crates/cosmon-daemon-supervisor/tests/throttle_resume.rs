// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: throttle resume across supervisor restart.
//!
//! Regression coverage for a throttle-resume bug. A fresh child
//! added via hot-reload enters the `Exited` branch, is moved to
//! `Throttling` with an in-memory `throttle_until`, and is persisted to
//! `state.json` *without* `throttle_until` (the field is deliberately
//! transient — see `PersistedChild` in `ports.rs`). If the supervisor
//! restarts before the throttle window elapses, the reloaded child has
//! `status = Throttling` and `throttle_until = None`. The old fallback
//! `unwrap_or_else(|| throttle_deadline(spec, now))` always anchored
//! the deadline on the *restart's* `now`, so the child stayed throttling
//! forever — one second in the future, every tick, for the lifetime of
//! the supervisor.
//!
//! The fix re-anchors the fallback on `last_exit_at` (or `now` when
//! neither is persisted, i.e. "freshly reloaded, no exit history"), so
//! the throttle either elapses correctly or the child spawns
//! immediately. This test proves the latter path: a child whose
//! `state.json` says `Throttling + last_exit_at = null` must spawn on
//! the next `step_once`, not linger.

use std::fs;
use std::path::Path;
use std::time::Duration;

use cosmon_daemon_supervisor::{ChildStatus, Supervisor};

fn write_config(dir: &Path) -> std::path::PathBuf {
    let cfg = format!(
        r#"
[supervisor]
state_file = "{state}"
log_file = "{log}"
kill_switch = "{ks}"

[[daemon]]
name = "stuck-after-restart"
binary = "/bin/sleep"
args = ["600"]
throttle_seconds = 30
enabled = true
"#,
        state = dir.join("state.json").display(),
        log = dir.join("supervisor.log").display(),
        ks = dir.join("kill.lock").display(),
    );
    let path = dir.join("daemons.toml");
    fs::write(&path, cfg).unwrap();
    path
}

/// Write a pre-seeded `state.json` that looks like a supervisor restart
/// caught the child mid-throttle: `status = Throttling`, no
/// `last_exit_at`, no `throttle_until` (never persisted).
fn seed_stuck_state(dir: &Path) {
    let state = serde_json::json!({
        "version": 1,
        "children": {
            "stuck-after-restart": {
                "name": "stuck-after-restart",
                "status": "throttling",
                "pid": null,
                "last_exit_code": null,
                "last_spawn_at": null,
                "last_exit_at": null,
                "respawn_count": 0,
            }
        }
    });
    fs::write(
        dir.join("state.json"),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn throttling_child_with_no_exit_history_respawns_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path());
    seed_stuck_state(tmp.path());

    let mut supervisor = Supervisor::new(
        config_path,
        &tmp.path().join("state.json"),
        tmp.path().join("kill.lock"),
    )
    .expect("new supervisor");

    // Single step_once must move the child from Throttling → Running.
    // Before the fix, throttle_until=None + last_exit_at=None fell back
    // to `now + 30s`, and `step_once` left the child throttling.
    supervisor.step_once().expect("step");

    // Give the OS a moment to record the pid in the process table.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let snap = supervisor.snapshot();
    let rec = snap
        .iter()
        .find(|(n, _, _)| n == "stuck-after-restart")
        .expect("record present");
    assert_eq!(
        rec.1,
        ChildStatus::Running,
        "child should have respawned after restart, not lingered in Throttling"
    );
    assert!(rec.2.is_some(), "pid should be set after respawn");

    // Clean up the sleep child so the test doesn't leak a process.
    supervisor.shutdown().await.expect("shutdown");
}
