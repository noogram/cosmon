// SPDX-License-Identifier: AGPL-3.0-only

//! Scheduler event log — append-only JSONL sibling of the state file.
//!
//! ## Why a separate event log
//!
//! `state.json` answers "what is true right now?" and is rewritten atomically
//! on every tick. `events.jsonl` answers "what *happened* and in what order?"
//! Distinct concerns, distinct files. The scheduler's event log is currently
//! scoped to structural moments (sunset, sunset-unload failure) rather than
//! every dispatch — those have their own log redirection via `log_file`.
//!
//! ## Shape of a record
//!
//! Each line is exactly one JSON object with a stable top-level shape:
//!
//! ```json
//! {"ts":"2026-04-19T15:00:00Z","kind":"patrol.sunsetted","patrol":"u2-probe","detail":{"reason":"variance-threshold converged"}}
//! {"ts":"2026-04-19T15:00:00Z","kind":"patrol.sunset_unload_failed","patrol":"u2-probe","detail":{"plist":"~/Library/…","error":"Path not valid"}}
//! ```
//!
//! ## Derivation of the path
//!
//! The log file lives next to the state file: `state_file` with its extension
//! replaced by `.events.jsonl`. Keeping it a sibling means the scheduler does
//! not need a second config knob and operators who already know where state
//! lives find the events immediately.
//!
//! ## Failure policy
//!
//! Appending to the event log is **defensive I/O**: a failure here must
//! never abort the tick. The public [`append_event`] function returns an
//! `io::Result` so the caller can log the error itself, but a failing write
//! does not propagate past the scheduler boundary.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single append-only event line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulerEvent {
    /// UTC timestamp of the event.
    pub ts: DateTime<Utc>,

    /// Kind of event. Stable string (not an enum) so future event kinds
    /// added by downstream tooling do not force a schema migration on
    /// readers that just want to show the line.
    pub kind: String,

    /// Name of the patrol this event relates to. Empty string for
    /// scheduler-wide events (reserved for future use).
    pub patrol: String,

    /// Arbitrary JSON detail blob. Readers that care about structure parse
    /// it per-kind; the append path is agnostic.
    pub detail: serde_json::Value,
}

impl SchedulerEvent {
    /// Build a `scheduler.ticked` event. Emitted once per successful tick
    /// as a liveness heartbeat so external observers (`cs pulse`) can
    /// measure the real scheduler cadence without relying on worker-spawn
    /// proxies in cosmon's own `events.jsonl`.
    ///
    /// `patrols_fired` and `patrols_skipped` are summary counters — they
    /// allow a reader to distinguish an idle-but-alive scheduler (all
    /// skipped) from a scheduler that is actively dispatching.
    #[must_use]
    pub fn ticked(ts: DateTime<Utc>, patrols_fired: u32, patrols_skipped: u32) -> Self {
        Self {
            ts,
            kind: "scheduler.ticked".to_owned(),
            patrol: String::new(),
            detail: serde_json::json!({
                "patrols_fired": patrols_fired,
                "patrols_skipped": patrols_skipped,
            }),
        }
    }

    /// Build a `patrol.sunsetted` event. Emitted once per patrol lifetime
    /// when the convergence rule fires.
    #[must_use]
    pub fn sunsetted(
        ts: DateTime<Utc>,
        patrol: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            ts,
            kind: "patrol.sunsetted".to_owned(),
            patrol: patrol.into(),
            detail: serde_json::json!({ "reason": reason.into() }),
        }
    }

    /// Build a `patrol.sunset_unload_failed` event. Advisory — records
    /// that the launchctl unload side-effect failed but the sunset
    /// decision is still authoritative (state records `sunset_decided_at`
    /// and subsequent ticks short-circuit).
    #[must_use]
    pub fn sunset_unload_failed(
        ts: DateTime<Utc>,
        patrol: impl Into<String>,
        plist: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            ts,
            kind: "patrol.sunset_unload_failed".to_owned(),
            patrol: patrol.into(),
            detail: serde_json::json!({
                "plist": plist.into(),
                "error": error.into(),
            }),
        }
    }

    /// Build a `patrol.sunset_hook_failed` event. Advisory — records that
    /// a hook declared in `on_sunset = [...]` failed to execute after the
    /// sunset decision was recorded. The sunset itself is still
    /// authoritative (idempotence flag is flipped before hooks run).
    #[must_use]
    pub fn sunset_hook_failed(
        ts: DateTime<Utc>,
        patrol: impl Into<String>,
        hook: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            ts,
            kind: "patrol.sunset_hook_failed".to_owned(),
            patrol: patrol.into(),
            detail: serde_json::json!({
                "hook": hook.into(),
                "error": error.into(),
            }),
        }
    }
}

/// Append one event to `path`, one JSON object per line. Creates the
/// parent directory and the file on first write.
///
/// # Errors
///
/// Returns the underlying `io::Error` if directory creation, file open,
/// or write fails. Callers are expected to treat this as advisory.
pub fn append_event(path: &Path, event: &SchedulerEvent) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(event)
        .map_err(|e| io::Error::other(format!("event serialize: {e}")))?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// Derive the events-file path from `state_file`. Replaces any extension
/// with `.events.jsonl` — `~/.cosmon/scheduler.state.json` becomes
/// `~/.cosmon/scheduler.state.events.jsonl`. The approach keeps the two
/// files visibly paired in a directory listing.
#[must_use]
pub fn derive_events_path(state_file: &Path) -> PathBuf {
    let mut s = state_file.as_os_str().to_owned();
    s.push(".events.jsonl");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn ticked_event_shape() {
        let ts = Utc.with_ymd_and_hms(2026, 6, 26, 10, 0, 0).unwrap();
        let ev = SchedulerEvent::ticked(ts, 3, 7);
        assert_eq!(ev.kind, "scheduler.ticked");
        assert_eq!(
            ev.patrol, "",
            "patrol field must be empty for scheduler-wide events"
        );
        assert_eq!(ev.ts, ts);
        assert_eq!(
            ev.detail
                .get("patrols_fired")
                .and_then(serde_json::Value::as_u64),
            Some(3)
        );
        assert_eq!(
            ev.detail
                .get("patrols_skipped")
                .and_then(serde_json::Value::as_u64),
            Some(7)
        );
    }

    #[test]
    fn sunsetted_event_shape() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 19, 15, 0, 0).unwrap();
        let ev = SchedulerEvent::sunsetted(ts, "u2-probe", "variance-threshold converged");
        assert_eq!(ev.kind, "patrol.sunsetted");
        assert_eq!(ev.patrol, "u2-probe");
        assert_eq!(
            ev.detail.get("reason").and_then(|v| v.as_str()),
            Some("variance-threshold converged")
        );
    }

    #[test]
    fn sunset_hook_failed_event_shape() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 19, 15, 0, 0).unwrap();
        let ev = SchedulerEvent::sunset_hook_failed(
            ts,
            "u2-probe",
            "notify_telegram",
            "script exited non-zero: 2",
        );
        assert_eq!(ev.kind, "patrol.sunset_hook_failed");
        assert_eq!(ev.patrol, "u2-probe");
        assert_eq!(
            ev.detail.get("hook").and_then(|v| v.as_str()),
            Some("notify_telegram")
        );
        assert!(ev.detail.get("error").is_some());
    }

    #[test]
    fn sunset_unload_failed_event_shape() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 19, 15, 0, 0).unwrap();
        let ev = SchedulerEvent::sunset_unload_failed(
            ts,
            "u2-probe",
            "/Users/me/Library/LaunchAgents/u2.plist",
            "Could not find specified service",
        );
        assert_eq!(ev.kind, "patrol.sunset_unload_failed");
        assert_eq!(
            ev.detail.get("plist").and_then(|v| v.as_str()),
            Some("/Users/me/Library/LaunchAgents/u2.plist")
        );
        assert!(ev.detail.get("error").is_some());
    }

    #[test]
    fn append_event_creates_file_and_writes_one_line_per_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a").join("events.jsonl");

        let ts = Utc.with_ymd_and_hms(2026, 4, 19, 15, 0, 0).unwrap();
        append_event(&path, &SchedulerEvent::sunsetted(ts, "p1", "r1")).unwrap();
        append_event(&path, &SchedulerEvent::sunsetted(ts, "p2", "r2")).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            // Every line must be a valid JSON object.
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["kind"], "patrol.sunsetted");
        }
    }

    #[test]
    fn derive_events_path_sibling_convention() {
        let state = Path::new("/tmp/cosmon/scheduler.state.json");
        assert_eq!(
            derive_events_path(state),
            PathBuf::from("/tmp/cosmon/scheduler.state.json.events.jsonl")
        );
    }
}
