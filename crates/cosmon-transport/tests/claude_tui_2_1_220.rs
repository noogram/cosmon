// SPDX-License-Identifier: AGPL-3.0-only

//! The readiness classifier, held against seven real Claude Code 2.1.220 panes.
//!
//! Every other test of `classify_output` builds its pane from a string literal
//! someone typed. That is how the classifier drifted: the TUI changed its
//! composer from a box to a pair of rules and stopped hiding it during a turn,
//! and not one test noticed, because not one test held a frame the TUI had
//! actually painted. The fixtures in `tests/fixtures/claude-tui-2.1.220/` were
//! captured with `tmux capture-pane -p` from a live session — one idle, six
//! taken four seconds apart while the model was streaming a long answer.
//!
//! Before the repair, `classify_output` answered `AwaitingHuman` for all seven.
//! Two callers were downstream of that:
//!
//! * `cs tackle`'s dispatch gate admits only `Ready` / `Working`, so a pane
//!   launched outside bypass-permissions mode could not be dispatched into at
//!   all; and
//! * `cs tackle`'s briefing-submit confirmation loop has `Working` as its only
//!   early exit, so every dispatch paid the full 90 s
//!   `BRIEFING_SUBMIT_INBAND_CAP` — the flat 92/93 s an external tester
//!   measured against jobs that took 32 s and 53 s.
//!
//! Keep these assertions coupled to the files. When 2.1.220 stops being the
//! build in the field, capture the new one beside it rather than relaxing
//! anything here: a classifier verified against a described TUI drifts on
//! exactly the schedule this one did.

use cosmon_transport::readiness::{classify_output, Liveness, SessionStatus};

/// The window `detect_status` asks the backend for. Production never sees more
/// than this, so neither does the assertion.
const CAPTURE_LINES: usize = 30;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-tui-2.1.220")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The trailing `CAPTURE_LINES` lines — what `TmuxBackend::capture_output`
/// hands the classifier.
fn as_captured(pane: &str) -> String {
    let lines: Vec<&str> = pane.lines().collect();
    lines[lines.len().saturating_sub(CAPTURE_LINES)..].join("\n")
}

/// Every fixture, and the verdict it must produce.
///
/// `streaming-2` through `streaming-6` are `Ready` on purpose, and the reason is
/// worth stating because it looks like a miss. The model was indeed producing
/// tokens in all five frames, but four of them carry no evidence of it in the
/// captured window: the `⏺` that opened the turn has scrolled past the top, and
/// the status line above the composer is blank in that frame (2, 3, 4) or holds
/// the *completed* `✻ Baked for 16s` summary (5, 6). Reading those bytes as an
/// idle composer is the honest answer, and it costs nothing: both callers treat
/// `Ready` and `Working` alike at the dispatch gate, and the briefing-submit
/// loop polls once a second, so it only needs to catch one frame like
/// `streaming-1` — which it does, since roughly half the frames carry the
/// running clock.
///
/// What is *not* acceptable, and what this table exists to pin, is that none of
/// them is `AwaitingHuman` and that `Working` is reachable at all.
const EXPECTED: &[(&str, SessionStatus)] = &[
    ("idle.pane", SessionStatus::Ready),
    // `✢ Coalescing… (3s · thinking with medium effort)` — a spinner glyph with
    // a running clock, directly above the composer's upper rule.
    ("streaming-1.pane", SessionStatus::Working),
    ("streaming-2.pane", SessionStatus::Ready),
    ("streaming-3.pane", SessionStatus::Ready),
    ("streaming-4.pane", SessionStatus::Ready),
    ("streaming-5.pane", SessionStatus::Ready),
    ("streaming-6.pane", SessionStatus::Ready),
];

#[test]
fn every_2_1_220_pane_classifies_as_captured() {
    for (name, want) in EXPECTED {
        let pane = fixture(name);
        assert_eq!(
            classify_output(&as_captured(&pane)),
            *want,
            "{name}: classified from the 30-line capture production actually sees"
        );
    }
}

#[test]
fn the_capture_window_does_not_change_the_verdict() {
    // A verdict that flips when the window widens is a verdict resting on
    // scrollback. Every rule that fires on these panes is tail-scoped, so the
    // full 51-line pane must agree with the 30-line capture.
    for (name, want) in EXPECTED {
        let pane = fixture(name);
        assert_eq!(
            classify_output(&pane),
            *want,
            "{name}: full pane disagrees with the captured window"
        );
    }
}

#[test]
fn no_2_1_220_pane_reads_as_awaiting_a_human() {
    // The regression itself, stated once without reference to which of the two
    // work-accepting verdicts each pane earns. Every one of these frames came
    // from a session that was running fine.
    for (name, _) in EXPECTED {
        let status = classify_output(&as_captured(&fixture(name)));
        assert_ne!(
            status,
            SessionStatus::AwaitingHuman,
            "{name}: a healthy 2.1.220 pane read as parked on a human"
        );
        assert_ne!(
            status,
            SessionStatus::Unknown,
            "{name}: a healthy 2.1.220 pane read as unrecognisable"
        );
    }
}

#[test]
fn every_2_1_220_pane_passes_the_dispatch_gate() {
    // `dispatch_gate_liveness` is private, but its rule is public and exact:
    // `Ready | Working` is what it admits. Asserting membership here is the
    // same claim, and it is the one `cs tackle`'s second spawn stage makes
    // before it decides whether to tear the session down.
    for (name, _) in EXPECTED {
        let status = classify_output(&as_captured(&fixture(name)));
        assert!(
            matches!(status, SessionStatus::Ready | SessionStatus::Working),
            "{name}: classified {status}, which the dispatch gate refuses — \
             `cs tackle` would kill this healthy session"
        );
        // And C0's separate question — did the binary run? — still answers yes.
        assert_eq!(status.liveness(), Liveness::Live, "{name}");
    }
}

#[test]
fn the_working_arm_is_reachable_on_this_tui() {
    // The 90 s tax stated as a property rather than a measurement. If no
    // 2.1.220 frame can ever classify `Working`, the briefing-submit loop has
    // no early exit and every dispatch pays its whole budget — which is the
    // regression, whatever the individual verdicts happen to be.
    let reached = EXPECTED
        .iter()
        .filter(|(name, _)| classify_output(&as_captured(&fixture(name))) == SessionStatus::Working)
        .count();
    assert!(
        reached > 0,
        "no 2.1.220 frame classifies Working — `cs tackle`'s briefing-submit \
         loop has lost its only early exit"
    );
}

#[test]
fn the_idle_pane_is_not_working() {
    // The other direction, and the reason `✻ Baked for 16s` is not accepted as
    // work evidence. A pane whose last turn finished long ago keeps that
    // summary in the status slot; if it counted, the briefing-submit loop would
    // call a briefing delivered on the strength of the previous turn.
    assert_eq!(
        classify_output(&as_captured(&fixture("idle.pane"))),
        SessionStatus::Ready
    );
    for name in ["streaming-5.pane", "streaming-6.pane"] {
        assert_ne!(
            classify_output(&as_captured(&fixture(name))),
            SessionStatus::Working,
            "{name}: a completed turn's leftover summary was read as work in flight"
        );
    }
}
