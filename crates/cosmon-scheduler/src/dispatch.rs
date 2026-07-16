// SPDX-License-Identifier: AGPL-3.0-only

//! Patrol dispatcher — converts a fired [`Decision::WouldFire`] into an
//! actual subprocess.
//!
//! ## Two dispatch modes
//!
//! - **`dispatch = "detached"`** (default): spawn the child, redirect its
//!   stdout/stderr to the patrol's (or scheduler-wide) log file, do
//!   **not** `wait()`. When the scheduler process exits seconds later,
//!   the child is reparented to `init` (launchd on macOS) and keeps
//!   running. Exit code is recorded as `None`; operators read the log
//!   file for per-run outcome.
//! - **`dispatch = "wait"`**: spawn + block until the child exits, then
//!   record exit code in state. Intended for very short patrols where
//!   the scheduler tick can reasonably afford the blocking wait (e.g.
//!   `true`, `test`, a snappy `cs` invocation).
//!
//! ## Why no `unsafe`
//!
//! We **intentionally** do not call `setsid`/`pre_exec`: `Child::drop`
//! on Unix does not kill the child, so simply not calling `wait()` is
//! enough to leave the process running past the scheduler's lifetime.
//! This preserves the crate's `#![forbid(unsafe_code)]` invariant.
//!
//! ## Logging
//!
//! stdout+stderr go to the patrol's `log_file` (or the scheduler-wide
//! `log_file` if unset). File is opened with `append + create` so the
//! scheduler never truncates operator history. Log rotation is
//! explicitly out of scope (v1 non-goal — use `newsyslog`/`logrotate`,
//! see plan.md).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use thiserror::Error;

use crate::config::{Patrol, SchedulerConfig};
use crate::environment::shellexpand_home;

/// Outcome of a sunset action. `unload_error` is `Some` when the advisory
/// `launchctl unload` failed — callers emit `patrol.sunset_unload_failed`
/// in that case but still record `sunset_decided_at` in state, so the
/// next tick short-circuits instead of looping into a second unload.
#[derive(Debug, Clone, Default)]
pub struct SunsetOutcome {
    /// The plist path (or label) we attempted to unload. `None` if the
    /// patrol has no `launchctl_plist` configured (event emission only).
    pub plist: Option<String>,
    /// `Some(message)` if `launchctl unload` failed or was skipped with
    /// a non-zero exit code.
    pub unload_error: Option<String>,
}

/// Execute the sunset side-effect for a patrol whose convergence rule
/// fired. Advisory: if the unload fails the caller still sets the
/// idempotence flag so the next tick short-circuits.
///
/// # Errors
///
/// Infrastructure errors (e.g. missing `launchctl` binary) are reported
/// via [`SunsetOutcome::unload_error`]. This function returns
/// `SunsetOutcome` unconditionally — a failing unload is *not* a dispatch
/// error, it is a structured outcome that the caller records as an
/// event.
#[must_use]
pub fn run_sunset_action(patrol: &Patrol) -> SunsetOutcome {
    let Some(sunset) = patrol.sunset.as_ref() else {
        return SunsetOutcome::default();
    };
    let Some(plist_raw) = sunset.launchctl_plist.as_deref() else {
        return SunsetOutcome::default();
    };
    let plist = shellexpand_home(plist_raw).into_owned();
    let unload_error = match Command::new("launchctl")
        .arg("unload")
        .arg(&plist)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // macOS quirk: `launchctl unload` of a missing plist exits 0
            // with "Unload failed: …" on stderr. Treat that as a failure
            // so the operator sees a `patrol.sunset_unload_failed` event
            // rather than a silent success.
            let stderr_signals_failure =
                stderr.contains("Unload failed") || stderr.contains("failed");
            if out.status.success() && !stderr_signals_failure {
                None
            } else {
                Some(format!(
                    "launchctl unload exit={}: {}",
                    out.status.code().unwrap_or(-1),
                    stderr.trim()
                ))
            }
        }
        Err(e) => Some(format!("launchctl unload spawn failed: {e}")),
    };
    SunsetOutcome {
        plist: Some(plist),
        unload_error,
    }
}

/// Outcome of a single dispatch call. Fed into [`crate::state::PatrolState`].
#[derive(Debug, Clone, Copy)]
pub struct DispatchOutcome {
    /// OS PID of the spawned child, if available.
    pub pid: Option<u32>,
    /// Exit code, populated only for `dispatch = "wait"`. Signal-terminated
    /// children have no exit code on Unix; we record `None` for those too.
    pub exit_code: Option<i32>,
}

/// Abstracts the "spawn a real subprocess" step so tests can observe
/// dispatch attempts without spawning anything.
pub trait Dispatcher {
    /// Spawn (or pretend to spawn) the patrol. Implementations must not
    /// mutate scheduler state themselves — the caller records outcomes
    /// in [`crate::state::SchedulerState`].
    ///
    /// # Errors
    ///
    /// Returns a [`DispatchError`] if the child could not be spawned,
    /// the log file could not be opened, or the sync `wait` failed.
    fn dispatch(
        &self,
        patrol: &Patrol,
        scheduler: &SchedulerConfig,
    ) -> Result<DispatchOutcome, DispatchError>;
}

/// Errors surfaced by [`Dispatcher::dispatch`].
#[derive(Debug, Error)]
pub enum DispatchError {
    /// The patrol's `command` array was empty. In practice
    /// [`Config::validate`](crate::config::Config::validate) rejects
    /// this at load time, so hitting this variant is a bug.
    #[error("dispatch: patrol '{name}' has empty command")]
    EmptyCommand {
        /// Patrol name for operator diagnostics.
        name: String,
    },

    /// `std::process::Command::spawn` failed — typically binary not on
    /// PATH, not executable, or the working directory is missing.
    #[error("dispatch: patrol '{name}' spawn failed: {source}")]
    Spawn {
        /// Patrol name for operator diagnostics.
        name: String,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// The log file (or scheduler-wide log file) could not be opened
    /// for append.
    #[error("dispatch: patrol '{name}' log-open failed for {path}: {source}")]
    LogOpen {
        /// Patrol name for operator diagnostics.
        name: String,
        /// Path of the log file we tried to open.
        path: String,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// Synchronous `wait()` on a `dispatch = "wait"` child failed.
    #[error("dispatch: patrol '{name}' wait failed: {source}")]
    Wait {
        /// Patrol name for operator diagnostics.
        name: String,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// `dispatch` field was neither `"detached"` nor `"wait"`.
    #[error(
        "dispatch: patrol '{name}' unknown dispatch mode '{mode}' (expected 'detached' or 'wait')"
    )]
    UnknownMode {
        /// Patrol name for operator diagnostics.
        name: String,
        /// The offending mode string from the TOML.
        mode: String,
    },
}

/// Production dispatcher — spawns real OS processes via `std::process`.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessDispatcher;

impl Dispatcher for ProcessDispatcher {
    fn dispatch(
        &self,
        patrol: &Patrol,
        scheduler: &SchedulerConfig,
    ) -> Result<DispatchOutcome, DispatchError> {
        if patrol.command.is_empty() {
            return Err(DispatchError::EmptyCommand {
                name: patrol.name.clone(),
            });
        }

        let log_path_raw = patrol.log_file.as_deref().unwrap_or(&scheduler.log_file);
        let log_path = shellexpand_home(log_path_raw).into_owned();
        let log_file = open_log(&log_path).map_err(|source| DispatchError::LogOpen {
            name: patrol.name.clone(),
            path: log_path.clone(),
            source,
        })?;
        let log_file_stderr = log_file
            .try_clone()
            .map_err(|source| DispatchError::LogOpen {
                name: patrol.name.clone(),
                path: log_path.clone(),
                source,
            })?;

        let mut cmd = Command::new(&patrol.command[0]);
        cmd.args(&patrol.command[1..]);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::from(log_file));
        cmd.stderr(Stdio::from(log_file_stderr));
        apply_working_dir(&mut cmd, patrol.working_dir.as_deref());
        apply_env(&mut cmd, &patrol.env);

        match patrol.dispatch.as_str() {
            "detached" => spawn_detached(&mut cmd, &patrol.name),
            "wait" => spawn_and_wait(&mut cmd, &patrol.name),
            other => Err(DispatchError::UnknownMode {
                name: patrol.name.clone(),
                mode: other.to_owned(),
            }),
        }
    }
}

fn spawn_detached(cmd: &mut Command, name: &str) -> Result<DispatchOutcome, DispatchError> {
    let child = cmd.spawn().map_err(|source| DispatchError::Spawn {
        name: name.to_owned(),
        source,
    })?;
    let pid = child.id();
    // Intentionally drop the Child without waiting — on Unix this does
    // not signal or reap the process; it simply releases the handle.
    // On macOS the child gets reparented to launchd once we exit.
    drop_detached(child);
    Ok(DispatchOutcome {
        pid: Some(pid),
        exit_code: None,
    })
}

/// Separate helper so clippy's `drop_non_drop` does not flag the
/// intentional drop — and so the *intent* is documented locally.
fn drop_detached(_child: Child) {
    // no-op: we want the Child handle released without wait()
}

fn spawn_and_wait(cmd: &mut Command, name: &str) -> Result<DispatchOutcome, DispatchError> {
    let mut child = cmd.spawn().map_err(|source| DispatchError::Spawn {
        name: name.to_owned(),
        source,
    })?;
    let pid = child.id();
    let status = child.wait().map_err(|source| DispatchError::Wait {
        name: name.to_owned(),
        source,
    })?;
    Ok(DispatchOutcome {
        pid: Some(pid),
        exit_code: status.code(),
    })
}

fn open_log(path: &str) -> io::Result<File> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn apply_working_dir(cmd: &mut Command, wd: Option<&str>) {
    if let Some(raw) = wd {
        let expanded = shellexpand_home(raw);
        cmd.current_dir(PathBuf::from(expanded.into_owned()));
    }
}

fn apply_env(cmd: &mut Command, extra: &BTreeMap<String, String>) {
    for (k, v) in extra {
        cmd.env(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler_cfg(log_file: &str) -> SchedulerConfig {
        SchedulerConfig {
            state_file: "unused.json".to_owned(),
            log_file: log_file.to_owned(),
            kill_switch: "unused.lock".to_owned(),
            tick_interval_seconds: 60,
        }
    }

    fn patrol_named(
        name: &str,
        command: Vec<&str>,
        mode: &str,
        log_override: Option<&str>,
    ) -> Patrol {
        Patrol {
            name: name.to_owned(),
            interval_seconds: Some(60),
            cron: None,
            command: command.into_iter().map(str::to_owned).collect(),
            working_dir: None,
            env: BTreeMap::new(),
            kill_switch: None,
            log_file: log_override.map(str::to_owned),
            dispatch: mode.to_owned(),
            require_env: Vec::new(),
            timeout_seconds: None,
            enabled: true,
            sunset: None,
        }
    }

    #[test]
    fn dispatch_wait_mode_records_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("out.log");
        let log_str = log.to_string_lossy().into_owned();

        let patrol = patrol_named("echo-wait", vec!["true"], "wait", Some(&log_str));
        let cfg = scheduler_cfg(&log_str);

        let outcome = ProcessDispatcher.dispatch(&patrol, &cfg).expect("ok");
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.pid.is_some());
    }

    #[test]
    fn dispatch_detached_mode_returns_pid_no_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("out.log");
        let log_str = log.to_string_lossy().into_owned();

        // `true` exits immediately, so detached still succeeds — we just
        // don't wait for it.
        let patrol = patrol_named("echo-detached", vec!["true"], "detached", Some(&log_str));
        let cfg = scheduler_cfg(&log_str);

        let outcome = ProcessDispatcher.dispatch(&patrol, &cfg).expect("ok");
        assert!(outcome.pid.is_some());
        assert!(outcome.exit_code.is_none());
    }

    #[test]
    fn dispatch_writes_command_output_to_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("out.log");
        let log_str = log.to_string_lossy().into_owned();

        let patrol = patrol_named(
            "echo",
            vec!["echo", "dispatch-smoke-line"],
            "wait",
            Some(&log_str),
        );
        let cfg = scheduler_cfg(&log_str);
        ProcessDispatcher.dispatch(&patrol, &cfg).expect("ok");

        let contents = fs::read_to_string(&log).expect("log exists");
        assert!(
            contents.contains("dispatch-smoke-line"),
            "log did not capture stdout: {contents}"
        );
    }

    #[test]
    fn unknown_dispatch_mode_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("out.log");
        let log_str = log.to_string_lossy().into_owned();

        let patrol = patrol_named("bad-mode", vec!["true"], "bogus", Some(&log_str));
        let cfg = scheduler_cfg(&log_str);
        let err = ProcessDispatcher.dispatch(&patrol, &cfg).unwrap_err();
        assert!(matches!(err, DispatchError::UnknownMode { .. }));
    }

    #[test]
    fn missing_binary_errors_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("out.log");
        let log_str = log.to_string_lossy().into_owned();

        let patrol = patrol_named(
            "nope",
            vec!["__this_binary_cannot_exist_42__"],
            "wait",
            Some(&log_str),
        );
        let cfg = scheduler_cfg(&log_str);
        let err = ProcessDispatcher.dispatch(&patrol, &cfg).unwrap_err();
        assert!(matches!(err, DispatchError::Spawn { .. }));
    }

    #[test]
    fn run_sunset_action_noop_when_no_sunset_block() {
        let patrol = patrol_named("no-sunset", vec!["echo"], "detached", None);
        let outcome = run_sunset_action(&patrol);
        assert!(outcome.plist.is_none());
        assert!(outcome.unload_error.is_none());
    }

    #[test]
    fn run_sunset_action_noop_when_sunset_without_plist() {
        let mut patrol = patrol_named("no-plist", vec!["echo"], "detached", None);
        patrol.sunset = Some(crate::config::Sunset {
            strategy: crate::config::SunsetStrategy::SampleCount,
            sample_file: Some("/tmp/s".to_owned()),
            min_samples: Some(1),
            variance_threshold: None,
            window: None,
            trigger_file: None,
            launchctl_plist: None,
            on_sunset: Vec::new(),
        });
        let outcome = run_sunset_action(&patrol);
        assert!(outcome.plist.is_none());
        assert!(outcome.unload_error.is_none());
    }

    #[test]
    fn run_sunset_action_propagates_plist_path() {
        // Host-agnostic smoke: when `launchctl_plist` is set we always
        // return `Some(plist)` in the outcome regardless of whether the
        // unload succeeds. `unload_error` is inherently host-dependent
        // (macOS `launchctl unload` on a missing path exits 0 with
        // "Unload failed: 5" on stderr; Linux has no launchctl at all
        // and we get a spawn error). The caller's idempotence fence
        // (`sunset_decided_at`) ensures we never loop even when the
        // advisory unload silently "succeeds".
        let mut patrol = patrol_named("bad-plist", vec!["echo"], "detached", None);
        patrol.sunset = Some(crate::config::Sunset {
            strategy: crate::config::SunsetStrategy::SampleCount,
            sample_file: Some("/tmp/s".to_owned()),
            min_samples: Some(1),
            variance_threshold: None,
            window: None,
            trigger_file: None,
            launchctl_plist: Some("/tmp/__never_exists_u2_sunset_42__.plist".to_owned()),
            on_sunset: Vec::new(),
        });
        let outcome = run_sunset_action(&patrol);
        assert_eq!(
            outcome.plist.as_deref(),
            Some("/tmp/__never_exists_u2_sunset_42__.plist"),
            "plist must always be recorded in the outcome"
        );
    }

    #[test]
    fn empty_command_is_an_error_even_though_validator_catches_it() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("out.log");
        let log_str = log.to_string_lossy().into_owned();

        let mut patrol = patrol_named("empty", vec!["true"], "wait", Some(&log_str));
        patrol.command.clear();
        let cfg = scheduler_cfg(&log_str);
        let err = ProcessDispatcher.dispatch(&patrol, &cfg).unwrap_err();
        assert!(matches!(err, DispatchError::EmptyCommand { .. }));
    }
}
