// SPDX-License-Identifier: AGPL-3.0-only

//! An opt-in, append-only trace of what the readiness loop actually observed.
//!
//! # Why this exists
//!
//! Issue #20's fourth door produced a contradiction no amount of reading could
//! settle: fed arm C's captured pane verbatim, [`crate::readiness::classify_output`]
//! returns `AwaitingHuman`,
//! [`ClaudeTuiProbe::await_live`](crate::readiness::LiveProbe::await_live) maps
//! that to `Indeterminate`, and `cs tackle` is written to refuse it — yet on the
//! container bench `cs tackle` exited 0, left the tmux session up and left the
//! molecule `running`. Unit-green and bench-red at the same seam.
//!
//! The gap can only be between *what the process observed during its readiness
//! window* and *what the pane shows immediately before and after it*. Nothing in
//! the code path recorded the former, so every explanation on offer was a guess.
//! This module makes the process say it: one JSON line per sample, carrying the
//! classified status **and the exact bytes it classified**, so the pane the probe
//! read can be diffed against the pane the bench captured.
//!
//! # Contract
//!
//! - **Off unless asked.** Nothing is written, and no file is opened, unless
//!   `COSMON_READINESS_TRACE` names a path. A worker that has not asked for a
//!   trace pays one `env::var` per sample.
//! - **Never fails the dispatch.** Every I/O error here is swallowed. A
//!   diagnostic that can abort the thing it diagnoses is worse than no
//!   diagnostic; the trace is evidence, never control flow.
//! - **Never a gate.** No decision anywhere reads this module. It is a mirror
//!   held up to the loop, not a lever on it.
//!
//! # Format
//!
//! One JSON object per line (JSONL), appended. Keys are stable because the
//! bench greps them: `ts`, `event`, `worker`, and the optional `elapsed_ms`,
//! `status`, `liveness`, `note`, `pane_lines`, `pane`.

use std::io::Write;

/// The environment variable that names the trace file.
///
/// Set it to an absolute path the dispatching process can write. Unset (the
/// default) means no trace at all.
pub const TRACE_ENV: &str = "COSMON_READINESS_TRACE";

/// One traced observation of the readiness loop.
///
/// Constructed with [`Sample::new`] and refined with the builder methods; every
/// field beyond `event` and `worker` is optional because the sites that record
/// differ in what they know (a capture knows the pane, a verdict knows the
/// collapsed [`crate::readiness::Liveness`], the handshake knows only which key
/// it pressed).
#[derive(Debug, Clone)]
pub struct Sample<'a> {
    /// What happened — `capture`, `handshake`, `wait_ready.return`,
    /// `dispatch_gate`, `spawn_postcondition.return`.
    pub event: &'a str,
    /// The worker whose pane this is about.
    pub worker: &'a str,
    /// Milliseconds since the enclosing loop started, when the site knows it.
    pub elapsed_ms: Option<u128>,
    /// The Claude-TUI verdict, rendered through its `Display`.
    pub status: Option<String>,
    /// The substrate-agnostic verdict, rendered through its `Display`.
    pub liveness: Option<String>,
    /// A free-text clause naming *why* this line exists (which key was sent,
    /// which arm of a match returned).
    pub note: Option<&'a str>,
    /// The exact bytes that were classified. This is the load-bearing field:
    /// the whole point of the trace is to compare it with what the bench
    /// captured from the same pane.
    pub pane: Option<&'a str>,
}

impl<'a> Sample<'a> {
    /// A sample carrying only the two mandatory fields.
    #[must_use]
    pub fn new(event: &'a str, worker: &'a str) -> Self {
        Self {
            event,
            worker,
            elapsed_ms: None,
            status: None,
            liveness: None,
            note: None,
            pane: None,
        }
    }

    /// Attach the loop-relative timestamp.
    #[must_use]
    pub fn elapsed_ms(mut self, ms: u128) -> Self {
        self.elapsed_ms = Some(ms);
        self
    }

    /// Attach the Claude-TUI verdict (anything `Display`, so the enum stays
    /// this module's caller's vocabulary rather than its own).
    #[must_use]
    pub fn status(mut self, status: &impl std::fmt::Display) -> Self {
        self.status = Some(status.to_string());
        self
    }

    /// Attach the substrate-agnostic verdict.
    #[must_use]
    pub fn liveness(mut self, liveness: &impl std::fmt::Display) -> Self {
        self.liveness = Some(liveness.to_string());
        self
    }

    /// Attach the clause naming why this line exists.
    #[must_use]
    pub fn note(mut self, note: &'a str) -> Self {
        self.note = Some(note);
        self
    }

    /// Attach the exact bytes that were classified.
    #[must_use]
    pub fn pane(mut self, pane: &'a str) -> Self {
        self.pane = Some(pane);
        self
    }
}

/// `true` when a trace path is configured.
///
/// Exposed so a caller can skip building an expensive `Sample` (cloning a pane)
/// when nobody is listening.
#[must_use]
pub fn is_enabled() -> bool {
    std::env::var_os(TRACE_ENV).is_some_and(|v| !v.is_empty())
}

/// Append one sample to the trace file, if one is configured.
///
/// Silent and infallible by design — see the module contract. A caller must
/// never branch on whether this worked.
pub fn record(sample: &Sample<'_>) {
    let Some(path) = std::env::var_os(TRACE_ENV).filter(|v| !v.is_empty()) else {
        return;
    };

    let mut obj = serde_json::Map::new();
    obj.insert(
        "ts".into(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    obj.insert(
        "event".into(),
        serde_json::Value::String(sample.event.to_string()),
    );
    obj.insert(
        "worker".into(),
        serde_json::Value::String(sample.worker.to_string()),
    );
    if let Some(ms) = sample.elapsed_ms {
        // Saturating rather than truncating: a readiness window that somehow
        // outlived `u64::MAX` milliseconds should read as absurdly large in the
        // trace, never as a small number that invites a wrong reading.
        obj.insert(
            "elapsed_ms".into(),
            serde_json::Value::from(u64::try_from(ms).unwrap_or(u64::MAX)),
        );
    }
    if let Some(s) = &sample.status {
        obj.insert("status".into(), serde_json::Value::String(s.clone()));
    }
    if let Some(l) = &sample.liveness {
        obj.insert("liveness".into(), serde_json::Value::String(l.clone()));
    }
    if let Some(n) = sample.note {
        obj.insert("note".into(), serde_json::Value::String(n.to_string()));
    }
    if let Some(p) = sample.pane {
        obj.insert(
            "pane_lines".into(),
            serde_json::Value::from(u64::try_from(p.lines().count()).unwrap_or(u64::MAX)),
        );
        obj.insert("pane".into(), serde_json::Value::String(p.to_string()));
    }

    let Ok(line) = serde_json::to_string(&serde_json::Value::Object(obj)) else {
        return;
    };

    // Append-only, created on first write. Errors are swallowed: a worker whose
    // trace path is unwritable must still dispatch exactly as it would with the
    // variable unset.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trace must be a no-op when nobody asked for it — including not
    /// creating the file. This is what lets it live on the production dispatch
    /// path without being a side effect.
    #[test]
    fn records_nothing_when_the_variable_is_unset() {
        // `is_enabled` is the only observable this test can assert without
        // mutating process-wide env under a parallel test runner.
        if std::env::var_os(TRACE_ENV).is_none() {
            assert!(!is_enabled());
        }
    }

    /// A sample carries the pane verbatim — the property the whole module
    /// exists for. If this ever starts truncating, the trace stops being able
    /// to answer "did the probe see the same screen the bench captured?".
    #[test]
    fn a_sample_keeps_the_pane_verbatim() {
        let pane = "Select login method:\n\n ❯ 1. Claude account with subscription";
        let s = Sample::new("capture", "w-1").pane(pane);
        assert_eq!(s.pane, Some(pane));
    }
}
