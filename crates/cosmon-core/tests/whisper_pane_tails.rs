// SPDX-License-Identifier: AGPL-3.0-only

//! Pane tails reported in issue #121: ordinary output must remain deliverable.

use cosmon_core::dialogue::{classify_pane, DialogueClass};

#[test]
fn ordinary_pane_tails_have_no_blocking_dialogue() {
    let tails = [
        (
            "busy prose cut at pane edge",
            "⏺ I'll verify the result, then confirm the inputs; I'll add the\n\
             • Working (42s • esc to interrupt)\n\
             › Ask <...> to do anything\n\
             high · ~/…worktr…  ⚠ 2 warnings · f2 to view",
        ),
        (
            "busy status waiting for headroom",
            "⏺ The CPU token is held but heavy.sh is waiting for headroom (1-min load is 53).\n\
             ✻ Working…",
        ),
        ("busy status sentence", "⏺ The holding is <…>\n✻ Working…"),
        (
            "idle with informational update banner",
            "⏺ The result is ready\n\
             ✻ Baked for 1s · done 10:35 PM\n\
             ✔ Update installed · Restart to update\n\
             ❯",
        ),
    ];

    for (name, tail) in tails {
        let scan = classify_pane(tail);
        assert_eq!(scan.class, DialogueClass::None, "{name}: {scan:?}");
    }
}

#[test]
fn idle_composer_supersedes_questions_and_menus_in_scrollback() {
    let tails = [
        (
            "question in completed answer above input",
            "• Should I refactor this?\n› Write tests for @filename\n  high",
        ),
        (
            "earlier confirmation above input",
            "Do you want to proceed?\n❯ 1. Yes\n⏺ Done\n› Summarize recent commits",
        ),
        (
            "earlier question above bare input cursor",
            "❯ why is the build failing?\n⏺ Looking into it\n✻ Working…\n❯",
        ),
        (
            "stale money choice above input",
            "Approaching usage limit?\n❯ 1. Yes\n⏺ Finished\n› Write a summary",
        ),
        (
            "answer lists options",
            "⏺ Options:\n  1. Yes do it\n  2. No",
        ),
        ("ordinary prose", "continue? we will see"),
    ];
    for (name, tail) in tails {
        let scan = classify_pane(tail);
        assert_eq!(scan.class, DialogueClass::None, "{name}: {scan:?}");
    }
}

#[test]
fn live_confirmation_and_risky_prompt_are_never_whisperable() {
    let tails = [
        "Overwrite file? [y/n] (default n)",
        "Continue? (y/N) (default N)",
        "Running: rm -rf /tmp/x\nDelete everything? Press any key",
    ];
    for tail in tails {
        let scan = classify_pane(tail);
        assert_eq!(scan.class, DialogueClass::Unknown, "{tail}: {scan:?}");
        assert!(!scan.class.auto_confirmable());
    }
}

#[test]
fn active_prompt_after_idle_input_remains_blocking() {
    let pane = "› Check the file\n⏺ Done\nOverwrite file? [y/n] (default n)";
    let scan = classify_pane(pane);
    assert_eq!(scan.class, DialogueClass::Unknown, "{scan:?}");
}

#[test]
fn menu_choice_cursor_does_not_erase_its_question() {
    for pane in [
        "Upgrade your plan?\n› Continue",
        "Upgrade your plan? (Use arrow keys)\n› Continue",
    ] {
        let scan = classify_pane(pane);
        assert_eq!(scan.class, DialogueClass::MoneyStake, "{scan:?}");
        assert!(!scan.class.auto_confirmable());
    }
    let scan = classify_pane("Are you sure? (Use arrow keys)\n› Continue");
    assert_eq!(scan.class, DialogueClass::Unknown, "{scan:?}");
}
