// SPDX-License-Identifier: AGPL-3.0-only

//! `cs briefing-backstop` — the durable half of the briefing-submit guarantee
//! (COSMON #26-B).
//!
//! Hidden dispatch plumbing, in the same family as `cs realized-watch`: never
//! typed by an operator, spawned detached by `cs tackle` at the one moment it
//! is useful, and speaking only through the record it was handed and the
//! molecule's log.
//!
//! # What it is for
//!
//! `cs tackle` presses submit for a few seconds and then must let the
//! dispatcher go (see [`cosmon_cli::briefing_backstop`] for why patience cannot
//! be charged to a serial fleet loop). The field incident that started this —
//! four of eleven workers idle on `❯ [Pasted text #1 +86 lines]`, zero tokens,
//! for twenty minutes — was fixed by a single manual Enter *once the TUI had
//! settled*, which was long after any in-band window could still be open.
//!
//! So this process is what is still pressing then. It reads the
//! [`BriefingPending`] record `cs tackle` left on disk, re-opens the recorded
//! tmux socket, and runs the **same** receipt kernel on a twenty-minute budget.
//! It owns no dispatch and blocks nothing: the worst it can cost is its own
//! `capture-pane` every few seconds.
//!
//! # What it promises, and what it does not
//!
//! It promises that the pressing continues after `cs tackle` exits, and after
//! whatever launched `cs tackle` dies — the child is parked in its own process
//! group so a signal aimed at the dispatcher's group does not reach it, and it
//! is never waited on, so it is reparented to init rather than held open.
//!
//! It does not promise a delivery. A composer that never clears inside the
//! durable budget ends as a record annotated with the outcome and the nudge
//! count, left on disk for a human or a patrol pass to find. Removing the
//! record means one thing only: the briefing left the composer.

use std::path::PathBuf;
use std::time::Duration;

use cosmon_cli::briefing_backstop::{
    self, run_briefing_submit_loop, BriefingPending, BriefingSubmitOutcome,
};
use cosmon_core::id::WorkerId;
use cosmon_core::transport::TransportBackend;
use cosmon_transport::tmux::ComposerState;
use cosmon_transport::TmuxBackend;

use super::Context;

/// Arguments for the hidden `briefing-backstop` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// The molecule's state directory — the one holding
    /// [`briefing_backstop::RECORD_FILE`]. Passed explicitly because a
    /// detached child inherits no galaxy context and must not re-resolve the
    /// fleet from its own cwd (which is not the dispatched worktree).
    #[arg(long)]
    pub state_dir: PathBuf,

    /// How long to keep pressing, in seconds.
    ///
    /// Defaults to [`DURABLE_BUDGET_SECS`] — the length of the observed stall,
    /// not a round number.
    #[arg(long, default_value_t = DURABLE_BUDGET_SECS)]
    pub budget_secs: u64,

    /// Milliseconds between composer captures.
    ///
    /// Slower than the in-band poll on purpose: nobody is waiting on this
    /// process, and a `capture-pane` per second for twenty minutes is a cost
    /// the machine pays for no gain in the thing being watched (a TUI that is
    /// too busy to accept Enter does not become un-busy in under a second).
    #[arg(long, default_value_t = 3_000)]
    pub interval_ms: u64,

    /// Re-exec self as a detached child with the same flags minus this one,
    /// then return immediately.
    ///
    /// The act `cs tackle` performs in-process, exposed as a command so the
    /// survival property can be *tested*: a caller can arm the backstop this
    /// way, be killed along with its whole process group, and the child is
    /// still there pressing submit. Both paths call
    /// [`briefing_backstop::detach`], so the detachment under test is the
    /// detachment that ships.
    #[arg(long)]
    pub detach: bool,
}

/// The durable budget, in seconds.
///
/// Twenty minutes, because that is how long the 2026-07-24 workers
/// (`task-20260724-c014`) sat on an unsubmitted paste before a human pressed
/// Enter and they started instantly. A budget shorter than the observed stall
/// would reproduce the incident with extra steps; a much longer one would keep
/// a process alive against a worker whose problem is no longer a swallowed
/// keystroke.
pub const DURABLE_BUDGET_SECS: u64 = 20 * 60;

/// Run the durable backstop for the molecule whose state lives in
/// `args.state_dir`.
///
/// Exits `Ok` on every path, including "there was nothing to do". A backstop
/// that fails loudly would be spawned detached into a null stdio and shout at
/// nobody; the outcome that matters is recorded in the record file, which is
/// the artifact a human actually reads.
///
/// # Errors
///
/// Only a filesystem failure while writing back the annotated record — a
/// condition under which the state dir itself is broken and silence would be
/// worse than a non-zero exit.
pub fn run(_ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if args.detach {
        let armed = briefing_backstop::detach(&detached_argv(args));
        tracing::debug!(
            state_dir = %args.state_dir.display(),
            armed,
            "briefing backstop: detached child armed"
        );
        return Ok(());
    }

    let Some(record) = briefing_backstop::read(&args.state_dir) else {
        // Either `cs tackle` never gave up (the briefing landed in band, the
        // common case) or someone else already signed the receipt. Both mean
        // there is nothing here to press.
        tracing::debug!(
            state_dir = %args.state_dir.display(),
            "briefing backstop: no pending record; nothing to do"
        );
        return Ok(());
    };

    let Ok(worker) = WorkerId::new(&record.worker) else {
        tracing::warn!(
            worker = %record.worker,
            "briefing backstop: record names a worker that is not a valid id; \
             leaving the record for a human"
        );
        return Ok(());
    };

    let backend = TmuxBackend::new(record.socket.clone());
    let budget = Duration::from_secs(args.budget_secs);
    let poll = Duration::from_millis(args.interval_ms);
    let started = std::time::Instant::now();
    let mut nudges: u64 = 0;

    tracing::info!(
        molecule = %record.molecule,
        worker = %record.worker,
        socket = %record.socket,
        budget_seconds = args.budget_secs,
        "briefing backstop: resuming submit pressure after the dispatcher exited"
    );

    let outcome = run_briefing_submit_loop(
        budget,
        &mut || probe(&backend, &worker, &record.needle),
        &mut || {
            nudges += 1;
            // Empty input == a bare submit keystroke (see `send_input`), which
            // is exactly the manual recovery that unstalled these workers.
            let _ = backend.send_input(&worker, "");
        },
        &mut || started.elapsed(),
        &mut || std::thread::sleep(poll),
    );

    finish(&args.state_dir, &record, outcome, nudges)
}

/// The argv of the child a `--detach` invocation arms: the same run, minus the
/// flag that would make it detach again.
///
/// The budget and interval are carried across explicitly rather than left to
/// the child's defaults — a caller that asked for a two-second budget must get
/// a two-second budget, not a twenty-minute one, or the flag would silently
/// mean nothing on the only path that uses it.
fn detached_argv(args: &Args) -> Vec<std::ffi::OsString> {
    let mut child = briefing_backstop::backstop_argv(&args.state_dir);
    child.push(std::ffi::OsString::from("--budget-secs"));
    child.push(std::ffi::OsString::from(args.budget_secs.to_string()));
    child.push(std::ffi::OsString::from("--interval-ms"));
    child.push(std::ffi::OsString::from(args.interval_ms.to_string()));
    child
}

/// One composer reading for the durable loop.
///
/// `None` means the session is gone — the loop's cue to stop rather than spend
/// twenty minutes pressing Enter at a worker that no longer exists.
/// [`cosmon_transport::TmuxBackend::composer_state_for`] answers `NotFound`
/// only when no live session carries the worker's id, so the two error classes
/// stay apart: a vanished worker is a fact, while a tmux server that could not
/// be queried is an absence of one and reads as
/// [`ComposerState::Unobservable`], which signs nothing.
fn probe(backend: &TmuxBackend, worker: &WorkerId, needle: &str) -> Option<ComposerState> {
    match backend.composer_state_for(worker, needle) {
        Ok(state) => Some(state),
        Err(cosmon_core::transport::TransportError::NotFound(_)) => None,
        Err(_) => Some(ComposerState::Unobservable),
    }
}

/// Record what the durable pass concluded.
///
/// A receipt removes the record; anything else annotates it in place. That
/// asymmetry is the whole reporting contract: the *presence* of the file always
/// means "the last process to look saw an unsubmitted briefing", and its
/// annotation says how hard the last process tried.
fn finish(
    state_dir: &std::path::Path,
    record: &BriefingPending,
    outcome: BriefingSubmitOutcome,
    nudges: u64,
) -> anyhow::Result<()> {
    if outcome == BriefingSubmitOutcome::Delivered {
        tracing::info!(
            molecule = %record.molecule,
            worker = %record.worker,
            nudges,
            "briefing backstop: the briefing left the composer; receipt signed"
        );
        briefing_backstop::clear(state_dir)?;
        return Ok(());
    }

    let word = match outcome {
        BriefingSubmitOutcome::Delivered => unreachable!("handled above"),
        BriefingSubmitOutcome::Unobservable => "unobservable",
        BriefingSubmitOutcome::StuckPasted => "stuck-pasted",
        BriefingSubmitOutcome::SessionGone => "session-gone",
    };
    tracing::warn!(
        molecule = %record.molecule,
        worker = %record.worker,
        socket = %record.socket,
        outcome = word,
        nudges,
        "briefing backstop: durable budget spent without a delivery receipt; \
         the record is left in the molecule state dir for a human. Recover with \
         a bare submit (`tmux -L <socket> send-keys -H 0d -t <worker>`) or \
         re-dispatch with `cs tackle --force`"
    );
    let annotated = BriefingPending {
        backstop_outcome: Some(word.to_owned()),
        backstop_ended_at: Some(chrono::Utc::now().to_rfc3339()),
        backstop_nudges: Some(nudges),
        ..record.clone()
    };
    briefing_backstop::write(state_dir, &annotated)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(dir: &std::path::Path) -> BriefingPending {
        let record = BriefingPending {
            molecule: "task-20260730-73a1".to_owned(),
            worker: "worker-73a1".to_owned(),
            socket: "cosmon-test".to_owned(),
            needle: "final line of the brief".to_owned(),
            recorded_at: "2026-07-30T20:00:00Z".to_owned(),
            inband_seconds: 8,
            backstop_outcome: None,
            backstop_ended_at: None,
            backstop_nudges: None,
        };
        briefing_backstop::write(dir, &record).expect("write");
        record
    }

    /// The receipt, on disk: a delivered briefing leaves *no* record. Anything
    /// else would leave a permanent false alarm behind every successful late
    /// submit.
    #[test]
    fn a_delivery_removes_the_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = pending(dir.path());

        finish(dir.path(), &record, BriefingSubmitOutcome::Delivered, 3).expect("finish");

        assert_eq!(briefing_backstop::read(dir.path()), None);
    }

    /// Every non-delivery keeps the record and says which one it was. The
    /// file's presence is the alarm; the annotation is the diagnosis.
    #[test]
    fn every_non_delivery_keeps_the_record_and_names_itself() {
        for (outcome, word) in [
            (BriefingSubmitOutcome::StuckPasted, "stuck-pasted"),
            (BriefingSubmitOutcome::Unobservable, "unobservable"),
            (BriefingSubmitOutcome::SessionGone, "session-gone"),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let record = pending(dir.path());

            finish(dir.path(), &record, outcome, 7).expect("finish");

            let back = briefing_backstop::read(dir.path()).expect("record survives");
            assert_eq!(back.backstop_outcome.as_deref(), Some(word));
            assert_eq!(back.backstop_nudges, Some(7));
            assert!(back.backstop_ended_at.is_some());
            // The dispatch-time facts must survive the annotation — they are
            // what a human needs in order to press Enter by hand.
            assert_eq!(back.needle, record.needle);
            assert_eq!(back.socket, record.socket);
            assert_eq!(back.worker, record.worker);
        }
    }

    /// The durable budget is the length of the observed stall. Shorter and the
    /// incident reproduces with extra steps.
    #[test]
    fn the_durable_budget_outlasts_the_observed_stall() {
        assert!(
            DURABLE_BUDGET_SECS >= 20 * 60,
            "the 2026-07-24 workers sat unsubmitted for twenty minutes; a \
             durable budget of {DURABLE_BUDGET_SECS}s gives up before the \
             incident it exists to survive"
        );
    }
}
