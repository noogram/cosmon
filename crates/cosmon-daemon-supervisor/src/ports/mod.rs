// SPDX-License-Identifier: AGPL-3.0-only

//! Port traits — every side-effect the supervisor needs lives behind one of
//! these four abstractions.
//!
//! Hexagonal architecture, same discipline as `cosmon-transport`,
//! `cosmon-state`, and `cosmon-scheduler::Dispatcher`:
//!
//! - [`ProcessPort`] — spawn / signal / reap children. Implemented by
//!   `tokio::process::Command` in Task 2.
//! - [`ConfigWatchPort`] — observe edits to `daemons.toml` (debounced).
//!   Implemented by the `notify` crate in Task 2.
//! - [`ClockPort`] — source of the current [`DateTime<Utc>`] so policy
//!   decisions are testable without `std::time`.
//! - [`StatePort`] — atomic read/write of the persisted supervisor state
//!   (exit codes, pids, respawn counters, last seen). Implemented on top of
//!   the filestore temp-file-and-rename idiom in Task 2.
//!
//! Every port trait is `pub` so downstream crates (the real adapters + tests)
//! can implement them. Associated `Error` types are concrete so the
//! event-loop code doesn't drown in generics.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::DaemonSpec;
use crate::model::ChildStatus;

// ---------------------------------------------------------------------------
// Signal — what the supervisor asks the ProcessPort to deliver
// ---------------------------------------------------------------------------

/// POSIX signals the supervisor needs. Kept narrow on purpose — we never
/// want a code path that sends SIGKILL without the SIGTERM grace first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Polite "please terminate" — the supervisor sends this first and
    /// waits the grace window before escalating.
    Term,
    /// Hard kill — sent only after the `SIGTERM` grace elapsed.
    Kill,
}

/// Non-blocking reap outcome. Distinguishes "still alive" from "dead without
/// an exit code" (signal kills on some platforms) from a proper exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapOutcome {
    /// Child is still running.
    Alive,
    /// Child exited with the given Unix exit code.
    Exited(i32),
    /// Child died but the adapter could not capture a code (typically a
    /// signal kill without `WEXITSTATUS`).
    Signaled,
}

// ---------------------------------------------------------------------------
// ProcessPort
// ---------------------------------------------------------------------------

/// Errors the process port may report.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// No known child at this pid.
    #[error("unknown pid: {0}")]
    UnknownPid(u32),
    /// OS-level error surfaced by the adapter.
    #[error("os error: {0}")]
    Os(String),
}

/// A record of one spawn attempt — everything the event loop needs to
/// advance the [`crate::Child`] state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnedChild {
    /// OS pid assigned by the adapter.
    pub pid: u32,
    /// Wall-clock time the adapter observed the child go live.
    pub started_at: DateTime<Utc>,
}

/// Spawn, signal, and reap child processes. **No I/O in this trait — the
/// trait defines shape; the adapter does the work.**
pub trait ProcessPort {
    /// Spawn a new child process from `spec`, returning its pid and start
    /// time.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Os`] if the OS refuses the spawn (binary not
    /// found, `EAGAIN`, etc.). Throttle re-evaluation is the event loop's
    /// concern, not the port's.
    fn spawn(&mut self, spec: &DaemonSpec) -> Result<SpawnedChild, ProcessError>;

    /// Deliver `signal` to the process with the given pid.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::UnknownPid`] if the adapter has no record of
    /// the pid, [`ProcessError::Os`] for OS-level failures.
    fn signal(&mut self, pid: u32, signal: Signal) -> Result<(), ProcessError>;

    /// Non-blocking check: has the child exited? See [`ReapOutcome`] for the
    /// three possible states.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::UnknownPid`] if the adapter has no record of
    /// the pid.
    fn reap(&mut self, pid: u32) -> Result<ReapOutcome, ProcessError>;
}

// ---------------------------------------------------------------------------
// ConfigWatchPort
// ---------------------------------------------------------------------------

/// A single observed change on the watched config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChange {
    /// Wall-clock time the adapter observed the change.
    pub at: DateTime<Utc>,
}

/// Errors the config-watch port may report.
#[derive(Debug, Error)]
pub enum ConfigWatchError {
    /// Adapter could not subscribe to the file (permissions, missing
    /// directory, kernel limit, …).
    #[error("subscribe failed: {0}")]
    Subscribe(String),
    /// OS-level error while polling.
    #[error("poll failed: {0}")]
    Os(String),
}

/// Observe `daemons.toml` edits. The real adapter debounces inotify /
/// `FSEvents` bursts (edit-save fires multiple events) down to one
/// [`ConfigChange`]. The event loop reacts by reloading and running
/// [`crate::diff`].
pub trait ConfigWatchPort {
    /// Block until the next config change is observed.
    ///
    /// Returns `Ok(None)` when the watcher has been shut down cleanly
    /// (e.g. by a `Drop` signal), `Ok(Some(change))` when a change lands,
    /// and `Err(_)` on adapter errors.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWatchError`] if the underlying watcher fails.
    fn next(&mut self) -> Result<Option<ConfigChange>, ConfigWatchError>;
}

// ---------------------------------------------------------------------------
// ClockPort
// ---------------------------------------------------------------------------

/// Source of "now". Injected into policy decisions so tests can pin a
/// virtual clock and step it deliberately.
pub trait ClockPort {
    /// Return the current wall-clock time.
    fn now(&self) -> DateTime<Utc>;
}

// ---------------------------------------------------------------------------
// StatePort
// ---------------------------------------------------------------------------

/// Persisted status of one supervised child — what lives in
/// `daemon-supervisor.state.json` between supervisor restarts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedChild {
    /// Daemon name (the state-file key).
    pub name: String,
    /// Current status.
    pub status: ChildStatus,
    /// Last known pid, if any.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Last observed exit code, if any.
    #[serde(default)]
    pub last_exit_code: Option<i32>,
    /// Time of most recent spawn. Useful for operator diagnostics.
    #[serde(default)]
    pub last_spawn_at: Option<DateTime<Utc>>,
    /// Time of most recent exit. Used to recompute throttle deadlines on
    /// supervisor restart.
    #[serde(default)]
    pub last_exit_at: Option<DateTime<Utc>>,
    /// Lifetime respawn counter.
    #[serde(default)]
    pub respawn_count: u32,
}

/// Top-level state document the supervisor reads/writes atomically.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SupervisorState {
    /// Schema version. Always `1` for this crate release.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Children keyed by daemon name.
    #[serde(default)]
    pub children: BTreeMap<String, PersistedChild>,
}

const fn default_version() -> u32 {
    1
}

/// Errors surfaced by the state port.
#[derive(Debug, Error)]
pub enum StateError {
    /// I/O error (read / write / rename).
    #[error("io error: {0}")]
    Io(String),
    /// Serialization error (bad JSON).
    #[error("serde error: {0}")]
    Serde(String),
}

/// Load / save the supervisor state atomically.
///
/// "Atomically" is the adapter's responsibility — the trait just asks it to
/// `save` the full document and guarantees that a concurrent `load` sees
/// either the previous snapshot or the new one, never a torn read.
pub trait StatePort {
    /// Read the current state from storage. A fresh install returns
    /// [`SupervisorState::default()`].
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Io`] if the backing store is unreachable,
    /// [`StateError::Serde`] if the file exists but is corrupt.
    fn load(&self) -> Result<SupervisorState, StateError>;

    /// Replace the stored state with `state`.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Io`] on any I/O failure (including a failed
    /// atomic rename), [`StateError::Serde`] on serialization errors.
    fn save(&mut self, state: &SupervisorState) -> Result<(), StateError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_is_copyable_and_equality_holds() {
        assert_eq!(Signal::Term, Signal::Term);
        assert_ne!(Signal::Term, Signal::Kill);
    }

    #[test]
    fn supervisor_state_roundtrip() {
        let mut s = SupervisorState::default();
        s.children.insert(
            "x".into(),
            PersistedChild {
                name: "x".into(),
                status: ChildStatus::Running,
                pid: Some(42),
                last_exit_code: None,
                last_spawn_at: None,
                last_exit_at: None,
                respawn_count: 3,
            },
        );
        let j = serde_json::to_string(&s).unwrap();
        let back: SupervisorState = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}
