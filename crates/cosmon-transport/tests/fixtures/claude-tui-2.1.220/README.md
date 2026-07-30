# Claude Code 2.1.220 panes, captured 2026-07-30

Seven `tmux capture-pane -p` snapshots of a real Claude Code **2.1.220**
session: one idle, six taken four seconds apart while the model was actively
streaming a long answer.

They exist because `classify_output` returns **`AwaitingHuman` for all seven**
— idle and mid-stream alike. Never `Working`, never even `Ready`. Two causes
stack:

1. The composer is checked before the work markers, on the reasoning that an
   input box at the bottom means idle. That was true of a TUI whose prompt
   vanished during output. In 2.1.220 the `❯` stays painted for the whole
   stream, so the `Working` arm is unreachable.
2. `shows_composer` does not match this TUI either — otherwise these would
   classify as `Ready` rather than falling through to the chevron rule.

The consequence measured downstream: `cs tackle`'s briefing-submit loop can
only exit early on `Working`, so every dispatch pays the full 90 s
`BRIEFING_SUBMIT_INBAND_CAP` — the flat 92/93 s an external tester reported
against jobs of 32 s and 53 s.

Keep them. A classifier repaired against a described TUI rather than a
captured one is how this drifted in the first place: nothing in the suite
held a real frame, so nothing failed when the frame changed.

## Neutralised — what was changed, and what was not

The panes carried operator identity the public repository has no business
holding: an account address and organisation name, a subscription tier, a
count of MCP servers awaiting authentication, and a session path. Four
substitutions were applied:

| was | is |
|---|---|
| `<account>@<provider>'s Organization` | `operator@example.invalid's Org` |
| the capture directory | `/…/fixtures/probe` |
| the subscription tier | `Claude ***` |
| the MCP count | `N` |

A redaction marker rather than a plausible substitute where a value would
otherwise be *invented*: `Claude ***` says a tier was removed, whereas naming
a different real tier would replace one fact with another and read as
evidence.

**No classified marker was touched.** Every substitution absorbs its own
length change in the whitespace that immediately follows it, so every column
to its right — the inner `│` separators included — keeps its position, and
each line's character count is unchanged. The load-bearing check is the
verdict itself: `classify_output` returns `AwaitingHuman` for all seven both
before and after, which is exactly the property these fixtures exist to pin.

`scripts/publish.sh --check` passed on the un-neutralised captures. That is a
gap in the gate, not a clearance: a personal address inside a captured TUI
frame is precisely the shape it claims to name.
