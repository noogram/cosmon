// SPDX-License-Identifier: AGPL-3.0-only

//! Propulsion admission control — *when* is a nudge legitimate.
//!
//! # Why this module exists
//!
//! `cs patrol --propel` re-engages a worker whose molecule stopped making
//! progress. Until this module, the whole decision was one comparison:
//! `now - updated_at >= stale_after` ⇒ send the nudge. That rule has two
//! independent defects, both observed live on 2026-07-19 (release v0.2.0
//! session), where a single worker in a long reasoning turn — the pane
//! rendering `Cultivating… 10m44s`, tokens streaming, nothing wrong with it —
//! collected **nine identical propulsion nudges in ten minutes**:
//!
//! 1. **False-idle.** Progress staleness is a *control-plane* clock. A worker
//!    thinking for twelve minutes inside one step emits no cosmon event, so
//!    the control plane cannot tell it apart from a worker parked at a dead
//!    prompt. Absence of events is not evidence of idleness.
//! 2. **No backoff.** Even against a genuinely stalled worker, re-sending the
//!    *same* sentence every tick forever is spam: it burns the worker's
//!    context, costs tokens on every re-read, and — if the worker is stuck for
//!    a reason a sentence cannot fix — will never work on the ninth try
//!    either. Repetition is not remediation.
//!
//! The cure for (1) is a second, *orthogonal* clock: how long has the worker's
//! terminal been silent. The cure for (2) is exponential spacing plus a hard
//! attempt ceiling that converts a hopeless nudge loop into a single escalation.
//!
//! # The pane-activity clock is a duration, never a glyph (ADR-137 §2)
//!
//! ADR-137 forbids deciding a worker's *act* from rendered pane text: a guard
//! that recognises its target by a string arrests every worker that merely
//! prints the string. That prohibition is about **meaning read out of
//! worker-authored bytes**. This module reads no bytes and no meaning — only
//! *when the terminal last produced any output at all*, a monotonic clock the
//! transport (tmux `session_activity`, an adapter session-log mtime) maintains
//! about the worker rather than a sentence the worker composes.
//!
//! The use/mention hazard is therefore structurally absent, and the failure
//! direction is safe in the one way that matters: a worker can only ever make
//! this clock *fresher* (by producing output), and a fresh clock only ever
//! *suppresses* a nudge. There is no string a worker can print to make patrol
//! poke it, nor to make patrol kill it — the signal gates one advisory
//! sentence, never a lifecycle transition.
//!
//! Correspondingly, an *unknown* pane clock ([`PropelView::pane_idle`] =
//! `None`) is not treated as "idle": it falls through to the progress clock
//! alone, i.e. exactly the pre-fix behaviour, still under backoff.

use chrono::Duration;
use serde::{Deserialize, Serialize};

/// Upper bound on the exponential backoff between two propulsion nudges for
/// the same stalled step.
///
/// Without a cap the doubling runs away (a molecule stalled overnight would
/// schedule its next nudge days out, so a worker that *did* become
/// re-nudgeable would never be reached). Thirty minutes keeps the worst-case
/// re-engagement latency inside one step's stall budget.
pub const PROPEL_BACKOFF_CAP: Duration = Duration::minutes(30);

/// How many propulsion nudges one stalled step may receive before patrol stops
/// repeating itself and escalates.
///
/// Four attempts under doubling spans roughly `stale_after × 15` of wall clock
/// (with the default 300 s threshold: ~0, 10, 30, and 60 minutes in). A worker
/// that ignored four spaced-out nudges is not going to answer the fifth; the
/// fault is structural and belongs to a human or to `cs patrol --heal`, not to
/// a louder sentence.
pub const PROPEL_MAX_ATTEMPTS: u32 = 4;

/// The two clocks and the attempt ledger patrol needs to decide one molecule's
/// propulsion, pre-digested by the shell.
///
/// Deliberately holds **no pane text** (ADR-137 §2) — only durations. See the
/// module docs for why a pane *duration* is admissible where a pane *string*
/// is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropelView {
    /// Control-plane clock: how long since the molecule recorded progress
    /// (`now - updated_at`). This is what made the molecule a candidate.
    pub progress_age: Duration,
    /// Transport clock: how long the worker's terminal has produced nothing.
    /// `None` when the transport cannot report it (no backend, unsupported
    /// adapter) — decided as "unknown", never as "idle".
    pub pane_idle: Option<Duration>,
    /// Nudges already delivered for *this* stall (reset by the shell whenever
    /// the molecule makes real progress).
    pub attempts: u32,
    /// How long since the last nudge for this stall; `None` when none was ever
    /// sent.
    pub since_last_propel: Option<Duration>,
}

/// Why patrol declined to nudge a stale-by-progress molecule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "skip", rename_all = "snake_case")]
pub enum PropelSkip {
    /// The worker's terminal produced output recently: it is thinking or
    /// streaming, not idle. The false-idle repair.
    PaneActive {
        /// Seconds of terminal silence observed.
        idle_secs: i64,
        /// Silence required before the worker counts as idle.
        threshold_secs: i64,
    },
    /// A nudge is due eventually, but not yet — the exponential window from
    /// the previous nudge has not elapsed.
    Backoff {
        /// Seconds since the previous nudge.
        since_secs: i64,
        /// Seconds that must elapse before the next one.
        window_secs: i64,
        /// Nudges already delivered for this stall.
        attempts: u32,
    },
}

/// What patrol should do with one stale-by-progress molecule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PropelDecision {
    /// Send the propulsion nudge.
    Nudge {
        /// 1-based ordinal of the nudge about to be sent.
        attempt: u32,
        /// Seconds that must elapse before the *next* nudge would be allowed.
        next_window_secs: i64,
    },
    /// Do nothing this pass.
    Skip(PropelSkip),
    /// The attempt ceiling is spent: stop nudging, surface the molecule as a
    /// health anomaly for `cs patrol --heal` / a human instead.
    Escalate {
        /// Nudges delivered before giving up.
        attempts: u32,
    },
}

/// The spacing required before the nudge numbered `attempts + 1`.
///
/// Doubles per delivered nudge, starting at `stale_after`, saturating at
/// [`PROPEL_BACKOFF_CAP`]. Computed in seconds with a checked shift so a large
/// `attempts` cannot overflow into a tiny window (which would restore the spam).
#[must_use]
pub fn propel_backoff(attempts: u32, stale_after: Duration) -> Duration {
    let base = stale_after.num_seconds().max(1);
    let cap = PROPEL_BACKOFF_CAP.num_seconds();
    let factor = 1_i64.checked_shl(attempts.min(62)).unwrap_or(i64::MAX);
    let window = base.checked_mul(factor).unwrap_or(i64::MAX);
    Duration::seconds(window.min(cap))
}

/// Pure admission control for one propulsion candidate.
///
/// The caller has already established that the molecule is `Running`, assigned,
/// and stale by progress. This decides whether that staleness *means* the
/// worker is idle, and whether a nudge is owed right now.
///
/// Order matters, and it is the order of increasing cost of being wrong:
/// an active pane is checked first (nudging a working worker is the observed
/// harm), the ceiling next (so an exhausted molecule reports `Escalate` rather
/// than a perpetual `Backoff`), the window last.
#[must_use]
pub fn decide_propel(view: &PropelView, stale_after: Duration) -> PropelDecision {
    let threshold = stale_after.num_seconds().max(1);

    // (1) False-idle repair. A terminal that spoke more recently than the
    // staleness threshold belongs to a worker that is working.
    if let Some(idle) = view.pane_idle {
        let idle_secs = idle.num_seconds();
        if idle_secs < threshold {
            return PropelDecision::Skip(PropelSkip::PaneActive {
                idle_secs,
                threshold_secs: threshold,
            });
        }
    }

    // (2) Ceiling. Four ignored nudges are a structural fault, not a volume
    // problem; escalate instead of repeating.
    if view.attempts >= PROPEL_MAX_ATTEMPTS {
        return PropelDecision::Escalate {
            attempts: view.attempts,
        };
    }

    // (3) Spacing. The first nudge is immediate; each later one waits twice as
    // long as the one before.
    let window = propel_backoff(view.attempts, stale_after);
    if let Some(since) = view.since_last_propel {
        if since < window {
            return PropelDecision::Skip(PropelSkip::Backoff {
                since_secs: since.num_seconds(),
                window_secs: window.num_seconds(),
                attempts: view.attempts,
            });
        }
    }

    PropelDecision::Nudge {
        attempt: view.attempts + 1,
        next_window_secs: propel_backoff(view.attempts + 1, stale_after).num_seconds(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default staleness threshold `cs patrol --propel` ships with.
    const STALE: Duration = Duration::seconds(300);

    fn view(progress: i64, pane: Option<i64>, attempts: u32, since: Option<i64>) -> PropelView {
        PropelView {
            progress_age: Duration::seconds(progress),
            pane_idle: pane.map(Duration::seconds),
            attempts,
            since_last_propel: since.map(Duration::seconds),
        }
    }

    /// The regression this module was written for: a worker deep in a long
    /// reasoning turn emits no cosmon events (progress 11 min stale) but its
    /// terminal is streaming (silent 2 s). It must receive **zero** nudges.
    #[test]
    fn thinking_worker_is_never_nudged() {
        let d = decide_propel(&view(660, Some(2), 0, None), STALE);
        assert_eq!(
            d,
            PropelDecision::Skip(PropelSkip::PaneActive {
                idle_secs: 2,
                threshold_secs: 300,
            })
        );
    }

    /// …and it stays un-nudged however many passes patrol makes, because the
    /// pane clock is re-read every pass and never advances toward the threshold
    /// while the worker keeps working.
    #[test]
    fn thinking_worker_survives_repeated_passes() {
        for pass in 0..9 {
            let d = decide_propel(&view(300 + pass * 70, Some(3), 0, None), STALE);
            assert!(
                matches!(d, PropelDecision::Skip(PropelSkip::PaneActive { .. })),
                "pass {pass} nudged a working worker: {d:?}"
            );
        }
    }

    /// A genuinely silent worker — both clocks cold — is nudged on the first
    /// pass, with no waiting.
    #[test]
    fn truly_stale_worker_is_nudged_immediately() {
        assert_eq!(
            decide_propel(&view(400, Some(400), 0, None), STALE),
            PropelDecision::Nudge {
                attempt: 1,
                next_window_secs: 600,
            }
        );
    }

    /// An unknown pane clock degrades to the pre-fix progress-only rule rather
    /// than suppressing the nudge: patrol must still rescue workers on
    /// transports that cannot report terminal activity.
    #[test]
    fn unknown_pane_activity_still_nudges() {
        assert!(matches!(
            decide_propel(&view(400, None, 0, None), STALE),
            PropelDecision::Nudge { attempt: 1, .. }
        ));
    }

    /// The spam shape, directly: nudged 70 s ago, still stale. The second nudge
    /// is not owed until 600 s have passed.
    #[test]
    fn second_nudge_waits_for_the_doubled_window() {
        assert_eq!(
            decide_propel(&view(900, Some(900), 1, Some(70)), STALE),
            PropelDecision::Skip(PropelSkip::Backoff {
                since_secs: 70,
                window_secs: 600,
                attempts: 1,
            })
        );
        assert!(matches!(
            decide_propel(&view(1200, Some(1200), 1, Some(605)), STALE),
            PropelDecision::Nudge { attempt: 2, .. }
        ));
    }

    /// Windows double per delivered nudge and saturate at the cap — never
    /// wrapping back to a small value on a large attempt count.
    #[test]
    fn backoff_doubles_then_saturates() {
        assert_eq!(propel_backoff(0, STALE), Duration::seconds(300));
        assert_eq!(propel_backoff(1, STALE), Duration::seconds(600));
        assert_eq!(propel_backoff(2, STALE), Duration::seconds(1200));
        assert_eq!(propel_backoff(3, STALE), PROPEL_BACKOFF_CAP);
        assert_eq!(propel_backoff(99, STALE), PROPEL_BACKOFF_CAP);
    }

    /// Past the ceiling patrol stops talking and hands the molecule to the
    /// healer — the escalation is reported even though the backoff window has
    /// long elapsed.
    #[test]
    fn exhausted_attempts_escalate_instead_of_repeating() {
        assert_eq!(
            decide_propel(
                &view(9000, Some(9000), PROPEL_MAX_ATTEMPTS, Some(9000)),
                STALE
            ),
            PropelDecision::Escalate {
                attempts: PROPEL_MAX_ATTEMPTS,
            }
        );
    }

    /// End-to-end cadence over a genuinely dead worker: exactly
    /// [`PROPEL_MAX_ATTEMPTS`] nudges, spaced by the doubling window, then
    /// escalation forever. The pre-fix code emitted one nudge per pass.
    #[test]
    fn stale_worker_gets_spaced_nudges_then_escalates() {
        let stale_after = STALE;
        let mut attempts = 0_u32;
        let mut since_last: Option<i64> = None;
        let mut nudges = 0;
        let mut escalated = false;
        // 60 passes at 70 s cadence ≈ 70 min, the live observation window.
        for pass in 0..60_i64 {
            let clock = 300 + pass * 70;
            let v = view(clock, Some(clock), attempts, since_last);
            match decide_propel(&v, stale_after) {
                PropelDecision::Nudge { attempt, .. } => {
                    nudges += 1;
                    assert_eq!(attempt, attempts + 1);
                    attempts += 1;
                    since_last = Some(0);
                }
                PropelDecision::Skip(PropelSkip::Backoff { .. }) => {
                    since_last = since_last.map(|s| s + 70);
                }
                PropelDecision::Skip(PropelSkip::PaneActive { .. }) => {
                    unreachable!("pane is as cold as the progress clock")
                }
                PropelDecision::Escalate { .. } => {
                    escalated = true;
                    break;
                }
            }
        }
        assert_eq!(nudges, PROPEL_MAX_ATTEMPTS as usize);
        assert!(escalated, "ceiling never reached in 70 minutes");
    }
}
