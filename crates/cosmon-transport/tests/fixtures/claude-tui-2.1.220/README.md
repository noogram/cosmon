# Claude Code 2.1.220 panes, captured 2026-07-30

Seven `tmux capture-pane -p` snapshots of a real Claude Code **2.1.220**
session: one idle, six taken four seconds apart while the model was actively
streaming a long answer.

They exist because `classify_output` returned **`AwaitingHuman` for all seven**
— idle and mid-stream alike. Never `Working`, never even `Ready`. Two causes
stacked:

1. The composer was checked before the work markers, on the reasoning that an
   input box at the bottom means idle. That was true of a TUI whose prompt
   vanished during output. In 2.1.220 the `❯` stays painted for the whole
   stream, so the `Working` arm was unreachable.
2. `shows_composer` did not match this TUI either — otherwise these would have
   classified as `Ready` rather than falling through to the chevron rule.

Two consequences downstream, both in `cs tackle`. Its dispatch gate admits only
`Ready` / `Working`, so a session launched outside bypass-permissions mode —
whose footer is `⏸ manual mode on`, matching no marker — could not be
dispatched into at all; the bypass footer's `⏵⏵` was the only thing rescuing
the ordinary path. And its briefing-submit loop could only exit early on
`Working`, so every dispatch paid the full 90 s `BRIEFING_SUBMIT_INBAND_CAP` —
the flat 92/93 s an external tester reported against jobs of 32 s and 53 s.

That second exit is gone (COSMON #26-A, shipped before this repair and
deliberately not waiting for it). The loop now leaves on a *delivery receipt* —
two consecutive captures showing our own briefing text gone from the composer —
and `Working` is not even a parameter of that decision any more. Measured on
two real dispatches after it landed: 24 s and 25 s, against 105 s and 107 s
before. These panes pin the half of that fix that lives here:
`composer_indicates_pending` must read them `Clear`.

## After the repair (task-20260730-ec81)

`idle` and `streaming-2`…`streaming-6` classify `Ready`; `streaming-1`
classifies `Working`. The assertions live in
`tests/claude_tui_2_1_220.rs`, which also explains why five genuinely-streaming
frames are honestly read as `Ready`: four carry no evidence of the turn inside
the 30-line window production captures, and reading absent evidence as present
is the habit that produced the drift.

Two rules changed in `readiness.rs`:

- `shows_composer` learned this TUI's composer — an input line ruled above and
  below, where the older one was boxed. A menu ruled the same way is still
  refused, because its chevron rests on an option.
- work-in-flight is now checked *before* the composer, and its evidence is a
  running clock in the status slot (`✢ Coalescing… (3s · …)`), not a `⏺` in
  scrollback. A finished turn leaves `✻ Baked for 16s` there instead, which
  deliberately does not qualify — otherwise a pane idle since yesterday would
  read `Working`.

Keep the files. A classifier repaired against a described TUI rather than a
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
