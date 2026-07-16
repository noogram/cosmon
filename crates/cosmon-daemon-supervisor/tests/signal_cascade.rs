// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test: signal cascade (SIGTERM → SIGKILL grace).
//!
//! Spawns a real child that installs `signal.SIG_IGN` for SIGTERM via
//! Python so the signal is unambiguously ignored (far more reliable than
//! `trap '' TERM` in `/bin/sh`, whose behavior varies across macOS bash
//! and Linux dash). Runs the supervisor's `shutdown()` path and asserts:
//!
//! 1. The child receives SIGTERM and keeps running (because it ignores it).
//! 2. After the grace window elapses, the supervisor escalates to SIGKILL
//!    and the child actually dies.
//! 3. The final state reflects the child as `Exited`.

use std::fs;
use std::time::Duration;

use cosmon_daemon_supervisor::adapters::tokio_process::pid_is_alive;
use cosmon_daemon_supervisor::{ChildStatus, Supervisor};

fn which(bin: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|p| std::path::PathBuf::from(p).join(bin))
        .find(|p| p.exists())
}

/// Resolve a fast, concrete `python3` interpreter, **bypassing any pyenv
/// shim** on `PATH`.
///
/// The grace-escalation contract this test asserts has a hidden race against
/// interpreter startup: the child must reach `signal.signal(SIGTERM, SIG_IGN)`
/// *before* the supervisor's `shutdown()` delivers SIGTERM (the test waits
/// only ~300 ms first). A bare `which("python3")` resolves to
/// `~/.pyenv/shims/python3`, whose `pyenv exec` version-resolution adds 2–5 s
/// of startup during which the shim — not yet the real interpreter — still
/// runs the *default* SIGTERM disposition. SIGTERM then kills it instantly,
/// `shutdown()` returns in ~50 ms, and the `>= 4 s` escalation assertion
/// fails deterministically on any pyenv machine (it passes on CI where
/// `python3` is `/usr/bin/python3`). Resolving the shim to its underlying
/// interpreter closes the race. Same root cause as the cosmon-runtime
/// resident-loop stubs: a pyenv shim that delays the real interpreter.
///
/// Resolution order (first hit wins): `$COSMON_TEST_PYTHON`, then
/// `pyenv which python3` (unwraps a shim), then well-known absolute
/// interpreters, then the original `PATH` walk as a last resort.
fn python_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("COSMON_TEST_PYTHON") {
        let p = std::path::PathBuf::from(p);
        if p.exists() {
            return p;
        }
    }
    if let Ok(out) = std::process::Command::new("pyenv")
        .args(["which", "python3"])
        .output()
    {
        if out.status.success() {
            let p =
                std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
            if p.exists() {
                return p;
            }
        }
    }
    for cand in [
        "/usr/bin/python3",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
    ] {
        let p = std::path::PathBuf::from(cand);
        if p.exists() {
            return p;
        }
    }
    which("python3")
        .or_else(|| which("python"))
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/bin/python3"))
}

fn write_config(
    dir: &std::path::Path,
    name: &str,
    binary: &std::path::Path,
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
        binary = binary.display(),
    );
    let path = dir.join("daemons.toml");
    fs::write(&path, cfg).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_escalates_sigterm_ignore_to_sigkill() {
    let tmp = tempfile::tempdir().unwrap();
    let py = python_bin();
    if !py.exists() {
        eprintln!("skipping: no python3 available");
        return;
    }
    // The child installs SIG_IGN for SIGTERM, then *touches a readiness
    // sentinel* (`argv[1]`) so the test can wait for the handler to be in
    // place — not merely for the pid to exist. A fixed sleep here is racy:
    // under heavy parallel load (the whole `cargo test --workspace` run)
    // python's interpreter startup can exceed any constant we pick. If
    // SIGTERM is delivered before `signal.signal(...)` executes, the child
    // dies with the *default* disposition, `shutdown()` legitimately
    // returns fast, and the `elapsed >= 4s` assertion flakes — exactly the
    // ~54ms false failure this test was misdiagnosing as a supervisor bug.
    let ready = tmp.path().join("stubborn.ready");
    let ready_arg = ready.to_string_lossy().into_owned();
    let script = "import signal,sys,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);open(sys.argv[1],'w').close();time.sleep(600)";
    let config_path = write_config(tmp.path(), "stubborn", &py, &["-c", script, &ready_arg]);

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
    // Default SIGTERM behavior (exit fast) — /bin/sleep honors SIGTERM.
    let sleep_bin = std::path::PathBuf::from("/bin/sleep");
    let config_path = write_config(tmp.path(), "polite", &sleep_bin, &["600"]);

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
