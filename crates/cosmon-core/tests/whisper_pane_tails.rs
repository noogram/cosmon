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
