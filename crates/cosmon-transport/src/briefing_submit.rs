// SPDX-License-Identifier: AGPL-3.0-only

//! The briefing-submit receipt kernel: poll the composer, re-press submit
//! while it still holds the briefing, and sign delivery only on two
//! consecutive clear readings.
//!
//! One kernel, several callers and budgets: `cs tackle` runs it in band for a
//! few seconds, the detached `cs briefing-backstop` runs it for twenty
//! minutes, and the RPP API's in-process executor runs it after spawning a
//! worker (issue #81). It lives in this crate, next to the composer
//! classifier it consumes, so all of them share one implementation.

use crate::tmux::ComposerState;

/// How many consecutive `Clear` readings sign the delivery receipt.
///
/// One is not enough. The composer repaints — a placeholder can be absent from
/// the single frame `capture-pane` happened to catch mid-redraw — and a single
/// clear frame would retire the loop on a flicker, which is exactly the failure
/// the old code avoided by never trusting `Clear` at all. Two consecutive
/// *successful* captures (an [`ComposerState::Unobservable`] in between resets
/// the count, it does not extend it) cost one extra poll and remove that class.
pub const BRIEFING_CLEAR_CONFIRMATIONS: u8 = 2;

/// Interval between briefing-submit confirmation polls on the **in-band** path.
///
/// The durable backstop polls on its own, slower clock: it is not charged to a
/// dispatcher, and a `capture-pane` per second for twenty minutes is a cost
/// nobody is waiting on but the machine still pays.
pub const BRIEFING_SUBMIT_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// One-step decision for the briefing-submit confirmation loop — the pure
/// kernel of the retry, factored out so the nudge/stop logic is unit-testable
/// without a live tmux server or Claude TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BriefingSubmitStep {
    /// The briefing has left the composer, confirmed by
    /// [`BRIEFING_CLEAR_CONFIRMATIONS`] consecutive successful captures. Stop.
    Delivered,
    /// The briefing is still pasted-but-unsubmitted in the composer. Re-`Enter`.
    Nudge,
    /// Not yet decidable: either the pane could not be read, or the composer has
    /// read clear fewer times than the receipt requires. Look again rather than
    /// injecting a stray `Enter` into a session that may be mid-submit.
    Wait,
}

/// How a briefing-submit confirmation ended.
///
/// Replaces the pre-#26-A `()` return, which conflated "the worker is producing
/// tokens" with "we gave up after 90 s" — the conflation that turned a stuck
/// submit into a silent hang instead of a reportable outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BriefingSubmitOutcome {
    /// The briefing left the composer, on two consecutive readable captures.
    ///
    /// This is the receipt, and the only success. Note what it is *not*: it is
    /// not "the worker looks busy". `Working` is unreachable on Claude Code
    /// 2.1.220, so a delivery condition resting on it never fires and the
    /// dispatcher pays the whole budget every time. What we can always check is
    /// whether the text we wrote is still on screen.
    Delivered,
    /// The budget ran out without a receipt and without a visibly stuck
    /// composer — the pane could not be read well enough to say either way.
    /// Ambiguous, non-fatal, logged.
    Unobservable,
    /// The composer still holds the pasted-but-unsubmitted briefing after the
    /// whole budget. A typed give-up, not a silent one.
    StuckPasted,
    /// The session the briefing was sent to no longer exists.
    ///
    /// Not a failure of delivery and not a success: there is nothing left to
    /// press. Distinguished from [`Unobservable`](Self::Unobservable) because
    /// "the worker is gone" is a fact and "I could not read the pane" is an
    /// absence of one — and because it is the only outcome that lets a
    /// twenty-minute durable budget stop in the first second instead of
    /// nudging a session that will never answer.
    SessionGone,
}

/// Decide whether the confirmation loop may keep going, given how long it has
/// run in total and what this tick decided to do.
///
/// Pure so the deadline is unit-testable without a live tmux server. Two
/// load-bearing properties:
///
/// - a confirmed delivery exits **immediately**, at whatever the clock says.
///   The nominal dispatch therefore costs one poll, not a budget;
/// - a *pending* composer is never abandoned silently — it is nudged for the
///   whole `budget` and then escalated as
///   [`BriefingSubmitOutcome::StuckPasted`].
///
/// One clock, not two. An earlier version had a `quiet` window alongside
/// `total`, both spelled 90 s while the doc comment on one called it the
/// "short" window that "gives up quickly": two names for one number, which is
/// how a reader ends up believing there is a fast path that does not exist.
#[must_use]
pub fn briefing_submit_deadline(
    total: std::time::Duration,
    step: BriefingSubmitStep,
    budget: std::time::Duration,
) -> Option<BriefingSubmitOutcome> {
    match step {
        BriefingSubmitStep::Delivered => Some(BriefingSubmitOutcome::Delivered),
        BriefingSubmitStep::Nudge => {
            (total >= budget).then_some(BriefingSubmitOutcome::StuckPasted)
        }
        BriefingSubmitStep::Wait => {
            (total >= budget).then_some(BriefingSubmitOutcome::Unobservable)
        }
    }
}

/// Decide the next action for the briefing-submit confirmation loop, from the
/// composer reading alone.
///
/// `clear_streak` counts consecutive [`ComposerState::Clear`] readings ending
/// with this one; the caller resets it on any reading that is not `Clear`, so an
/// unreadable pane can never be counted as half a receipt.
///
/// # Why the session status is not a parameter
///
/// It used to be, and `Working` was the loop's only early exit. On Claude Code
/// 2.1.220 that arm is unreachable — every captured frame classifies as
/// `AwaitingHuman`, streaming or idle — so the exit never fired and each
/// dispatch paid the full budget. Deleting the parameter rather than reordering
/// the arms is deliberate: it makes "delivery is proven by the composer, never
/// by a chrome heuristic" a property of the signature, not of a comment that the
/// next classifier repair could quietly invert.
#[must_use]
pub fn briefing_submit_step(state: ComposerState, clear_streak: u8) -> BriefingSubmitStep {
    match state {
        ComposerState::Pending => BriefingSubmitStep::Nudge,
        ComposerState::Clear if clear_streak >= BRIEFING_CLEAR_CONFIRMATIONS => {
            BriefingSubmitStep::Delivered
        }
        // One clear sighting, or a pane we could not read: look again.
        ComposerState::Clear | ComposerState::Unobservable => BriefingSubmitStep::Wait,
    }
}

/// The transport-free core of the briefing-submit retry: poll, decide, nudge,
/// check the deadline, sleep.
///
/// **One loop, two budgets.** `cs tackle` runs it in band for a few seconds so
/// a stuck composer cannot tax a serial dispatcher; the detached
/// `cs briefing-backstop` runs the very same function for twenty minutes, after
/// that dispatcher is gone. The receipt is therefore identical on both paths by
/// construction rather than by review — which matters because the durable path
/// is the one nobody watches.
///
/// `probe` answers `None` when the session has vanished; the loop stops at once
/// with [`BriefingSubmitOutcome::SessionGone`] rather than spending a budget on
/// keystrokes nothing will receive.
///
/// The injected clock and sleep are what make the *wall-clock cost of the loop
/// itself* testable. That cost is a load-bearing property here — the
/// dispatch-blocking regression this seam pins is not about which outcome comes
/// back but about **how long the caller waits for it**, and a test that has to
/// spend real minutes to observe a minutes-long block is not a test anybody
/// runs.
///
/// `now` returns elapsed-since-start, not an absolute instant, so a test can
/// drive virtual time by advancing a counter in `sleep`.
/// # Why there is no seed parameter (COSMON #26-C, withdrawn)
///
/// `cs tackle` briefly handed this loop the `ComposerState` its own paste loop
/// had just seen, so the receipt could start from one confirmation instead of
/// zero. It measured beautifully — 1.09 s of dispatch latency down to 14 ms —
/// and it was wrong.
///
/// The two-consecutive-`Clear` rule was never only about *counting* two
/// readings. Part of what it bought was the [`BRIEFING_SUBMIT_POLL`] of
/// wall-clock BETWEEN them: two looks at a repainting terminal, one second
/// apart, are two independent samples. A seeded look taken ~14 ms before the
/// confirming one is a single sample counted twice, and any transient frame
/// that happens to lack the paste — a redraw carrying a spinner, a scrolled
/// transcript — is then enough to sign a delivery for a briefing still sitting
/// in the composer.
///
/// The duplication that seemed to justify the seam was real but superficial:
/// the same *question* was asked twice. The spacing between the answers was
/// not duplication, it was the evidence. Latency on this path is worth having,
/// and it is not worth having here — the dispatch profile puts the dominant
/// term elsewhere entirely, in the per-dispatch `claude --model <m> -p ping`.
pub fn run_briefing_submit_loop(
    budget: std::time::Duration,
    probe: &mut dyn FnMut() -> Option<ComposerState>,
    nudge: &mut dyn FnMut(),
    now: &mut dyn FnMut() -> std::time::Duration,
    sleep: &mut dyn FnMut(),
) -> BriefingSubmitOutcome {
    // Consecutive `Clear` readings. Reset — not merely left alone — by anything
    // else, so an unreadable frame between two clear ones cannot be spliced
    // into a receipt.
    let mut clear_streak: u8 = 0;
    loop {
        let Some(state) = probe() else {
            return BriefingSubmitOutcome::SessionGone;
        };
        clear_streak = if state == ComposerState::Clear {
            clear_streak.saturating_add(1)
        } else {
            0
        };
        let step = briefing_submit_step(state, clear_streak);
        if step == BriefingSubmitStep::Nudge {
            nudge();
        }
        if let Some(outcome) = briefing_submit_deadline(now(), step, budget) {
            return outcome;
        }
        sleep();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vanished session stops the loop at the first probe, whatever the
    /// budget says. This is what keeps a twenty-minute durable budget from
    /// being spent pressing Enter at a dead worker.
    #[test]
    fn a_vanished_session_stops_the_loop_at_once() {
        let clock = std::cell::Cell::new(std::time::Duration::ZERO);
        let nudges = std::cell::Cell::new(0_usize);
        let outcome = run_briefing_submit_loop(
            std::time::Duration::from_secs(1_200),
            &mut || None,
            &mut || nudges.set(nudges.get() + 1),
            &mut || clock.get(),
            &mut || clock.set(clock.get() + BRIEFING_SUBMIT_POLL),
        );

        assert_eq!(outcome, BriefingSubmitOutcome::SessionGone);
        assert_eq!(clock.get(), std::time::Duration::ZERO);
        assert_eq!(
            nudges.get(),
            0,
            "a dead session must never be sent a keystroke"
        );
    }
}
