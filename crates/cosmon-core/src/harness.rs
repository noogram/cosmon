// SPDX-License-Identifier: AGPL-3.0-only

//! Functional test harness traits — mock the mechanical layer, never the LLM.
//!
//! # Role in the architecture
//!
//! Cosmon's transactional core is already pure (zero I/O in [`crate`]).
//! The CLI and runtime layers shell out to external processes (`cs tackle`,
//! `git`, `tmux`, `cargo`) and read wall-clock time ([`chrono::Utc::now`]).
//! Those two seams — **process execution** and **time** — are the only
//! mechanical dependencies that prevent an end-to-end scenario test from
//! running hermetically.
//!
//! This module defines the two injection points and keeps only the *ports*
//! (traits) plus their in-memory mock implementations. The production
//! adapter for process execution — `RealCommandRunner`, which defers to
//! [`std::process::Command`] — lives in `cosmon-transport`
//! (`cosmon_transport::command_runner`), so the domain crate stays zero-I/O
//! (INV-DOMAIN-PURE-NO-IO, ADR-082). The wall-clock adapter is [`RealClock`],
//! which wraps [`chrono::Utc::now`] and is the one ambient-time seam still
//! declared here (covered by waiver W1 until the planned `Clock` injection
//! lands).
//!
//! # Why not mock the LLM
//!
//! A provably untestable layer ([Rice theorem]: any non-trivial semantic
//! property of an arbitrary program is undecidable) is not a useful place
//! for fixtures. The panel consensus on this project's testing strategy
//! decided: **mock the mechanical layer, never the LLM**. `FakeWorker`
//! consumes canned response files that pretend to be a finished worker's
//! output; it does not pretend to be the worker.
//!
//! [Rice theorem]: https://en.wikipedia.org/wiki/Rice%27s_theorem

use std::path::Path;

use chrono::{DateTime, Utc};

// Mock-only imports — kept behind the same gate as the mock items below so a
// default (ports-only) build stays warning-clean under `-D warnings`.
#[cfg(any(test, feature = "test-harness"))]
use chrono::TimeZone;
#[cfg(any(test, feature = "test-harness"))]
use std::fmt;
#[cfg(any(test, feature = "test-harness"))]
use std::path::PathBuf;
#[cfg(any(test, feature = "test-harness"))]
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Captured output — what a CommandRunner returns
// ---------------------------------------------------------------------------

/// The observable result of one [`CommandRunner::exec`] call.
///
/// Mirrors the useful subset of [`std::process::Output`] while staying
/// serialisable so fixtures can be stored as JSON. `status` is the raw
/// process exit code, or `None` when the process was killed by a signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code; `None` on signal termination.
    pub status: Option<i32>,
    /// Captured stdout bytes as UTF-8 (lossy on invalid sequences).
    pub stdout: String,
    /// Captured stderr bytes as UTF-8 (lossy on invalid sequences).
    pub stderr: String,
}

impl CommandOutput {
    /// A successful (`exit 0`) run with the given stdout and no stderr.
    #[must_use]
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self {
            status: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// A failed run with the given exit code and stderr message.
    #[must_use]
    pub fn err(code: i32, stderr: impl Into<String>) -> Self {
        Self {
            status: Some(code),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    /// Returns `true` if `status == Some(0)`.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Errors that can happen while trying to run a command through a
/// [`CommandRunner`].
///
/// Kept separate from [`crate::error::CosmonError`] so the harness can live
/// in `cosmon-core` without pulling in I/O error plumbing across the whole
/// crate.
#[derive(Debug, thiserror::Error)]
pub enum CommandRunnerError {
    /// The underlying process could not be spawned at all (e.g. missing
    /// binary, permission denied, working directory does not exist).
    #[error("failed to spawn {cmd}: {reason}")]
    Spawn {
        /// The command that could not be launched.
        cmd: String,
        /// Human-readable reason.
        reason: String,
    },
    /// The fixture script returned nothing for a call the mock was asked
    /// to service. Only raised by mock implementations.
    #[error("no scripted response for call #{index}: {cmd}")]
    Unscripted {
        /// Zero-based call index that had no scripted response.
        index: usize,
        /// Command line for diagnostics.
        cmd: String,
    },
}

// ---------------------------------------------------------------------------
// CommandRunner — the process-execution seam
// ---------------------------------------------------------------------------

/// Injectable subprocess executor.
///
/// The runtime and CLI layers should hold `Box<dyn CommandRunner>` instead
/// of calling [`std::process::Command`] directly. Tests inject
/// [`MockCommandRunner`] to assert on the exact sequence of calls
/// ([`MockCommandRunner::calls`]) and to script canned outputs
/// ([`MockCommandRunner::script`]).
///
/// The trait is object-safe: all parameters are concrete types and the
/// return type is a `Result<CommandOutput, CommandRunnerError>`.
pub trait CommandRunner: Send + Sync {
    /// Execute `cmd` with `args` in `cwd`, return captured output.
    ///
    /// # Errors
    ///
    /// Returns [`CommandRunnerError::Spawn`] if the process cannot be
    /// launched. A non-zero exit code is *not* an error — it is a
    /// successful call returning a [`CommandOutput`] with a non-zero
    /// status.
    fn exec(
        &self,
        cmd: &str,
        args: &[&str],
        cwd: &Path,
    ) -> Result<CommandOutput, CommandRunnerError>;
}

// The production `RealCommandRunner` adapter (which calls
// `std::process::Command`) lives in `cosmon-transport`
// (`cosmon_transport::command_runner::RealCommandRunner`) — moved out of the
// domain crate so `cosmon-core` performs no process I/O
// (INV-DOMAIN-PURE-NO-IO, ADR-082). Only the [`CommandRunner`] port and the
// in-memory [`MockCommandRunner`] remain here.

/// One recorded call to [`CommandRunner::exec`].
#[cfg(any(test, feature = "test-harness"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    /// Command binary name passed to the runner.
    pub cmd: String,
    /// Arguments list passed alongside `cmd`.
    pub args: Vec<String>,
    /// Working directory for the call.
    pub cwd: PathBuf,
}

#[cfg(any(test, feature = "test-harness"))]
impl fmt::Display for RecordedCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.cmd, self.args.join(" "))
    }
}

/// In-memory [`CommandRunner`] that records every call and returns
/// scripted responses.
///
/// Scripting rule: responses are pulled from an internal FIFO queue built
/// with [`MockCommandRunner::script`]. If the queue is empty when a call
/// arrives, a default successful empty-output response is returned — this
/// keeps tests terse when they only care about the call sequence, not the
/// output content.
#[cfg(any(test, feature = "test-harness"))]
#[derive(Default)]
pub struct MockCommandRunner {
    inner: Arc<Mutex<MockInner>>,
}

#[cfg(any(test, feature = "test-harness"))]
#[derive(Default)]
struct MockInner {
    scripted: std::collections::VecDeque<CommandOutput>,
    calls: Vec<RecordedCall>,
}

#[cfg(any(test, feature = "test-harness"))]
impl MockCommandRunner {
    /// Build an empty mock runner. Calls return [`CommandOutput::ok("")`]
    /// until something is pushed into the script queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a scripted response to the FIFO queue; subsequent calls pop
    /// responses in the order they were scripted.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned by a prior panic in
    /// another thread — test-only code, so this is fine.
    pub fn script(&self, output: CommandOutput) {
        self.inner
            .lock()
            .expect("mock lock")
            .scripted
            .push_back(output);
    }

    /// Snapshot of the calls recorded so far, in call order.
    ///
    /// # Panics
    /// Panics on poisoned mutex (test-only).
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.inner.lock().expect("mock lock").calls.clone()
    }

    /// Number of calls recorded so far.
    ///
    /// # Panics
    /// Panics on poisoned mutex (test-only).
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.inner.lock().expect("mock lock").calls.len()
    }
}

#[cfg(any(test, feature = "test-harness"))]
impl CommandRunner for MockCommandRunner {
    #[allow(clippy::similar_names)]
    fn exec(
        &self,
        cmd: &str,
        args: &[&str],
        cwd: &Path,
    ) -> Result<CommandOutput, CommandRunnerError> {
        let mut inner = self.inner.lock().expect("mock lock");
        inner.calls.push(RecordedCall {
            cmd: cmd.to_owned(),
            args: args.iter().map(|s| (*s).to_owned()).collect(),
            cwd: cwd.to_path_buf(),
        });
        Ok(inner
            .scripted
            .pop_front()
            .unwrap_or_else(|| CommandOutput::ok("")))
    }
}

// ---------------------------------------------------------------------------
// Clock — the wall-clock seam
// ---------------------------------------------------------------------------

/// Injectable wall clock. Production uses [`RealClock`] which wraps
/// [`Utc::now`]; tests use [`FixedClock`] or [`AdvancingClock`] to get
/// deterministic timestamps on event records.
pub trait Clock: Send + Sync {
    /// Return the current UTC time as seen by this clock.
    fn now(&self) -> DateTime<Utc>;
}

/// Production [`Clock`] returning [`Utc::now`] on every call.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Deterministic [`Clock`] always returning the same instant.
///
/// Useful when you want ISO-8601 timestamps to be byte-identical across
/// test runs (e.g. for golden-file comparisons or record/replay diffs).
#[cfg(any(test, feature = "test-harness"))]
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    instant: DateTime<Utc>,
}

#[cfg(any(test, feature = "test-harness"))]
impl FixedClock {
    /// Build a fixed clock anchored at `instant`.
    #[must_use]
    pub fn new(instant: DateTime<Utc>) -> Self {
        Self { instant }
    }

    /// Convenience constructor: `FixedClock::epoch()` anchors at
    /// `1970-01-01T00:00:00Z`.
    ///
    /// # Panics
    /// Never, in practice — the epoch is a valid UTC instant. The
    /// `.expect` is a belt-and-braces guard for the chrono API.
    #[must_use]
    pub fn epoch() -> Self {
        Self::new(
            Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
                .single()
                .expect("epoch is a valid UTC instant"),
        )
    }
}

#[cfg(any(test, feature = "test-harness"))]
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}

/// A clock that returns a strictly monotonically increasing sequence,
/// advancing by one second per call.
///
/// Useful when a test needs `now()` to be different on each call (e.g.
/// event timestamps must be strictly ordered) without the indeterminism
/// of [`RealClock`].
#[cfg(any(test, feature = "test-harness"))]
#[derive(Debug)]
pub struct AdvancingClock {
    inner: Mutex<DateTime<Utc>>,
    step: chrono::Duration,
}

#[cfg(any(test, feature = "test-harness"))]
impl AdvancingClock {
    /// Build an advancing clock that starts at `start` and advances by
    /// `step` on every [`Clock::now`] call.
    #[must_use]
    pub fn new(start: DateTime<Utc>, step: chrono::Duration) -> Self {
        Self {
            inner: Mutex::new(start),
            step,
        }
    }

    /// Convenience: start at epoch, advance by one second per call.
    ///
    /// # Panics
    /// Never in practice — the epoch instant is always valid.
    #[must_use]
    pub fn one_second() -> Self {
        Self::new(
            Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
                .single()
                .expect("epoch"),
            chrono::Duration::seconds(1),
        )
    }
}

#[cfg(any(test, feature = "test-harness"))]
impl Clock for AdvancingClock {
    /// # Panics
    /// Panics on poisoned mutex (test-only).
    fn now(&self) -> DateTime<Utc> {
        let mut guard = self.inner.lock().expect("clock lock");
        let t = *guard;
        *guard = t + self.step;
        t
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_runner_records_calls_and_returns_scripted_outputs() {
        let runner = MockCommandRunner::new();
        runner.script(CommandOutput::ok("first"));
        runner.script(CommandOutput::err(1, "boom"));

        let cwd = PathBuf::from("/tmp");
        let out1 = runner.exec("cs", &["tackle", "m1"], &cwd).unwrap();
        assert!(out1.success());
        assert_eq!(out1.stdout, "first");

        let out2 = runner.exec("cs", &["done", "m1"], &cwd).unwrap();
        assert!(!out2.success());
        assert_eq!(out2.stderr, "boom");

        // Default response when script queue is empty.
        let out3 = runner.exec("git", &["status"], &cwd).unwrap();
        assert!(out3.success());
        assert_eq!(out3.stdout, "");

        let calls = runner.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].cmd, "cs");
        assert_eq!(calls[0].args, vec!["tackle", "m1"]);
        assert_eq!(calls[2].cmd, "git");
    }

    #[test]
    fn fixed_clock_is_stable() {
        let clk = FixedClock::epoch();
        assert_eq!(clk.now(), clk.now());
    }

    #[test]
    fn advancing_clock_is_strictly_monotone() {
        let clk = AdvancingClock::one_second();
        let a = clk.now();
        let b = clk.now();
        let c = clk.now();
        assert!(a < b && b < c);
        assert_eq!(b - a, chrono::Duration::seconds(1));
    }
}
