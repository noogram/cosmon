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
use crate::pilot_lease::LeaseEpoch;

/// Which seat a pilot session occupies on a mission (ADR-168 §D6).
///
/// The default is [`PilotRole::Copilot`], and that is the whole point:
/// FAIL-CLOSED-AUTHORITY says an unknown session, lease or epoch implies
/// read-only. A presence file written by an older `cs` has no `role` field;
/// deserialising it must not silently mint a primary.
///
/// # Examples
///
/// ```
/// use cosmon_core::presence::PilotRole;
///
/// // A snapshot from before the field existed decodes as read-only.
/// let old: PilotRole = serde_json::from_str("null").unwrap_or_default();
/// assert_eq!(old, PilotRole::Copilot);
/// assert!(!old.is_primary());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PilotRole {
    /// Holds (or claims to hold) the pilot lease — may emit operator gestures.
    Primary,
    /// Observes, messages, checkpoints and reports. Never mutates.
    #[default]
    Copilot,
}

impl PilotRole {
    /// Return `true` iff this role is [`PilotRole::Primary`].
    ///
    /// Exists so call-sites read as a question about authority rather than as
    /// an enum comparison, and so the fail-closed default has one place to be
    /// wrong instead of many.
    #[must_use]
    pub fn is_primary(self) -> bool {
        matches!(self, Self::Primary)
    }

    /// The lowercase wire name — the same token the JSON snapshot carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Copilot => "copilot",
        }
    }
}

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
/// let p = Presence::new(
///     SessionId::new("demo-sid").unwrap(),
///     "cosmon",
///     "/tmp/proj",
///     4242,
///     now,
/// );
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
    /// The provider that minted the underlying model session — `claude`,
    /// `codex`, … . Half of the PROVIDER-ID-NATIVE key (ADR-168 §D2); the
    /// other half is [`Self::native_session_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The provider's own id for the session, read from inside its log —
    /// never decoded from a directory name and never a display title. A
    /// `/rename` must not move a session, which is why the key is this and
    /// not [`Self::headline`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_session_id: Option<String>,
    /// Which seat this pilot occupies. Absent on a pre-M2 snapshot, which
    /// decodes as [`PilotRole::Copilot`] — read-only, per
    /// FAIL-CLOSED-AUTHORITY.
    #[serde(default)]
    pub role: PilotRole,
    /// The session this one is co-piloting, as a cosmon session id. Set on a
    /// co-pilot; `None` on a primary. This is what makes presence *reciprocal*
    /// rather than merely parallel: a peer scan can tell "also here" from
    /// "watching me".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<SessionId>,
    /// What this pilot can do, as free-form tokens (`observe`, `message`,
    /// `checkpoint`, …). Advertised, not enforced — the lease is what
    /// refuses, per ADR-168 §D6.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The most recent checkpoint this pilot published, if any. A co-pilot
    /// reads it to know where a takeover would resume from without replaying
    /// the transcript (CHECKPOINT-NOT-SCROLLBACK).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
    /// The mission this pilot's seat is about. `role: Primary` is a claim of
    /// authority, and authority is per-mission (ADR-168 §D6) — a seat with no
    /// mission names no lease and therefore backs nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission: Option<MoleculeId>,
    /// The lease epoch this pilot believes it holds on [`Self::mission`].
    ///
    /// Recorded rather than looked up, because "the epoch I believe I hold" is
    /// exactly the claim the guard checks. A snapshot that reports `primary`
    /// at an epoch the ledger has moved past is a *readable* stale primary,
    /// which is what makes the refusal after a transfer diagnosable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_epoch: Option<LeaseEpoch>,
}

impl Presence {
    /// Build the minimal presence of a session that has advertised nothing
    /// beyond being alive: no molecule, no headline, no provider, and the
    /// fail-closed [`PilotRole::Copilot`] seat.
    ///
    /// Exists so that adding a co-pilot field is an additive change at every
    /// call-site instead of a compile error at each one — the six M2 fields
    /// were added to a struct with five literal constructors, and the next
    /// six should cost less.
    #[must_use]
    pub fn new(
        session_id: SessionId,
        galaxy: impl Into<String>,
        cwd: impl Into<PathBuf>,
        pid: u32,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            session_id,
            galaxy: galaxy.into(),
            cwd: cwd.into(),
            pid,
            started_at: at,
            heartbeat_at: at,
            current_molecule: None,
            headline: String::new(),
            tty: None,
            provider: None,
            native_session_id: None,
            role: PilotRole::default(),
            follows: None,
            capabilities: Vec::new(),
            checkpoint_id: None,
            mission: None,
            lease_epoch: None,
        }
    }

    /// The canonical `<provider>:<native-session-id>` selector, when this
    /// session has published both halves.
    ///
    /// Returns `None` rather than a partial key: half a selector cannot
    /// address a session, and rendering `claude:` as if it could is exactly
    /// the confusion PROVIDER-ID-NATIVE exists to prevent.
    #[must_use]
    pub fn selector(&self) -> Option<String> {
        match (&self.provider, &self.native_session_id) {
            (Some(p), Some(n)) => Some(format!("{p}:{n}")),
            _ => None,
        }
    }

    /// The authority this snapshot claims: the mission it names and the epoch
    /// it believes it holds there.
    ///
    /// `None` unless the seat is [`PilotRole::Primary`] **and** both halves
    /// are present. A co-pilot claims nothing by definition, and a primary
    /// that names no mission or no epoch has made a claim the guard cannot
    /// check — which fails closed to "no claim", never to "trusted claim".
    #[must_use]
    pub fn claimed_authority(&self) -> Option<(&MoleculeId, LeaseEpoch)> {
        if !self.role.is_primary() {
            return None;
        }
        match (&self.mission, self.lease_epoch) {
            (Some(m), Some(e)) => Some((m, e)),
            _ => None,
        }
    }

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
            headline: "idle".to_owned(),
            tty: Some("ttys012".to_owned()),
            ..Presence::new(
                SessionId::new("sid-test").unwrap(),
                "cosmon",
                PathBuf::from("/tmp/proj"),
                4242,
                now,
            )
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

    // A snapshot written by a `cs` that predates M2 has none of the six
    // co-pilot fields. It must still decode — and it must decode to the
    // read-only seat, not to an authority nobody granted.
    #[test]
    fn a_pre_m2_snapshot_decodes_as_a_read_only_copilot() {
        let legacy = r#"{
            "session_id": "sid-legacy",
            "galaxy": "cosmon",
            "cwd": "/tmp/proj",
            "pid": 4242,
            "started_at": "2026-04-24T12:00:00Z",
            "heartbeat_at": "2026-04-24T12:00:00Z",
            "headline": "idle"
        }"#;
        let p: Presence = serde_json::from_str(legacy).unwrap();
        assert_eq!(p.role, PilotRole::Copilot);
        assert!(!p.role.is_primary());
        assert!(p.follows.is_none());
        assert!(p.capabilities.is_empty());
        assert_eq!(p.selector(), None);
    }

    #[test]
    fn a_selector_needs_both_halves_or_it_is_none() {
        let now = fixed_now();
        let mut p = sample(now);
        assert_eq!(p.selector(), None);
        p.provider = Some("claude".to_owned());
        assert_eq!(p.selector(), None, "half a key addresses nothing");
        p.native_session_id = Some("4940f28e".to_owned());
        assert_eq!(p.selector().as_deref(), Some("claude:4940f28e"));
    }

    // A snapshot written before M4 names no mission and no epoch. It must not
    // therefore be trusted as an authority: half a claim is no claim.
    #[test]
    fn a_primary_seat_without_a_mission_and_epoch_claims_nothing() {
        let now = fixed_now();
        let mut p = sample(now);
        p.role = PilotRole::Primary;
        assert_eq!(p.claimed_authority(), None, "no mission, no epoch");

        p.mission = Some(MoleculeId::new("task-20260731-9cf4").unwrap());
        assert_eq!(p.claimed_authority(), None, "a mission without an epoch");

        p.lease_epoch = Some(LeaseEpoch::first());
        let (mission, epoch) = p.claimed_authority().unwrap();
        assert_eq!(mission.as_str(), "task-20260731-9cf4");
        assert_eq!(epoch, LeaseEpoch::first());
    }

    #[test]
    fn a_copilot_claims_nothing_however_it_is_filled_in() {
        let now = fixed_now();
        let mut p = sample(now);
        p.role = PilotRole::Copilot;
        p.mission = Some(MoleculeId::new("task-20260731-9cf4").unwrap());
        p.lease_epoch = Some(LeaseEpoch::first());
        assert_eq!(p.claimed_authority(), None);
    }

    #[test]
    fn role_round_trips_through_its_wire_name() {
        for role in [PilotRole::Primary, PilotRole::Copilot] {
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, format!("\"{}\"", role.as_str()));
            let back: PilotRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, back);
        }
    }
}
