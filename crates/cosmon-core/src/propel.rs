// SPDX-License-Identifier: AGPL-3.0-only

//! Propulsion admission control — *when* is a nudge legitimate.
//!
//! # One judge, every channel
//!
//! Cosmon can push an unbidden sentence into a live worker's terminal from
//! several organs: the propulsion tier of `cs patrol --propel`, the briefing
//! tier of `cs patrol --nudge`, and the `A2` re-engagement remedy of
//! `cs patrol --heal`. Each of them once carried its **own copy** of the
//! heuristic "does this worker look idle?", and a repair applied to one copy
//! left the others spamming — exactly what happened on 2026-07-19, when the
//! `patrol.rs` propulsion fix (task-20260719-00ed) left every sibling emitter
//! untouched.
//!
//! [`decide_nudge`] is therefore the **single** admission gate. An emitter
//! does not get to ask "is this worker idle?" itself; it assembles a
//! [`NudgeView`] and obeys the verdict. Adding a new channel means adding a
//! [`NudgeChannel`] variant, not a new heuristic.
//!
//! # The gate that outranks every clock: `awaiting-operator`
//!
//! A worker that has finished its work and is holding a queue of atomic
//! questions for the operator is **not idle** — it is *waiting*, correctly, at
//! a boundary cosmon deliberately put there (ADR-123: `cs await-operator`,
//! [`crate::operator_block::AWAITING_OP_TAG`]). Both of its clocks look dead
//! (no progress events, a silent terminal), so every idleness heuristic
//! mistakes the gate for a stall.
//!
//! The harm is not merely noise. A worker parked at an operator gate and told
//! "continue execution immediately" every few minutes is being subjected to a
//! slow, repeated pressure toward taking the very action the gate exists to
//! withhold. The worker that first reported this named it exactly: *a nudge
//! that repeats indefinitely at a gated worker is a slow pressure toward
//! taking the gated action*. A safety boundary that erodes under repetition is
//! not a boundary.
//!
//! So [`NudgeSkip::AwaitingOperator`] is checked **before** every other rule,
//! including the pane clock — it is the one verdict where being wrong in the
//! permissive direction is not a wasted token but a defeated gate. Its peer,
//! [`NudgeSkip::NotRunning`], covers the same shape reached by a different
//! road: a molecule already `Completed` has no step left to continue, and one
//! that is `Starved` or `Frozen` must never be re-prompted (see
//! [`crate::molecule::MoleculeStatus::Starved`]).
//!
//! # Why the rest of this module exists
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
//! Correspondingly, an *unknown* pane clock ([`NudgeView::pane_idle`] =
//! `None`) is not treated as "idle": it falls through to the progress clock
//! alone, i.e. exactly the pre-fix behaviour, still under backoff.

use chrono::Duration;
use serde::{Deserialize, Serialize};

use crate::molecule::MoleculeStatus;

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

/// Which organ is asking to speak into a worker's terminal.
///
/// Carried purely so a verdict can be *attributed* in reports and tests —
/// the gate rules themselves are channel-independent by design. Per-channel
/// tuning belongs in the `stale_after` the emitter passes, never in a second
/// copy of the idleness heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NudgeChannel {
    /// `cs patrol --propel` — "you appear idle mid-molecule, continue".
    Propulsion,
    /// `cs patrol --nudge` — "re-read your briefing and continue".
    Briefing,
    /// `cs patrol --heal`, remedy `A2` — re-engagement after a diagnosis.
    Heal,
}

/// Everything the judge needs about one candidate worker, pre-digested by the
/// shell.
///
/// Deliberately holds **no pane text** (ADR-137 §2) — only durations, a status,
/// and a boolean the worker itself emitted through `cs await-operator`. See the
/// module docs for why a pane *duration* is admissible where a pane *string*
/// is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NudgeView {
    /// Which emitter is asking. Reported back, never used to weaken a rule.
    pub channel: NudgeChannel,
    /// The molecule's lifecycle status. Anything but `Running` means there is
    /// no step for a nudge to resume.
    pub status: MoleculeStatus,
    /// The worker declared an operator gate (ADR-123) and is holding questions.
    /// Outranks every clock below — see the module docs.
    pub awaiting_operator: bool,
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
pub enum NudgeSkip {
    /// The worker is parked at an operator gate it emitted itself. Repeating
    /// "continue execution immediately" at it is pressure against the gate,
    /// not re-engagement. The highest-priority refusal.
    AwaitingOperator,
    /// The molecule is not `Running`: there is no current step to resume, and
    /// for `Starved` a re-prompt is actively counter-productive.
    NotRunning {
        /// The status observed instead of `Running`.
        status: MoleculeStatus,
    },
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
pub enum NudgeDecision {
    /// Send the propulsion nudge.
    Nudge {
        /// 1-based ordinal of the nudge about to be sent.
        attempt: u32,
        /// Seconds that must elapse before the *next* nudge would be allowed.
        next_window_secs: i64,
    },
    /// Do nothing this pass.
    Skip(NudgeSkip),
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

/// The **single** admission gate every nudge emitter must pass through.
///
/// The caller has already established that the molecule is assigned and stale
/// by progress. This decides whether that staleness *means* the worker is
/// idle, and whether a nudge is owed right now.
///
/// Order matters, and it is the order of increasing cost of being wrong:
///
/// 1. an operator gate (nudging through it erodes a safety boundary — the
///    worst outcome, and the only one that is not merely wasteful),
/// 2. a non-`Running` status (there is no step to continue),
/// 3. an active pane (nudging a thinking worker — the 2026-07-19 spam),
/// 4. the attempt ceiling (so an exhausted molecule reports `Escalate` rather
///    than a perpetual `Backoff`),
/// 5. the spacing window.
#[must_use]
pub fn decide_nudge(view: &NudgeView, stale_after: Duration) -> NudgeDecision {
    let threshold = stale_after.num_seconds().max(1);

    // (1) The operator gate. Checked before any clock, because both clocks of
    // a correctly-gated worker read "dead" and every heuristic below would
    // mistake the boundary for a stall.
    if view.awaiting_operator {
        return NudgeDecision::Skip(NudgeSkip::AwaitingOperator);
    }

    // (2) Nothing to continue. A Completed molecule's worker is done; a
    // Starved one must not be re-prompted (ADR-062); Frozen is a deliberate
    // suspension a nudge has no authority to lift.
    if view.status != MoleculeStatus::Running {
        return NudgeDecision::Skip(NudgeSkip::NotRunning {
            status: view.status,
        });
    }

    // (3) False-idle repair. A terminal that spoke more recently than the
    // staleness threshold belongs to a worker that is working.
    if let Some(idle) = view.pane_idle {
        let idle_secs = idle.num_seconds();
        if idle_secs < threshold {
            return NudgeDecision::Skip(NudgeSkip::PaneActive {
                idle_secs,
                threshold_secs: threshold,
            });
        }
    }

    // (4) Ceiling. Four ignored nudges are a structural fault, not a volume
    // problem; escalate instead of repeating.
    if view.attempts >= PROPEL_MAX_ATTEMPTS {
        return NudgeDecision::Escalate {
            attempts: view.attempts,
        };
    }

    // (5) Spacing. The first nudge is immediate; each later one waits twice as
    // long as the one before.
    let window = propel_backoff(view.attempts, stale_after);
    if let Some(since) = view.since_last_propel {
        if since < window {
            return NudgeDecision::Skip(NudgeSkip::Backoff {
                since_secs: since.num_seconds(),
                window_secs: window.num_seconds(),
                attempts: view.attempts,
            });
        }
    }

    NudgeDecision::Nudge {
        attempt: view.attempts + 1,
        next_window_secs: propel_backoff(view.attempts + 1, stale_after).num_seconds(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default staleness threshold `cs patrol --propel` ships with.
    const STALE: Duration = Duration::seconds(300);

    fn view(progress: i64, pane: Option<i64>, attempts: u32, since: Option<i64>) -> NudgeView {
        NudgeView {
            channel: NudgeChannel::Propulsion,
            status: MoleculeStatus::Running,
            awaiting_operator: false,
            progress_age: Duration::seconds(progress),
            pane_idle: pane.map(Duration::seconds),
            attempts,
            since_last_propel: since.map(Duration::seconds),
        }
    }

    /// Every channel that can speak into a worker's terminal, so the gate
    /// tests below assert *universally* rather than for propulsion alone —
    /// the regression being fixed was precisely a per-organ repair.
    const CHANNELS: [NudgeChannel; 3] = [
        NudgeChannel::Propulsion,
        NudgeChannel::Briefing,
        NudgeChannel::Heal,
    ];

    /// The 2026-07-19 (worker a850) regression: a worker that finished its
    /// work and is holding atomic questions for the operator has both clocks
    /// cold and a spent-looking ledger, yet must receive **zero** nudges — on
    /// **every** channel, at **every** attempt count, forever.
    #[test]
    fn worker_awaiting_operator_is_never_nudged_on_any_channel() {
        for channel in CHANNELS {
            for attempts in 0..=PROPEL_MAX_ATTEMPTS {
                // Hours of silence on both clocks, nudge ledger long expired:
                // every other rule in the judge would say "speak".
                let mut v = view(9_000, Some(9_000), attempts, Some(9_000));
                v.channel = channel;
                v.awaiting_operator = true;
                assert_eq!(
                    decide_nudge(&v, STALE),
                    NudgeDecision::Skip(NudgeSkip::AwaitingOperator),
                    "{channel:?} nudged a gated worker at attempt {attempts}"
                );
            }
        }
    }

    /// The gate outranks the clocks *in both directions*: it holds whether the
    /// molecule is still `Running` (blocked mid-step) or already `Completed`
    /// (work done, questions pending — the literal field observation).
    #[test]
    fn operator_gate_holds_across_statuses() {
        for status in [MoleculeStatus::Running, MoleculeStatus::Completed] {
            let mut v = view(9_000, Some(9_000), 0, None);
            v.status = status;
            v.awaiting_operator = true;
            assert_eq!(
                decide_nudge(&v, STALE),
                NudgeDecision::Skip(NudgeSkip::AwaitingOperator),
                "{status:?} + operator gate produced a nudge"
            );
        }
    }

    /// Sixty patrol passes over seventy minutes at a gated worker — the exact
    /// shape that delivered "des dizaines de nudges" — must produce not one
    /// nudge and must never drift into `Escalate` either (escalation would tag
    /// the molecule as a health anomaly when it is perfectly healthy).
    #[test]
    fn gated_worker_survives_seventy_minutes_of_passes() {
        for pass in 0..60_i64 {
            let mut v = view(300 + pass * 70, Some(300 + pass * 70), 0, None);
            v.awaiting_operator = true;
            assert_eq!(
                decide_nudge(&v, STALE),
                NudgeDecision::Skip(NudgeSkip::AwaitingOperator),
                "pass {pass} broke the gate"
            );
        }
    }

    /// A molecule that is not `Running` has no current step for a nudge to
    /// resume. `Starved` is the sharpest case: ADR-062 says a re-prompt is not
    /// merely useless there but may compound the throttle.
    #[test]
    fn non_running_statuses_are_never_nudged() {
        for status in [
            MoleculeStatus::Completed,
            MoleculeStatus::Collapsed,
            MoleculeStatus::Frozen,
            MoleculeStatus::Starved,
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
        ] {
            let mut v = view(9_000, Some(9_000), 0, None);
            v.status = status;
            assert_eq!(
                decide_nudge(&v, STALE),
                NudgeDecision::Skip(NudgeSkip::NotRunning { status }),
                "{status:?} was nudged"
            );
        }
    }

    /// The gates must not become a blanket mute: a genuinely stalled, running,
    /// un-gated worker is still rescued on every channel.
    #[test]
    fn ungated_running_worker_is_still_nudged_on_any_channel() {
        for channel in CHANNELS {
            let mut v = view(400, Some(400), 0, None);
            v.channel = channel;
            assert!(
                matches!(
                    decide_nudge(&v, STALE),
                    NudgeDecision::Nudge { attempt: 1, .. }
                ),
                "{channel:?} refused a legitimate rescue"
            );
        }
    }

    /// The regression this module was written for: a worker deep in a long
    /// reasoning turn emits no cosmon events (progress 11 min stale) but its
    /// terminal is streaming (silent 2 s). It must receive **zero** nudges.
    #[test]
    fn thinking_worker_is_never_nudged() {
        let d = decide_nudge(&view(660, Some(2), 0, None), STALE);
        assert_eq!(
            d,
            NudgeDecision::Skip(NudgeSkip::PaneActive {
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
            let d = decide_nudge(&view(300 + pass * 70, Some(3), 0, None), STALE);
            assert!(
                matches!(d, NudgeDecision::Skip(NudgeSkip::PaneActive { .. })),
                "pass {pass} nudged a working worker: {d:?}"
            );
        }
    }

    /// A genuinely silent worker — both clocks cold — is nudged on the first
    /// pass, with no waiting.
    #[test]
    fn truly_stale_worker_is_nudged_immediately() {
        assert_eq!(
            decide_nudge(&view(400, Some(400), 0, None), STALE),
            NudgeDecision::Nudge {
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
            decide_nudge(&view(400, None, 0, None), STALE),
            NudgeDecision::Nudge { attempt: 1, .. }
        ));
    }

    /// The spam shape, directly: nudged 70 s ago, still stale. The second nudge
    /// is not owed until 600 s have passed.
    #[test]
    fn second_nudge_waits_for_the_doubled_window() {
        assert_eq!(
            decide_nudge(&view(900, Some(900), 1, Some(70)), STALE),
            NudgeDecision::Skip(NudgeSkip::Backoff {
                since_secs: 70,
                window_secs: 600,
                attempts: 1,
            })
        );
        assert!(matches!(
            decide_nudge(&view(1200, Some(1200), 1, Some(605)), STALE),
            NudgeDecision::Nudge { attempt: 2, .. }
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
            decide_nudge(
                &view(9000, Some(9000), PROPEL_MAX_ATTEMPTS, Some(9000)),
                STALE
            ),
            NudgeDecision::Escalate {
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
            match decide_nudge(&v, stale_after) {
                NudgeDecision::Nudge { attempt, .. } => {
                    nudges += 1;
                    assert_eq!(attempt, attempts + 1);
                    attempts += 1;
                    since_last = Some(0);
                }
                NudgeDecision::Skip(NudgeSkip::Backoff { .. }) => {
                    since_last = since_last.map(|s| s + 70);
                }
                NudgeDecision::Skip(NudgeSkip::PaneActive { .. }) => {
                    unreachable!("pane is as cold as the progress clock")
                }
                NudgeDecision::Skip(NudgeSkip::AwaitingOperator | NudgeSkip::NotRunning { .. }) => {
                    unreachable!("this worker is Running and holds no operator gate")
                }
                NudgeDecision::Escalate { .. } => {
                    escalated = true;
                    break;
                }
            }
        }
        assert_eq!(nudges, PROPEL_MAX_ATTEMPTS as usize);
        assert!(escalated, "ceiling never reached in 70 minutes");
    }
}
