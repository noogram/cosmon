// SPDX-License-Identifier: AGPL-3.0-only

//! Permanent seam-level regression for COSMON-DEV #85.
//!
//! A captured pane tail can contain stale permission text above the current
//! Codex menu. The named menu must dominate that stale text so patrol cannot
//! auto-confirm it and whisper cannot mistake pasted prose for worker input.

use cosmon_core::dialogue::{classify_codex_dialog, classify_pane, CodexDialogKind, DialogueClass};

fn assert_fail_closed(pane: &str, expected_kind: CodexDialogKind, expected_class: DialogueClass) {
    let kind = classify_codex_dialog(pane);
    let scan = classify_pane(pane);
    assert_eq!(
        kind,
        Some(expected_kind),
        "CONTRACT P1 breach: named Codex menu was not recognised"
    );
    assert_eq!(
        scan.class,
        expected_class,
        "CONTRACT P2/P4 breach: named Codex menu resolved to {:?}; stale permission text must not make the current menu auto-confirmable or whisper-safe",
        scan.class
    );
    assert!(
        !scan.class.auto_confirmable(),
        "CONTRACT P2/P4 breach: named Codex menu is auto-confirmable"
    );
}

#[test]
fn update_menu_dominates_stale_permission_text() {
    let pane = "Do you want to proceed?\n  1. Yes\n\nUpdate available! 0.154.0 -> 0.157.0";
    assert_fail_closed(
        pane,
        CodexDialogKind::UpdateAvailable,
        DialogueClass::Unknown,
    );
}

#[test]
fn reasoning_menu_dominates_stale_permission_text() {
    let pane = "Do you want to proceed?\n  1. Yes\n\nSelect Reasoning Level for gpt-6-astra";
    assert_fail_closed(
        pane,
        CodexDialogKind::ReasoningPicker,
        DialogueClass::Unknown,
    );
}

#[test]
fn rate_limit_menu_dominates_stale_permission_text() {
    let pane = "Do you want to proceed?\n  1. Yes\n\nApproaching rate limits\nSwitch to gpt-5.6-luna for lower credit usage?\n> 1. Switch to gpt-5.6-luna\n  2. Keep current model\nPress enter to confirm or esc to go back";
    assert_fail_closed(
        pane,
        CodexDialogKind::RateLimitSwitch,
        DialogueClass::MoneyStake,
    );
}
