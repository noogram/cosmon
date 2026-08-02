// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: signal cascade (SIGTERM → SIGKILL grace).
//!
//! Spawns a real child that sets SIGTERM to `SIG_IGN` via the POSIX shell
//! builtin `trap '' TERM`, so the signal is unambiguously ignored. Runs the
//! supervisor's `shutdown()` path and asserts:
//!
//! 1. The child receives SIGTERM and keeps running (because it ignores it).
//! 2. After the grace window elapses, the supervisor escalates to SIGKILL
//!    and the child actually dies.
//! 3. The final state reflects the child as `Exited`.

use std::fs;
use std::time::Duration;

use cosmon_daemon_supervisor::adapters::tokio_process::pid_is_alive;
use cosmon_daemon_supervisor::{ChildStatus, Supervisor};

/// The SIGTERM-ignoring child, as a POSIX shell one-liner.
///
/// `trap '' TERM` sets the *disposition* to `SIG_IGN` — not a handler whose
/// execution a shell may defer until the current foreground command returns
/// — so the shell and everything it forks genuinely ignore SIGTERM. `$1` is
/// a readiness sentinel the test waits on: the file appears only once the
/// trap is installed, which is what makes the escalation assertion below a
/// statement about the supervisor rather than about child startup latency.
///
/// The busy-wait is a loop of one-second sleeps rather than one long sleep
/// on purpose: when the supervisor finally escalates to SIGKILL it reaps the
/// shell, and any `sleep` grandchild is orphaned for whatever remains of its
/// own timeout. One second of orphan is cheap; ten minutes is a leak.
///
/// This replaced a Python stub, which cost a whole interpreter on `PATH` (a
/// pyenv shim's 2–5 s startup used to race the 300 ms pre-shutdown wait, and
/// a BusyBox image has no interpreter at all). `sh` is the one program POSIX
/// guarantees, and it starts in single-digit milliseconds.
const SIGTERM_IGNORING_CHILD: &str = "trap '' TERM; : > \"$1\"; while : ; do sleep 1; done";

fn write_config(
    dir: &std::path::Path,
    name: &str,
    binary: &str,
    args: &[&str],
) -> std::path::PathBuf {
    let args_toml = args
        .iter()
        .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let cfg = format!(
        r#"
[supervisor]
state_file = "{state}"
log_file = "{log}"
kill_switch = "{ks}"

[[daemon]]
name = "{name}"
binary = "{binary}"
args = [{args_toml}]
throttle_seconds = 0
enabled = true
"#,
        state = dir.join("state.json").display(),
        log = dir.join("supervisor.log").display(),
        ks = dir.join("kill.lock").display(),
        name = name,
        binary = binary,
    );
    let path = dir.join("daemons.toml");
    fs::write(&path, cfg).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_escalates_sigterm_ignore_to_sigkill() {
    let tmp = tempfile::tempdir().unwrap();
    // The child ignores SIGTERM, then *touches a readiness sentinel* (`$1`)
    // so the test can wait for the trap to be in place — not merely for the
    // pid to exist. A fixed sleep here is racy: under heavy parallel load
    // (the whole `cargo test --workspace` run) child startup can exceed any
    // constant we pick. If SIGTERM is delivered before `trap` executes, the
    // child dies with the *default* disposition, `shutdown()` legitimately
    // returns fast, and the `elapsed >= 4s` assertion flakes — exactly the
    // ~54ms false failure this test was misdiagnosing as a supervisor bug.
    let ready = tmp.path().join("stubborn.ready");
    let ready_arg = ready.to_string_lossy().into_owned();
    // `sh -c <script> <argv0> <argv1>`: the first operand after the script
    // becomes `$0`, so a filler is needed for the sentinel to land in `$1`.
    let config_path = write_config(
        tmp.path(),
        "stubborn",
        "sh",
        &["-c", SIGTERM_IGNORING_CHILD, "stubborn-child", &ready_arg],
    );

    let mut supervisor = Supervisor::new(
        config_path,
        &tmp.path().join("state.json"),
        tmp.path().join("kill.lock"),
    )
    .expect("new supervisor");

    supervisor.step_once().expect("initial step");

    // Wait until the SIGTERM handler is actually installed (readiness
    // sentinel present), not just until the process exists. Generous cap
    // (12 s) so a cold, loaded interpreter still gets there deterministically.
    let mut ready_seen = false;
    for _ in 0..600 {
        if ready.exists() {
            ready_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        ready_seen,
        "child never signalled that its SIGTERM SIG_IGN handler was installed"
    );

    let pid = supervisor
        .snapshot()
        .into_iter()
        .find(|(n, _, _)| n == "stubborn")
        .and_then(|(_, _, pid)| pid)
        .expect("pid recorded");
    assert!(pid_is_alive(pid), "child should be alive before shutdown");

    let start = std::time::Instant::now();
    supervisor.shutdown().await.expect("shutdown");
    let elapsed = start.elapsed();

    // Shutdown must have taken at least ~grace (SIGTERM ignored), but
    // not hung indefinitely (< grace + 3s overhead).
    assert!(
        elapsed >= Duration::from_secs(4),
        "shutdown returned too quickly — did we escalate? elapsed: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "shutdown took too long: {elapsed:?}"
    );

    // Reap race: the OS may not have finalized the exit status yet even
    // though the process is no longer in the process table. Give it a
    // moment.
    for _ in 0..50 {
        if !pid_is_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !pid_is_alive(pid),
        "child pid {pid} is still alive after shutdown"
    );

    let snap = supervisor.snapshot();
    let rec = snap.iter().find(|(n, _, _)| n == "stubborn").unwrap();
    assert_eq!(rec.1, ChildStatus::Exited);
    assert_eq!(rec.2, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_polite_child_terminates_before_grace() {
    let tmp = tempfile::tempdir().unwrap();
    // Default SIGTERM behavior (exit fast) — `sleep` honors SIGTERM. The
    // binary is named, not pathed: BusyBox ships it at `/bin/sleep`, GNU
    // coreutils at `/usr/bin/sleep`, and the spawn port resolves via PATH.
    let config_path = write_config(tmp.path(), "polite", "sleep", &["600"]);

    let mut supervisor = Supervisor::new(
        config_path,
        &tmp.path().join("state.json"),
        tmp.path().join("kill.lock"),
    )
    .expect("new supervisor");

    supervisor.step_once().expect("initial step");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = supervisor
        .snapshot()
        .into_iter()
        .find(|(n, _, _)| n == "polite")
        .and_then(|(_, _, pid)| pid)
        .expect("pid recorded");
    assert!(pid_is_alive(pid));

    let start = std::time::Instant::now();
    supervisor.shutdown().await.expect("shutdown");
    let elapsed = start.elapsed();

    // Should return well before the 5 s grace window.
    assert!(
        elapsed < Duration::from_secs(3),
        "polite shutdown should be fast, got: {elapsed:?}"
    );

    for _ in 0..50 {
        if !pid_is_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!pid_is_alive(pid));
}
