// SPDX-License-Identifier: AGPL-3.0-only

//! Tmux session view.
//!
//! A [`Session`] is a tmux session hosting a cosmon worker. The socket path
//! is retained so queries can route back to the originating tmux instance
//! when a snapshot spans multiple sockets.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A tmux session running a cosmon worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Tmux session name (e.g. `cosmon-task-20260412-a22b`).
    pub name: String,
    /// Tmux socket path this session lives on.
    pub socket: String,
    /// Project root (the directory containing `.cosmon/`) this session belongs to.
    pub project_root: String,
    /// Molecule this session is executing, if any.
    pub molecule_id: Option<String>,
    /// Worker id attached to this session, if any.
    pub worker_id: Option<String>,
    /// Last-activity timestamp for this tmux session, if known. Sourced from
    /// tmux `#{session_activity}` (the instant of the last keystroke or
    /// pane output). Absence means the adapter could not resolve it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<DateTime<Utc>>,
}

/// Liveness tier derived from a session's last-activity timestamp.
///
/// Used by `cs peek` to render a heartbeat column and sort by activity.
/// The thresholds are deliberately broad — the operator needs a glanceable
/// signal, not a precise SLA.
///
/// The `Serialize`/`Deserialize` impls make this a **machine contract**:
/// `cs peek --json` publishes the tier as its `snake_case` name. That is
/// admissible only because the tier is settled — a fixed set of variants,
/// fixed thresholds, `Ord` already load-bearing — and it is not the object
/// under redesign. A consumer re-deriving the tier from `last_activity`
/// alone would re-derive it wrongly, because [`classify`](Self::classify)
/// is fed the max of the tmux and molecule clocks, not the tmux clock
/// alone — and because [`Harvestable`](Self::Harvestable) is not derived
/// from a clock at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatTier {
    /// The work is finished and the molecule is waiting for `cs done` —
    /// `Completed` on disk and not yet archived.
    ///
    /// Not a liveness reading, and deliberately not derived from one: a
    /// post-completion tmux session that nobody has torn down keeps
    /// bumping `#{session_activity}`, so classifying it by the clock
    /// renders 🟢 *active* on a molecule where nothing is running. That is
    /// the one reading an operator cannot recover from — a finished
    /// molecule that looks like work in flight is never harvested.
    ///
    /// Sorts lowest because it competes for no liveness attention: the
    /// operator reaches these rows through `cs peek --phase harvestable`,
    /// which selects exactly this set, not by scanning the activity sort.
    /// Peer of [`Orphaned`](Self::Orphaned), which is likewise an operator
    /// gesture wearing a liveness tier's clothes.
    Harvestable,
    /// The molecule is marked running in state, but no live tmux session
    /// exists. The worker is gone; this row needs `cs done` or `cs purge`.
    Orphaned,
    /// Tmux session alive but no output for more than 30 minutes.
    Stalled,
    /// Activity within the last 30 minutes.
    Quiet,
    /// Activity within the last 5 minutes.
    Idle,
    /// Activity within the last 30 seconds — the worker is producing output.
    Active,
}

impl HeartbeatTier {
    /// Every tier, liveliest first — the order an operator-facing legend
    /// reads them in.
    ///
    /// Exists so the `cs peek` glyph legend can be derived from the enum
    /// rather than transcribed beside it; a transcribed legend rots the
    /// first time a tier is added.
    pub const ALL: &'static [HeartbeatTier] = &[
        Self::Active,
        Self::Idle,
        Self::Quiet,
        Self::Stalled,
        Self::Orphaned,
        Self::Harvestable,
    ];

    /// Classify a session by its last-activity timestamp, given `now` as the
    /// current time. Returns [`HeartbeatTier::Stalled`] if no activity has
    /// ever been observed on a live session.
    ///
    /// Never returns [`Harvestable`](Self::Harvestable): that tier is a fact
    /// about the *molecule* (completed and unarchived), not about a clock,
    /// so it is applied by the caller that holds the molecule — see
    /// `cs peek`'s `snapshot_to_rows`.
    #[must_use]
    pub fn classify(last_activity: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        let Some(ts) = last_activity else {
            return Self::Stalled;
        };
        let secs = now.signed_duration_since(ts).num_seconds().max(0);
        if secs <= 30 {
            Self::Active
        } else if secs <= 300 {
            Self::Idle
        } else if secs <= 1800 {
            Self::Quiet
        } else {
            Self::Stalled
        }
    }

    /// Glyph used by the TUI heartbeat column.
    ///
    /// Red (🔴) is **forbidden** for `Stalled` and `Quiet`: a live tmux with
    /// no recent output is a quiet worker, not a broken one. Red is reserved
    /// for genuine ghosts — `Orphaned` already owns its own 💀 token, and the
    /// `RowKind::Ghost` / `RowKind::Drift` path at the table-row level is
    /// where alarming reds belong. See ADR-052 (one-ledger-one-writer) and
    /// the chronicle §"2026-04-19 — La pastille
    /// qui ne ment plus" for the visual charter this enforces.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Active => "🟢",
            Self::Idle => "🟡",
            Self::Quiet | Self::Stalled => "⚪",
            Self::Orphaned => "💀",
            Self::Harvestable => "🌾",
        }
    }

    /// Short human label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Quiet => "quiet",
            Self::Stalled => "stalled",
            Self::Orphaned => "orphaned",
            Self::Harvestable => "harvestable",
        }
    }
}

/// Filter predicate for [`crate::aggregate::FleetSnapshot::list_sessions`].
#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    /// Restrict to sessions whose project root equals this value.
    pub project_root: Option<String>,
    /// Restrict to sessions on this tmux socket.
    pub socket: Option<String>,
    /// Restrict to sessions whose name contains this substring.
    pub name_contains: Option<String>,
}

impl SessionFilter {
    /// Returns true if `session` matches every populated field of this filter.
    #[must_use]
    pub fn matches(&self, session: &Session) -> bool {
        if let Some(root) = &self.project_root {
            if &session.project_root != root {
                return false;
            }
        }
        if let Some(socket) = &self.socket {
            if &session.socket != socket {
                return false;
            }
        }
        if let Some(needle) = &self.name_contains {
            if !session.name.contains(needle) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            name: "cosmon-task-abc".into(),
            socket: "/private/tmp/tmux-501/default".into(),
            project_root: "/Users/x/dev/cosmon".into(),
            molecule_id: Some("mol-abc".into()),
            worker_id: Some("w-1".into()),
            last_activity: None,
        }
    }

    #[test]
    fn heartbeat_tier_classify_buckets() {
        let now = Utc::now();
        assert_eq!(
            HeartbeatTier::classify(Some(now - chrono::Duration::seconds(5)), now),
            HeartbeatTier::Active
        );
        assert_eq!(
            HeartbeatTier::classify(Some(now - chrono::Duration::minutes(2)), now),
            HeartbeatTier::Idle
        );
        assert_eq!(
            HeartbeatTier::classify(Some(now - chrono::Duration::minutes(10)), now),
            HeartbeatTier::Quiet
        );
        assert_eq!(
            HeartbeatTier::classify(Some(now - chrono::Duration::hours(2)), now),
            HeartbeatTier::Stalled
        );
        assert_eq!(HeartbeatTier::classify(None, now), HeartbeatTier::Stalled);
    }

    #[test]
    fn heartbeat_tier_ordering_active_beats_others() {
        assert!(HeartbeatTier::Active > HeartbeatTier::Idle);
        assert!(HeartbeatTier::Idle > HeartbeatTier::Quiet);
        assert!(HeartbeatTier::Quiet > HeartbeatTier::Stalled);
        assert!(HeartbeatTier::Stalled > HeartbeatTier::Orphaned);
    }

    /// Regression gate for the `RowKind` visual charter (ADR-052, chronicle
    /// 2026-04-19 "La pastille qui ne ment plus"). Red is reserved for
    /// genuine ghosts; a quiet-but-live worker must never render 🔴.
    #[test]
    fn heartbeat_tier_glyph_never_red_for_quiet_or_stalled() {
        assert_ne!(HeartbeatTier::Quiet.glyph(), "🔴");
        assert_ne!(HeartbeatTier::Stalled.glyph(), "🔴");
        assert_eq!(HeartbeatTier::Quiet.glyph(), "⚪");
        assert_eq!(HeartbeatTier::Stalled.glyph(), "⚪");
    }

    #[test]
    fn empty_filter_matches_everything() {
        assert!(SessionFilter::default().matches(&sample()));
    }

    #[test]
    fn project_root_filter_discriminates() {
        let f = SessionFilter {
            project_root: Some("/other".into()),
            ..SessionFilter::default()
        };
        assert!(!f.matches(&sample()));
    }

    #[test]
    fn name_contains_matches_substring() {
        let f = SessionFilter {
            name_contains: Some("task-abc".into()),
            ..SessionFilter::default()
        };
        assert!(f.matches(&sample()));
    }
}
