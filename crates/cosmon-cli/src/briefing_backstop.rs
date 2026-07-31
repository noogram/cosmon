// SPDX-License-Identifier: AGPL-3.0-only

//! The briefing-submit receipt kernel, and the durable record that lets it be
//! re-run by a process the dispatcher does not own (COSMON #26-B).
//!
//! # The problem this closes
//!
//! `cs tackle` pastes a briefing into a fresh worker's composer and presses
//! Enter. On a loaded fleet the TUI is still rendering the paste, swallows the
//! keystroke, and the worker sits on `❯ [Pasted text #1 +86 lines]` doing
//! nothing — the 2026-07-20 knowledge-fleet stall, where four of eleven workers
//! burned a fleet slot for zero tokens until a human pressed Enter by hand
//! twenty minutes later.
//!
//! #26-A gave that retry a *receipt*: the briefing text we wrote ourselves is
//! gone from the composer. What it could not give it is *patience*. The retry
//! runs inside `cs tackle`, and `cs tackle` returns in seconds — whatever waits
//! on it (`cs run`, a patrol pass, a fleet loop) is usually serial, so every
//! second spent pressing Enter for one stuck worker is a second the whole fleet
//! does not dispatch. The in-band window is therefore a few short retries, and
//! an earlier attempt to hand the residual patience to a *thread* was a false
//! promise: the thread died with the process it was spawned from, long before
//! the TUI settled.
//!
//! # The shape of the fix
//!
//! Patience that outlives the dispatcher cannot live in the dispatcher. So:
//!
//! 1. when the in-band window closes with the paste still visible, `cs tackle`
//!    writes a [`BriefingPending`] record next to the molecule's state — the
//!    durable half, readable by any later process ([`write()`], [`read()`]);
//! 2. it re-execs itself as a detached `cs briefing-backstop` child, in its own
//!    process group ([`backstop_argv`]), which resumes pressing on a long
//!    budget;
//! 3. the child signs — or fails to sign — the *same* receipt, because the
//!    decision kernel below is one piece of code with two callers and two
//!    budgets, not two implementations that can drift apart.
//!
//! The record is the reason step 3 is possible at all: the needle it stores is
//! what a process with no memory of the briefing needs in order to recognise it
//! still sitting in the composer.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use cosmon_transport::tmux::ComposerState;
use serde::{Deserialize, Serialize};

// ─────────────────────────── the durable record ───────────────────────────

/// File name of the durable briefing-pending record, inside a molecule's state
/// directory.
pub const RECORD_FILE: &str = "briefing-pending.json";

/// Everything a process that never saw the briefing needs in order to keep
/// pressing submit for it.
///
/// Written by `cs tackle` at the moment its in-band window gives up, read by
/// the detached `cs briefing-backstop` child that outlives it. Its presence on
/// disk means exactly one thing: **the last process to look saw an unsubmitted
/// briefing in this worker's composer.** It is removed on a delivery receipt,
/// and only on a delivery receipt.
///
/// # Why the needle and not the briefing
///
/// The composer scan consults exactly one line of the input it was given — the
/// last non-empty one ([`cosmon_transport::tmux::composer_needle`]). Storing
/// the whole briefing would copy `briefing.md`, which already sits in the same
/// directory, into a second file that is neither more precise nor easier to
/// read. Storing the needle is *exact*: handing it back as the `input` of a
/// later composer scan reproduces the identical comparison, because the needle
/// of a needle is itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefingPending {
    /// The molecule whose dispatch left this behind.
    pub molecule: String,
    /// The worker whose composer still holds the briefing. Also the tmux
    /// session name — resolved through the session listing, so a worker that
    /// has since died is recognised rather than nudged into the void.
    pub worker: String,
    /// The tmux socket (`tmux -L <socket>`) the worker lives on. Recorded
    /// rather than re-derived: a detached child inherits no fleet context and
    /// must not guess which server holds the session.
    pub socket: String,
    /// The line the composer scan looks for. See the type docs.
    pub needle: String,
    /// When the in-band window gave up, RFC 3339.
    pub recorded_at: String,
    /// How long the in-band window pressed before recording this, in seconds —
    /// so a reader can tell "gave up after 8 s" from "gave up after 90 s"
    /// without knowing which build wrote the file.
    pub inband_seconds: u64,
    /// Set only when the durable backstop *also* ran out of patience: the
    /// outcome it ended on, as a bare word (`stuck-pasted`, `unobservable`,
    /// `session-gone`). Absent while the record is still live work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backstop_outcome: Option<String>,
    /// When the durable backstop stopped, RFC 3339. Absent while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backstop_ended_at: Option<String>,
    /// How many submit keystrokes the durable backstop landed. Absent while
    /// live; `0` on a record whose composer could never be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backstop_nudges: Option<u64>,
}

/// Where the durable record lives for a molecule whose state directory is
/// `mol_state_dir`.
#[must_use]
pub fn record_path(mol_state_dir: &Path) -> PathBuf {
    mol_state_dir.join(RECORD_FILE)
}

/// Persist `record` for `mol_state_dir`, atomically.
///
/// Write-then-rename, because the reader is a *different process* that may look
/// at any instant: a torn JSON file would be read as "no record" by
/// [`read`] and the backstop would silently do nothing, which is the failure
/// this whole mechanism exists to remove.
///
/// # Errors
///
/// Any filesystem error from creating, writing, or renaming the record.
pub fn write(mol_state_dir: &Path, record: &BriefingPending) -> std::io::Result<()> {
    std::fs::create_dir_all(mol_state_dir)?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = mol_state_dir.join(format!("{RECORD_FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, record_path(mol_state_dir))
}

/// Read the durable record for `mol_state_dir`, if one is there.
///
/// A missing file and an unparseable one both answer `None`: the caller's only
/// sensible response to either is "there is nothing here I can act on", and a
/// backstop that refused to start because a hand-edited file lost a comma would
/// be strictly worse than one that treats it as absent.
#[must_use]
pub fn read(mol_state_dir: &Path) -> Option<BriefingPending> {
    let raw = std::fs::read_to_string(record_path(mol_state_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Remove the durable record — the on-disk form of signing the receipt.
///
/// Idempotent: a record that is already gone is success, because two backstops
/// racing on the same molecule must both be able to finish.
///
/// # Errors
///
/// Any filesystem error other than the record already being absent.
pub fn clear(mol_state_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(record_path(mol_state_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The argv (subcommand + flags, no program name) of the detached backstop
/// `cs tackle` arms for the molecule whose state lives in `mol_state_dir`.
///
/// Lives here, on the library surface, for the same reason
/// [`crate::realized_watcher::watcher_argv`] does: the spawner and the
/// integration test that proves the child survives its caller must invoke the
/// *same* command line, so a renamed flag is a compile failure rather than a
/// silently unarmed backstop.
#[must_use]
pub fn backstop_argv(mol_state_dir: &Path) -> Vec<OsString> {
    vec![
        OsString::from("briefing-backstop"),
        OsString::from("--state-dir"),
        mol_state_dir.as_os_str().to_os_string(),
    ]
}

/// Re-exec this binary as a detached child running `argv`, and return whether
/// the spawn succeeded.
///
/// This is the *whole* mechanism by which the briefing-submit guarantee
/// outlives the dispatcher, so each of the three things it does is load-bearing:
///
/// - **Its own process group.** `cs run` launches `cs tackle` as a child and
///   waits on it. A signal aimed at that job's group — an operator's Ctrl-C, a
///   fleet loop reaping a timed-out dispatcher — would otherwise reach the
///   backstop too. `process_group(0)` puts the child's pgid at its own pid, so
///   `kill(-dispatcher_pgid, …)` cannot name it.
/// - **Never waited on.** The child is orphaned deliberately and reparented to
///   init; nothing holds it open and nothing collects it as a zombie.
/// - **Silenced stdio.** It is detached into a session with no terminal. Its
///   findings go to the record file, which is a thing a human can read later,
///   rather than to a pipe nobody is holding.
///
/// It re-execs `current_exe` rather than resolving `cs` on `PATH` for the same
/// reason [`crate::realized_watcher`] does: the backstop and the dispatcher
/// must never skew versions.
///
/// Shared between `cs tackle` (which arms it in production) and
/// `cs briefing-backstop --detach` (which is the same act, reachable as a
/// command so an integration test can kill the caller and watch the child keep
/// working). One implementation, so the tested detach is the shipped one.
#[must_use]
pub fn detach(argv: &[OsString]) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut command = std::process::Command::new(exe);
    command
        .args(argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.spawn().is_ok()
}

// ─────────────────────────── the receipt kernel ───────────────────────────

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

    fn record() -> BriefingPending {
        BriefingPending {
            molecule: "task-20260730-73a1".to_owned(),
            worker: "worker-73a1".to_owned(),
            socket: "cosmon".to_owned(),
            needle: "final line of the brief".to_owned(),
            recorded_at: "2026-07-30T20:00:00Z".to_owned(),
            inband_seconds: 8,
            backstop_outcome: None,
            backstop_ended_at: None,
            backstop_nudges: None,
        }
    }

    /// The whole point of the record: a process that never saw the briefing
    /// reads back exactly what the process that sent it wrote. If this ever
    /// stops round-tripping, the backstop starts nudging against a needle it
    /// invented.
    #[test]
    fn a_record_round_trips_through_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            read(dir.path()),
            None,
            "an empty molecule dir has no record"
        );

        write(dir.path(), &record()).expect("write");
        assert_eq!(read(dir.path()), Some(record()));
    }

    /// Clearing the record IS signing the receipt, so it must be the one
    /// observation a later reader can trust — and it must be safe to do twice,
    /// because two backstops can race on the same molecule after a `--force`
    /// re-dispatch.
    #[test]
    fn clearing_is_idempotent_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), &record()).expect("write");

        clear(dir.path()).expect("first clear");
        assert_eq!(read(dir.path()), None);
        clear(dir.path()).expect("clearing an absent record is success");
    }

    /// A half-written or hand-mangled record reads as absent rather than
    /// exploding: the backstop's job is to press Enter, not to adjudicate JSON.
    #[test]
    fn an_unparseable_record_reads_as_no_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(record_path(dir.path()), "{ not json").expect("write junk");
        assert_eq!(read(dir.path()), None);
    }

    /// The needle is stored so it can be handed straight back to a composer
    /// scan. That only works because the derivation is idempotent — assert it
    /// here, on the transport function the record and the scan share, so a
    /// change to one cannot silently desync the other.
    #[test]
    fn the_stored_needle_is_its_own_needle() {
        let brief = "# Molecule\n\nline one\n\nfinal line of the brief\n\n";
        let needle = cosmon_transport::tmux::composer_needle(brief).expect("a needle");
        assert_eq!(needle, "final line of the brief");
        assert_eq!(
            cosmon_transport::tmux::composer_needle(needle),
            Some(needle)
        );
    }

    /// The argv shape is the contract between the spawner and the child. Lock
    /// it, or a renamed flag becomes a backstop that exits 2 into `/dev/null`.
    #[test]
    fn the_backstop_argv_names_the_state_dir_by_flag() {
        assert_eq!(
            backstop_argv(Path::new("/s/molecules/task-20260730-73a1")),
            vec![
                OsString::from("briefing-backstop"),
                OsString::from("--state-dir"),
                OsString::from("/s/molecules/task-20260730-73a1"),
            ]
        );
    }

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
