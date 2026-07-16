// SPDX-License-Identifier: AGPL-3.0-only

//! Live-session presence registry.
//!
//! Presence is the single on-disk primitive that makes N Claude (or any
//! other) sessions visible to each other. Each running session writes a
//! single JSON file under `.cosmon/state/presence/<sid>.json` and
//! refreshes its `heartbeat_at` periodically. Peers discover live sessions
//! by a directory scan — no broker, no mailbox, no daemon.
//!
//! # Lifetime
//!
//! - Writer: the session's own process. One file per session, single
//!   writer by construction.
//! - Readers: any peer doing a directory scan. Stale files (heartbeat
//!   older than [`STALE_AFTER`] AND originating PID no longer alive) are
//!   garbage-collected idempotently by any caller's `gc()`.
//!
//! # Distinction from [`crate::worker`]
//!
//! Workers are molecules-in-execution (fleet-level); Presence is about
//! *pilots* — the interactive sessions driving the cosmon galaxy. A
//! single host can host many pilot sessions (one per terminal tab) and
//! zero workers, or the reverse. The two concepts are never conflated.

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{MoleculeId, SessionId};

/// A heartbeat older than this duration is considered stale. Paired with
/// a PID-liveness check in [`Presence::is_live`] and the filestore `gc`,
/// a session that crashes hard (kernel panic, SIGKILL) disappears from
/// the scan within one heartbeat window.
pub const STALE_AFTER: Duration = Duration::minutes(3);

/// Chalk-mark left by a live session under
/// `.cosmon/state/presence/<session_id>.json`.
///
/// Fields are intentionally small and self-describing — any cosmon CLI
/// (or external tool) can read this file without loading the whole state
/// store. The on-disk schema is `serde_json` over this struct.
///
/// # Examples
///
/// ```
/// use chrono::{Duration, Utc};
/// use cosmon_core::id::SessionId;
/// use cosmon_core::presence::{Presence, STALE_AFTER};
///
/// let now = Utc::now();
/// let p = Presence {
///     session_id: SessionId::new("demo-sid").unwrap(),
///     galaxy: "cosmon".to_owned(),
///     cwd: "/tmp/proj".into(),
///     pid: 4242,
///     started_at: now,
///     heartbeat_at: now,
///     current_molecule: None,
///     headline: "idle".to_owned(),
///     tty: None,
/// };
/// assert!(p.is_live(now));
/// assert!(!p.is_live(now + STALE_AFTER + Duration::seconds(1)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    /// Identity of the emitting session.
    pub session_id: SessionId,
    /// Which galaxy the session is operating in (`cosmon`,
    /// `mailroom`, `accord`, …). Scanners use this for filtering.
    pub galaxy: String,
    /// Absolute working directory at session launch time. A peer reads
    /// this to know "where" the session lives (which project root,
    /// which worktree).
    pub cwd: PathBuf,
    /// OS process id of the session's driver. The `gc` sweep tests this
    /// for liveness so a session that died without unlinking its file
    /// is removed deterministically.
    pub pid: u32,
    /// When the session first emitted its presence file.
    pub started_at: DateTime<Utc>,
    /// Last refresh. The session hook bumps this every ~30 s; a reader
    /// treats the session as stale if `now - heartbeat_at > STALE_AFTER`.
    pub heartbeat_at: DateTime<Utc>,
    /// Molecule currently under the session's attention, if any.
    /// Advisory — the DAG is still the authoritative ownership signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_molecule: Option<MoleculeId>,
    /// One line of free-form text the operator or hook can set via
    /// `cs presence ping --headline "..."`. Shown in `cs presence ls`.
    pub headline: String,
    /// Controlling terminal, when resolvable (e.g. `ttys012`). Useful
    /// for disambiguating two sessions in the same galaxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<String>,
}

impl Presence {
    /// Return `true` iff the most recent heartbeat is within
    /// [`STALE_AFTER`] of `now`.
    ///
    /// Pure on the struct — the filestore's `gc` augments this with a
    /// PID-alive probe before deleting the file.
    #[must_use]
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        now - self.heartbeat_at < STALE_AFTER
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap()
    }

    fn sample(now: DateTime<Utc>) -> Presence {
        Presence {
            session_id: SessionId::new("sid-test").unwrap(),
            galaxy: "cosmon".to_owned(),
            cwd: PathBuf::from("/tmp/proj"),
            pid: 4242,
            started_at: now,
            heartbeat_at: now,
            current_molecule: None,
            headline: "idle".to_owned(),
            tty: Some("ttys012".to_owned()),
        }
    }

    #[test]
    fn is_live_on_fresh_heartbeat() {
        let now = fixed_now();
        let p = sample(now);
        assert!(p.is_live(now));
        assert!(p.is_live(now + Duration::seconds(30)));
    }

    #[test]
    fn is_stale_past_threshold() {
        let now = fixed_now();
        let p = sample(now);
        assert!(!p.is_live(now + STALE_AFTER + Duration::seconds(1)));
    }

    #[test]
    fn is_live_boundary_excludes_exact_threshold() {
        // `< STALE_AFTER` — exactly at threshold is stale. This pins
        // the comparator so a future change to `<=` fails the test
        // rather than silently widening the live window.
        let now = fixed_now();
        let p = sample(now);
        assert!(!p.is_live(now + STALE_AFTER));
    }

    #[test]
    fn json_roundtrip() {
        let now = fixed_now();
        let p = sample(now);
        let json = serde_json::to_string(&p).unwrap();
        let back: Presence = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn json_omits_optional_none_fields() {
        let now = fixed_now();
        let mut p = sample(now);
        p.tty = None;
        p.current_molecule = None;
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("\"tty\""), "tty should be skipped: {json}");
        assert!(
            !json.contains("\"current_molecule\""),
            "current_molecule should be skipped: {json}"
        );
    }
}
