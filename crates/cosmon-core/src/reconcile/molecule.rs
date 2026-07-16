// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule health — derived from the molecule's persisted status and
//! its worker's reconciled view.
//!
//! This is the molecule-level analogue of [`crate::worker::reconcile`]:
//! a pure function whose output is **never persisted**, always
//! recomputed. It answers "is this molecule making progress right now?"
//! without splitting [`MoleculeStatus`] — the lifecycle stays minimal,
//! the health is an overlay.
//!
//! # The observation DAG
//!
//! ```text
//! Transport ── worker::reconcile ──► EffectiveStatus
//!                                          │
//!                              molecule_health │
//!                                          ▼
//!                                   MoleculeHealth
//! ```
//!
//! The caller already reconciles the worker (step 1). The molecule's
//! health combines that effective worker status with the molecule's
//! persisted lifecycle status (step 2).
//!
//! # Non-goals
//!
//! - No `Reconcilable` trait. Module convention only (see [`super`]).
//! - No persistence. `MoleculeHealth` is display-only.
//! - No new fields on [`crate::molecule::Molecule`]. This function takes
//!   the minimal inputs it needs — nothing more.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::molecule::MoleculeStatus;
use crate::worker::EffectiveStatus;

/// Display-only health classification for a molecule.
///
/// Computed fresh by [`molecule_health`] from a molecule's persisted
/// status and (optionally) the effective status of the worker that owns
/// it. Never serialized to state files; [`Serialize`] exists only so
/// callers can surface it through `--json` outputs.
///
/// # Variants
///
/// - [`Self::Healthy`] — Running with a worker that reconciles to
///   [`EffectiveStatus::Healthy`]. The happy path.
/// - [`Self::Orphaned`] — Running or Queued but the worker is gone
///   (no effective status passed in, or the worker reconciles to
///   [`EffectiveStatus::Diverged`] / [`EffectiveStatus::Stopped`]).
///   This is the signal an operator should act on first.
/// - [`Self::Stalled`] — Running but the worker is
///   [`EffectiveStatus::Suspect`] (alive, cognitive declaration stale).
/// - [`Self::Blocked`] — Running but the worker is pinned on a
///   permission dialog / trust prompt
///   ([`EffectiveStatus::Blocked`]).
/// - [`Self::Degraded`] — Running but the worker reconciles to an
///   error or a paused state. The molecule will not progress until the
///   worker recovers.
/// - [`Self::Inert`] — No health to report. Pending or Frozen
///   molecules, or Queued molecules without an active worker. These
///   are valid resting states, not problems.
/// - [`Self::Terminal`] — Completed or Collapsed. Terminal states
///   have no health to check; the molecule is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoleculeHealth {
    /// Running with a worker in [`EffectiveStatus::Healthy`].
    Healthy,
    /// Running/Queued but the worker is dead or missing.
    Orphaned,
    /// Running with a worker in [`EffectiveStatus::Suspect`]
    /// (alive but cognitive declaration stale).
    Stalled,
    /// Running with a worker pinned on a permission / trust prompt.
    Blocked,
    /// Running with a worker in an error or paused state.
    Degraded,
    /// Pending, Frozen, or Queued without an active worker — not
    /// currently checkable.
    Inert,
    /// Completed or Collapsed — final state, nothing to monitor.
    Terminal,
}

impl MoleculeHealth {
    /// Single-character glyph for compact operator-facing tables.
    ///
    /// Chosen so the health cell stays one visible column wide even in
    /// monospaced terminals that split wider emoji into two cells.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Healthy => "♥",
            Self::Orphaned => "✖",
            Self::Stalled => "◷",
            Self::Blocked => "⛔",
            Self::Degraded => "!",
            Self::Inert => "·",
            Self::Terminal => "◌",
        }
    }

    /// `true` iff the variant calls for operator attention (stalled,
    /// orphaned, blocked, or degraded). Used to drive highlighting in
    /// fleet tables.
    #[must_use]
    pub fn needs_attention(self) -> bool {
        matches!(
            self,
            Self::Orphaned | Self::Stalled | Self::Blocked | Self::Degraded
        )
    }
}

impl fmt::Display for MoleculeHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => f.write_str("healthy"),
            Self::Orphaned => f.write_str("orphaned"),
            Self::Stalled => f.write_str("stalled"),
            Self::Blocked => f.write_str("blocked"),
            Self::Degraded => f.write_str("degraded"),
            Self::Inert => f.write_str("inert"),
            Self::Terminal => f.write_str("terminal"),
        }
    }
}

/// Compute a molecule's health from its persisted status plus the
/// reconciled effective status of the worker that owns it.
///
/// Pure and infallible — never does I/O, never returns `Result`, and
/// the output is never persisted. The caller is expected to have
/// already reconciled the worker via [`crate::worker::reconcile`] so
/// that `worker` reflects live observations, not stale fleet state.
///
/// Pass `worker = None` when the molecule has no worker bound
/// (pending, freshly queued, or the worker has been purged): the
/// lifecycle status alone determines the result in that case.
///
/// # Classification table
///
/// | molecule status         | worker                        | health      |
/// | ----------------------- | ----------------------------- | ----------- |
/// | `Completed`/`Collapsed` | any                           | `Terminal`  |
/// | `Pending` / `Frozen`    | any                           | `Inert`     |
/// | `Queued`                | `None` / `Healthy` / `Paused` | `Inert`     |
/// | `Queued`                | `Diverged` / `Stopped` / …    | `Orphaned`  |
/// | `Running`               | `None`                        | `Orphaned`  |
/// | `Running`               | `Healthy`                     | `Healthy`   |
/// | `Running`               | `Suspect`                     | `Stalled`   |
/// | `Running`               | `Blocked`                     | `Blocked`   |
/// | `Running`               | `Diverged` / `Stopped`        | `Orphaned`  |
/// | `Running`               | `Paused` / `Error(_)`         | `Degraded`  |
#[must_use]
pub fn molecule_health(status: MoleculeStatus, worker: Option<&EffectiveStatus>) -> MoleculeHealth {
    match status {
        MoleculeStatus::Completed | MoleculeStatus::Collapsed => MoleculeHealth::Terminal,
        MoleculeStatus::Pending | MoleculeStatus::Frozen => MoleculeHealth::Inert,
        // ADR-062 Starved: external authority refused service. The
        // worker is alive but cannot make progress until refresh; treat
        // as degraded for health-purposes (operator must wait or
        // rotate, never re-prompt).
        MoleculeStatus::Starved => MoleculeHealth::Degraded,
        MoleculeStatus::Queued => match worker {
            Some(
                EffectiveStatus::Diverged | EffectiveStatus::Stopped | EffectiveStatus::Error(_),
            ) => MoleculeHealth::Orphaned,
            None | Some(_) => MoleculeHealth::Inert,
        },
        MoleculeStatus::Running => match worker {
            Some(EffectiveStatus::Healthy) => MoleculeHealth::Healthy,
            Some(EffectiveStatus::Suspect) => MoleculeHealth::Stalled,
            Some(EffectiveStatus::Blocked) => MoleculeHealth::Blocked,
            None | Some(EffectiveStatus::Diverged | EffectiveStatus::Stopped) => {
                MoleculeHealth::Orphaned
            }
            Some(EffectiveStatus::Paused | EffectiveStatus::Error(_)) => MoleculeHealth::Degraded,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Terminal ────────────────────────────────────────────────

    #[test]
    fn completed_is_terminal_regardless_of_worker() {
        assert_eq!(
            molecule_health(MoleculeStatus::Completed, None),
            MoleculeHealth::Terminal,
        );
        assert_eq!(
            molecule_health(MoleculeStatus::Completed, Some(&EffectiveStatus::Healthy)),
            MoleculeHealth::Terminal,
        );
    }

    #[test]
    fn collapsed_is_terminal_regardless_of_worker() {
        assert_eq!(
            molecule_health(MoleculeStatus::Collapsed, None),
            MoleculeHealth::Terminal,
        );
        assert_eq!(
            molecule_health(MoleculeStatus::Collapsed, Some(&EffectiveStatus::Diverged)),
            MoleculeHealth::Terminal,
        );
    }

    // ── Pending / Frozen ────────────────────────────────────────

    #[test]
    fn pending_is_inert_regardless_of_worker() {
        assert_eq!(
            molecule_health(MoleculeStatus::Pending, None),
            MoleculeHealth::Inert,
        );
        assert_eq!(
            molecule_health(MoleculeStatus::Pending, Some(&EffectiveStatus::Healthy)),
            MoleculeHealth::Inert,
        );
    }

    #[test]
    fn frozen_is_inert_regardless_of_worker() {
        assert_eq!(
            molecule_health(MoleculeStatus::Frozen, None),
            MoleculeHealth::Inert,
        );
        assert_eq!(
            molecule_health(MoleculeStatus::Frozen, Some(&EffectiveStatus::Healthy)),
            MoleculeHealth::Inert,
        );
    }

    // ── Queued ──────────────────────────────────────────────────

    #[test]
    fn queued_without_worker_is_inert() {
        assert_eq!(
            molecule_health(MoleculeStatus::Queued, None),
            MoleculeHealth::Inert,
        );
    }

    #[test]
    fn queued_with_healthy_worker_is_inert() {
        assert_eq!(
            molecule_health(MoleculeStatus::Queued, Some(&EffectiveStatus::Healthy)),
            MoleculeHealth::Inert,
        );
    }

    #[test]
    fn queued_with_diverged_worker_is_orphaned() {
        assert_eq!(
            molecule_health(MoleculeStatus::Queued, Some(&EffectiveStatus::Diverged)),
            MoleculeHealth::Orphaned,
        );
    }

    // ── Running ─────────────────────────────────────────────────

    #[test]
    fn running_without_worker_is_orphaned() {
        assert_eq!(
            molecule_health(MoleculeStatus::Running, None),
            MoleculeHealth::Orphaned,
        );
    }

    #[test]
    fn running_with_healthy_worker_is_healthy() {
        assert_eq!(
            molecule_health(MoleculeStatus::Running, Some(&EffectiveStatus::Healthy)),
            MoleculeHealth::Healthy,
        );
    }

    #[test]
    fn running_with_suspect_worker_is_stalled() {
        assert_eq!(
            molecule_health(MoleculeStatus::Running, Some(&EffectiveStatus::Suspect)),
            MoleculeHealth::Stalled,
        );
    }

    #[test]
    fn running_with_blocked_worker_is_blocked() {
        assert_eq!(
            molecule_health(MoleculeStatus::Running, Some(&EffectiveStatus::Blocked)),
            MoleculeHealth::Blocked,
        );
    }

    #[test]
    fn running_with_diverged_worker_is_orphaned() {
        assert_eq!(
            molecule_health(MoleculeStatus::Running, Some(&EffectiveStatus::Diverged)),
            MoleculeHealth::Orphaned,
        );
    }

    #[test]
    fn running_with_stopped_worker_is_orphaned() {
        assert_eq!(
            molecule_health(MoleculeStatus::Running, Some(&EffectiveStatus::Stopped)),
            MoleculeHealth::Orphaned,
        );
    }

    #[test]
    fn running_with_paused_worker_is_degraded() {
        assert_eq!(
            molecule_health(MoleculeStatus::Running, Some(&EffectiveStatus::Paused)),
            MoleculeHealth::Degraded,
        );
    }

    #[test]
    fn running_with_errored_worker_is_degraded() {
        assert_eq!(
            molecule_health(
                MoleculeStatus::Running,
                Some(&EffectiveStatus::Error("crash".to_owned())),
            ),
            MoleculeHealth::Degraded,
        );
    }

    // ── Display / serde ────────────────────────────────────────

    #[test]
    fn display_slugs_are_stable() {
        assert_eq!(MoleculeHealth::Healthy.to_string(), "healthy");
        assert_eq!(MoleculeHealth::Orphaned.to_string(), "orphaned");
        assert_eq!(MoleculeHealth::Stalled.to_string(), "stalled");
        assert_eq!(MoleculeHealth::Blocked.to_string(), "blocked");
        assert_eq!(MoleculeHealth::Degraded.to_string(), "degraded");
        assert_eq!(MoleculeHealth::Inert.to_string(), "inert");
        assert_eq!(MoleculeHealth::Terminal.to_string(), "terminal");
    }

    #[test]
    fn serde_roundtrip_all_variants() {
        for h in [
            MoleculeHealth::Healthy,
            MoleculeHealth::Orphaned,
            MoleculeHealth::Stalled,
            MoleculeHealth::Blocked,
            MoleculeHealth::Degraded,
            MoleculeHealth::Inert,
            MoleculeHealth::Terminal,
        ] {
            let json = serde_json::to_string(&h).unwrap();
            let back: MoleculeHealth = serde_json::from_str(&json).unwrap();
            assert_eq!(back, h);
        }
    }

    #[test]
    fn needs_attention_matches_expected_variants() {
        let attention = [
            MoleculeHealth::Orphaned,
            MoleculeHealth::Stalled,
            MoleculeHealth::Blocked,
            MoleculeHealth::Degraded,
        ];
        let quiet = [
            MoleculeHealth::Healthy,
            MoleculeHealth::Inert,
            MoleculeHealth::Terminal,
        ];
        for h in attention {
            assert!(h.needs_attention(), "{h} should need attention");
        }
        for h in quiet {
            assert!(!h.needs_attention(), "{h} should not need attention");
        }
    }

    #[test]
    fn glyphs_are_distinct() {
        let all = [
            MoleculeHealth::Healthy,
            MoleculeHealth::Orphaned,
            MoleculeHealth::Stalled,
            MoleculeHealth::Blocked,
            MoleculeHealth::Degraded,
            MoleculeHealth::Inert,
            MoleculeHealth::Terminal,
        ];
        let glyphs: Vec<_> = all.iter().map(|h| h.glyph()).collect();
        let mut uniq = glyphs.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), glyphs.len(), "glyphs must be unique");
    }

    // ── Purity ─────────────────────────────────────────────────

    #[test]
    fn molecule_health_is_pure() {
        for _ in 0..3 {
            assert_eq!(
                molecule_health(MoleculeStatus::Running, Some(&EffectiveStatus::Healthy)),
                MoleculeHealth::Healthy,
            );
        }
    }
}
