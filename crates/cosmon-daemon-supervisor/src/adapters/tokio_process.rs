// SPDX-License-Identifier: AGPL-3.0-only

//! `tokio::process`-backed implementation of [`crate::ports::ProcessPort`].
//!
//! ## Design choices
//!
//! - **No `kill_on_drop`.** The supervisor may exit for any reason — orderly
//!   `SIGTERM`, OOM-kill, panic — and we *never* want that to cascade into
//!   SIGKILLs for the managed children. Dropping a [`tokio::process::Child`]
//!   with `kill_on_drop = false` (the tokio default when not explicitly set)
//!   releases the handle but leaves the child running. On macOS it is then
//!   reparented to launchd. This is the same discipline `cosmon-scheduler`
//!   uses in `dispatch::drop_detached` (see its module docs) so the two
//!   resident daemons behave identically.
//!
//! - **Signals via `nix`.** Tokio's `Child::kill` sends SIGKILL only; there
//!   is no built-in SIGTERM helper. Using `nix::sys::signal::kill` keeps the
//!   crate `#![forbid(unsafe_code)]` (nix wraps `libc::kill` safely) while
//!   giving us the two signals the supervisor actually needs.
//!
//! - **Log redirection.** `log_stdout` / `log_stderr` paths are opened with
//!   `append + create` (same as scheduler), inheriting parent directories
//!   via `fs::create_dir_all`. If the operator didn't declare a log path
//!   we inherit the supervisor's stdio (so they surface in the launchd log
//!   of the supervisor itself — still observable, just less segmented).

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::Stdio;

use chrono::Utc;
use nix::sys::signal::{self as nix_signal, Signal as NixSignal};
use nix::unistd::Pid;
use tokio::process::Command;

use crate::config::DaemonSpec;
use crate::ports::{ProcessError, ProcessPort, ReapOutcome, Signal, SpawnedChild};

/// Real-world [`ProcessPort`] backed by `tokio::process::Command`.
///
/// Owns the [`tokio::process::Child`] handles so that `reap()` can call
/// `try_wait()` on them non-blockingly. Spawning inserts into the map,
/// reaping on `Exited` / `Signaled` removes. Signals go straight to the
/// OS via `nix::sys::signal::kill` — the child handle isn't required
/// for that path, which matters because the handle might have been
/// consumed by a prior reap.
#[derive(Debug, Default)]
pub struct TokioProcessPort {
    children: HashMap<u32, tokio::process::Child>,
}

impl TokioProcessPort {
    /// Fresh port with an empty child table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live child handles currently owned by the port.
    ///
    /// Useful in integration tests that want to wait for every
    /// SIGTERM'd child to be reaped.
    #[must_use]
    pub fn alive_count(&self) -> usize {
        self.children.len()
    }

    /// `true` iff the port is still tracking a child with this pid.
    ///
    /// Integration tests use this to decide whether to keep polling
    /// `reap()` after a SIGTERM cascade.
    #[must_use]
    pub fn has_child(&self, pid: u32) -> bool {
        self.children.contains_key(&pid)
    }
}

fn open_log(path: &str) -> Result<std::fs::File, ProcessError> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| {
                ProcessError::Os(format!("create log parent {}: {e}", parent.display()))
            })?;
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| ProcessError::Os(format!("open log {path}: {e}")))
}

fn stdio_for(log: Option<&str>) -> Result<Stdio, ProcessError> {
    match log {
        Some(path) => Ok(Stdio::from(open_log(path)?)),
        None => Ok(Stdio::inherit()),
    }
}

impl ProcessPort for TokioProcessPort {
    fn spawn(&mut self, spec: &DaemonSpec) -> Result<SpawnedChild, ProcessError> {
        let mut cmd = Command::new(&spec.binary);
        cmd.args(&spec.args);
        cmd.stdin(Stdio::null());
        cmd.stdout(stdio_for(spec.log_stdout.as_deref())?);
        cmd.stderr(stdio_for(spec.log_stderr.as_deref())?);

        // Tokio's default is `kill_on_drop = false`; set explicitly so the
        // intent is locally documented: supervisor exit must not cascade.
        cmd.kill_on_drop(false);

        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = cmd
            .spawn()
            .map_err(|e| ProcessError::Os(format!("spawn '{}': {e}", spec.binary)))?;

        let pid = child
            .id()
            .ok_or_else(|| ProcessError::Os("spawned child has no pid".to_owned()))?;

        self.children.insert(pid, child);
        Ok(SpawnedChild {
            pid,
            started_at: Utc::now(),
        })
    }

    fn signal(&mut self, pid: u32, signal: Signal) -> Result<(), ProcessError> {
        let nix_sig = match signal {
            Signal::Term => NixSignal::SIGTERM,
            Signal::Kill => NixSignal::SIGKILL,
        };
        let pid_i32 = i32::try_from(pid)
            .map_err(|_| ProcessError::Os(format!("pid {pid} does not fit in i32")))?;
        // We don't require ownership of the Child handle — the OS cares
        // about pids, and on restart we may need to signal processes we
        // didn't spawn ourselves.
        match nix_signal::kill(Pid::from_raw(pid_i32), nix_sig) {
            Ok(()) => Ok(()),
            Err(nix::errno::Errno::ESRCH) => {
                // No such process — the pid we tracked is already gone.
                // Remove the handle if we had one so `alive_count` stays
                // honest; the event loop treats this as "already dead".
                self.children.remove(&pid);
                Err(ProcessError::UnknownPid(pid))
            }
            Err(e) => Err(ProcessError::Os(format!("kill({pid},{signal:?}): {e}"))),
        }
    }

    fn reap(&mut self, pid: u32) -> Result<ReapOutcome, ProcessError> {
        let child = self
            .children
            .get_mut(&pid)
            .ok_or(ProcessError::UnknownPid(pid))?;
        match child
            .try_wait()
            .map_err(|e| ProcessError::Os(format!("try_wait({pid}): {e}")))?
        {
            None => Ok(ReapOutcome::Alive),
            Some(status) => {
                self.children.remove(&pid);
                match status.code() {
                    Some(code) => Ok(ReapOutcome::Exited(code)),
                    None => Ok(ReapOutcome::Signaled),
                }
            }
        }
    }
}

/// Probe whether `pid` names a live process without sending a real signal.
///
/// This is the standard `kill(pid, 0)` trick: the OS does all the permission
/// checks and membership lookups but never actually delivers a signal, so
/// observing `Ok(())` is equivalent to "there exists a process with this pid".
/// Used on supervisor restart to detect orphans left over from a prior
/// incarnation (R3 mitigation — see `tests/double_spawn_on_restart.rs`).
///
/// Returns `false` on `ESRCH` (pid not found) and `false` on any other error
/// (permission denied counts as "not ours to worry about").
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    let Ok(pid_i32) = i32::try_from(pid) else {
        return false;
    };
    matches!(nix_signal::kill(Pid::from_raw(pid_i32), None), Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn spec_for(bin: &str, args: &[&str]) -> DaemonSpec {
        DaemonSpec {
            name: "t".into(),
            binary: bin.into(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            throttle_seconds: 1,
            env: BTreeMap::new(),
            log_stdout: None,
            log_stderr: None,
            kill_switch: None,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn spawn_true_exits_cleanly() {
        let mut port = TokioProcessPort::new();
        let spawned = port.spawn(&spec_for("/usr/bin/true", &[])).expect("spawn");
        assert!(spawned.pid > 0);

        // The child may exit before the first reap; poll up to 2s.
        for _ in 0..200 {
            match port.reap(spawned.pid).expect("reap") {
                ReapOutcome::Alive => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                ReapOutcome::Exited(0) => return,
                other => panic!("unexpected outcome: {other:?}"),
            }
        }
        panic!("/usr/bin/true never exited");
    }

    #[tokio::test]
    async fn signal_term_then_kill_escalates() {
        // `sleep 60` would outlast the test; we SIGTERM it immediately.
        let mut port = TokioProcessPort::new();
        let spawned = port
            .spawn(&spec_for("/bin/sleep", &["60"]))
            .expect("spawn sleep");
        let pid = spawned.pid;

        // Polite termination — should exit quickly.
        port.signal(pid, Signal::Term).expect("sigterm");

        // Poll up to 2s for reap. `sleep` on macOS/Linux both honor SIGTERM.
        for _ in 0..200 {
            match port.reap(pid).expect("reap") {
                ReapOutcome::Alive => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                ReapOutcome::Exited(_) | ReapOutcome::Signaled => return,
            }
        }
        panic!("child did not react to SIGTERM within 2s");
    }

    #[tokio::test]
    async fn sigkill_reliably_kills() {
        // Baseline: SIGKILL is guaranteed by POSIX to kill any process;
        // we don't need a SIGTERM-ignoring harness for the "kill works"
        // check. The SIGTERM-trap escalation path is covered end-to-end
        // by `tests/signal_cascade.rs` which uses the full supervisor
        // path with a real grace window.
        let mut port = TokioProcessPort::new();
        let spec = spec_for("/bin/sleep", &["60"]);
        let spawned = port.spawn(&spec).expect("spawn sleep");
        let pid = spawned.pid;

        port.signal(pid, Signal::Kill).expect("sigkill");
        for _ in 0..200 {
            match port.reap(pid).expect("reap") {
                ReapOutcome::Alive => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                ReapOutcome::Exited(_) | ReapOutcome::Signaled => return,
            }
        }
        panic!("child survived SIGKILL");
    }

    #[tokio::test]
    async fn unknown_pid_signal_is_error() {
        let mut port = TokioProcessPort::new();
        // Pid 1 exists but we don't own it — ESRCH won't fire, so this
        // tests a different path: use a pid that definitely doesn't exist.
        // We pick one larger than any realistic pid_max on macOS (99_999).
        let err = port.signal(999_999_999, Signal::Term);
        assert!(err.is_err(), "phantom pid should error");
    }

    #[test]
    fn pid_is_alive_handles_self() {
        // Our own pid is certainly alive.
        let me = std::process::id();
        assert!(pid_is_alive(me));
    }

    #[test]
    fn pid_is_alive_rejects_unused_pid() {
        // On macOS, pid 999_999_999 is vastly above pid_max — ESRCH.
        assert!(!pid_is_alive(999_999_999));
    }

    #[tokio::test]
    async fn log_redirection_writes_to_file() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("out.log");
        let mut spec = spec_for("/bin/sh", &["-c", "echo hello-from-child"]);
        spec.log_stdout = Some(log.to_string_lossy().into_owned());

        let mut port = TokioProcessPort::new();
        let spawned = port.spawn(&spec).expect("spawn");
        // Wait for exit
        for _ in 0..200 {
            if matches!(
                port.reap(spawned.pid).expect("reap"),
                ReapOutcome::Exited(_) | ReapOutcome::Signaled
            ) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let contents = std::fs::read_to_string(&log).unwrap();
        assert!(
            contents.contains("hello-from-child"),
            "log did not capture stdout: {contents}"
        );
    }
}
