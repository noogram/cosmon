// SPDX-License-Identifier: AGPL-3.0-only

//! The send seam must name the writer of every injection (COSMON #26 residual).
//!
//! # What these tests are for
//!
//! An operator found `cs done` sitting unsubmitted in a worker's composer and
//! there was no way to ask who put it there. The fix is an event emitted at the
//! single seam every tmux injection passes through,
//! `TmuxBackend::send_input_observed`. These tests redden if that emission
//! stops — which is the only failure mode that matters, because a missing
//! provenance event is invisible until the next time someone needs it and it
//! is not there.
//!
//! # Why they need no tmux
//!
//! Every case below targets a worker that does not exist on a socket nobody
//! serves. That is deliberate, and not a compromise: the seam records the
//! injection *before* it resolves and writes, so a send that fails is still
//! attributed. An event emitted only on success would leave exactly the
//! misfires — a keystroke aimed at a session that is gone, the case an
//! unexplained composer most needs explained — off the record. Asserting the
//! failing path therefore asserts the ordering, and it runs identically on a
//! host with no tmux installed.

use std::path::Path;

use cosmon_core::event_v2::{Envelope, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::injection::{
    injection_digest, InjectionLedger, InjectionOrigin, InjectionProvenance,
};
use cosmon_core::transport::TransportBackend;
use cosmon_transport::TmuxBackend;

fn mol() -> MoleculeId {
    MoleculeId::new("task-20260731-f0ab").unwrap()
}

fn wkr() -> WorkerId {
    WorkerId::new("ghost-1234").unwrap()
}

/// A backend pointed at a socket no tmux server is listening on.
fn dead_backend() -> TmuxBackend {
    TmuxBackend::new("cosmon-test-no-such-socket-f0ab")
}

fn ledgered(origin: InjectionOrigin, purpose: &str, dir: &Path) -> InjectionProvenance {
    InjectionProvenance::new(origin, purpose).with_ledger(InjectionLedger::new(mol(), dir))
}

fn injections(dir: &Path) -> Vec<EventV2> {
    let raw = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| Envelope::from_line(l).ok())
        .map(|e| e.event)
        .filter(|e| matches!(e, EventV2::InputInjected { .. }))
        .collect()
}

#[test]
fn an_injection_with_text_names_its_writer() {
    let dir = tempfile::tempdir().unwrap();
    let briefing = "# Molecule: task-20260731-f0ab\nDo the work.";

    let _ = dead_backend().send_input_observed(
        &wkr(),
        briefing,
        &ledgered(InjectionOrigin::TackleBriefing, "briefing", dir.path()),
    );

    let events = injections(dir.path());
    assert_eq!(events.len(), 1, "exactly one event per injection");
    let EventV2::InputInjected {
        mol_id,
        worker_id,
        origin,
        purpose,
        input_len,
        input_digest,
        bare_submit,
        ..
    } = &events[0]
    else {
        unreachable!("filtered above")
    };
    assert_eq!(mol_id.as_ref(), Some(&mol()));
    assert_eq!(worker_id, &wkr());
    assert_eq!(*origin, InjectionOrigin::TackleBriefing);
    assert_eq!(purpose, "briefing");
    assert_eq!(*input_len, briefing.len());
    assert_eq!(input_digest, &injection_digest(briefing));
    assert!(!*bare_submit);
}

#[test]
fn a_bare_submit_is_recorded_too() {
    // The whole reason the empty input is in scope: a naked Enter leaves no
    // text in the pane, so if it is not in the ledger it never happened as far
    // as any later investigation can tell.
    let dir = tempfile::tempdir().unwrap();

    let _ = dead_backend().send_input_observed(
        &wkr(),
        "",
        &ledgered(
            InjectionOrigin::BriefingBackstop,
            "backstop-submit",
            dir.path(),
        ),
    );

    let events = injections(dir.path());
    assert_eq!(events.len(), 1, "a bare submit is a row, not a hole");
    let EventV2::InputInjected {
        origin,
        input_len,
        bare_submit,
        input_digest,
        ..
    } = &events[0]
    else {
        unreachable!("filtered above")
    };
    assert_eq!(*origin, InjectionOrigin::BriefingBackstop);
    assert_eq!(*input_len, 0);
    assert!(*bare_submit, "the naked Enter must be flagged as such");
    assert_eq!(input_digest, &injection_digest(""));
}

#[test]
fn the_injected_text_never_reaches_the_ledger() {
    // `events.jsonl` is tracked in git; a briefing can quote private material.
    // The event must fingerprint the input, never republish it.
    let dir = tempfile::tempdir().unwrap();
    let confidential = "the client's unannounced acquisition target";

    let _ = dead_backend().send_input_observed(
        &wkr(),
        confidential,
        &ledgered(InjectionOrigin::Whisper, "whisper", dir.path()),
    );

    let raw = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
    assert!(raw.contains("input_injected"), "the event was written");
    assert!(
        !raw.contains("acquisition target"),
        "the ledger must carry a digest, never the text: {raw}"
    );
    assert!(raw.contains(&injection_digest(confidential)));
}

#[test]
fn each_caller_is_distinguishable_in_the_log() {
    // The forensic question is *which* of the senders wrote this. A log that
    // records every injection under one label answers nothing, so assert the
    // origins survive the round-trip distinctly.
    let dir = tempfile::tempdir().unwrap();
    let backend = dead_backend();
    let senders = [
        (InjectionOrigin::TackleBriefing, "briefing"),
        (InjectionOrigin::PatrolNudge, "nudge"),
        (InjectionOrigin::PatrolHeal, "rebrief"),
        (InjectionOrigin::Propulsion, "propel"),
        (InjectionOrigin::Thaw, "thaw-prompt"),
        (InjectionOrigin::Resume, "resume-prompt"),
        (InjectionOrigin::BriefingBackstop, "backstop-submit"),
        (InjectionOrigin::Whisper, "whisper"),
        (
            InjectionOrigin::DialogueAutoConfirm,
            "dialogue-auto-confirm",
        ),
        (InjectionOrigin::ReadinessProbe, "answer-trust-prompt"),
        (InjectionOrigin::GracefulExit, "adapter-quit"),
    ];

    for (origin, purpose) in senders {
        let _ = backend.send_input_observed(
            &wkr(),
            "keep going",
            &ledgered(origin, purpose, dir.path()),
        );
    }

    let events = injections(dir.path());
    assert_eq!(events.len(), senders.len(), "one event per injection");
    let observed: Vec<InjectionOrigin> = events
        .iter()
        .map(|e| match e {
            EventV2::InputInjected { origin, .. } => *origin,
            _ => unreachable!("filtered above"),
        })
        .collect();
    let expected: Vec<InjectionOrigin> = senders.iter().map(|(o, _)| *o).collect();
    assert_eq!(observed, expected);
}

#[test]
fn an_injection_that_cannot_be_delivered_is_still_attributed() {
    // The ordering guarantee, stated as its own case. The socket is dead, so
    // the send below fails — and the whole point is that the attempt is on the
    // record anyway. An event emitted after a successful write would lose
    // precisely the misfires.
    let dir = tempfile::tempdir().unwrap();

    let outcome = dead_backend().send_input_observed(
        &wkr(),
        "cs done",
        &ledgered(InjectionOrigin::Propulsion, "propel", dir.path()),
    );

    assert!(outcome.is_err(), "no session to deliver to");
    let events = injections(dir.path());
    assert_eq!(events.len(), 1, "the failed attempt is still recorded");
    let EventV2::InputInjected {
        session,
        input_digest,
        ..
    } = &events[0]
    else {
        unreachable!("filtered above")
    };
    assert!(
        session.is_empty(),
        "an unresolvable session is recorded as empty, not guessed: {session}",
    );
    // The forensic move issue #26 wanted: digest the string found in a
    // composer, then look for it in the ledger.
    assert_eq!(input_digest, &injection_digest("cs done"));
}

#[test]
fn an_undeclared_caller_is_logged_as_unattributed() {
    // `send_input` is the unattributed door, and it must still go through the
    // seam. It carries no ledger — a caller that did not name itself has no
    // molecule to file under either — so the assertion here is about the stamp
    // the port method chooses, which is what a future reader of an
    // `origin=unattributed` trace line will act on.
    let provenance = InjectionProvenance::unattributed();
    assert_eq!(provenance.origin, InjectionOrigin::Unattributed);
    assert!(provenance.ledger.is_none());

    // And it reaches the wire the same way an attributed one does: same error,
    // no separate code path.
    let dir = tempfile::tempdir().unwrap();
    let backend = dead_backend();
    assert!(backend.send_input(&wkr(), "hello").is_err());
    assert!(
        injections(dir.path()).is_empty(),
        "no ledger means no event — the trace line is the only record",
    );
}
