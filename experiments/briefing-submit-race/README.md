<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Briefing-submit race — measurement harness

What this answers, in one sentence: when cosmon pastes a briefing into a Claude
Code pane and then sends the submit carriage return, and the briefing stays in
the composer anyway — did the `0d` byte fail to arrive, or did it arrive and get
dropped by the TUI?

Nothing in the existing instrumentation can tell those two apart.
`send_input.settled` counts polls and names an exit reason; `capture-pane` shows
what the application *drew*. Both are downstream of the question. So the pane
runs the target under `ptyspy.py`, which allocates a second PTY for the child
and appends every byte it forwards, with a timestamp, to a log. The paste
arrives bracketed (`ESC[200~ … ESC[201~`, because `paste-buffer -p`), so
everything after the terminator in that log is the separately-sent keystroke —
the CR, or its absence, is then a fact on disk rather than an inference.

## Running it

```sh
python3 matrix.py --workdir <scratch> --ws <trusted-cwd> --out results.jsonl \
    [--busy] [--sizes 1,12,100,300] [--delays 0,100,250,500,1000] [--reps 5]
python3 aggregate.py results.jsonl
```

`--ws` must be a directory Claude Code already trusts, or every trial stalls on
the folder-trust dialog (see `cosmon-transport/src/claude_trust.rs`). Trials run
on a private tmux socket (`--socket`, `cosmon-test-` prefix by convention) that
is killed on every exit path including a signal; the fleet socket is never
touched.

Readiness is "the composer is drawn and empty", and *empty* is a rendering the
TUI is free to change: since Claude Code 2.1.220 an idle composer carries a
rotating hint (`❯ Try "fix lint errors"...`), so a gate demanding a bare glyph
times out on every trial and reports `composer never rendered` on a machine
where nothing is wrong. The gate therefore accepts a glyph alone or a glyph
followed by that placeholder, and still rejects a glyph followed by anything
else — which is the pending briefing it exists to catch.

## What one trial waits for, and for how long

`--settle-s` defaults to **30 s**, not the 4 s this harness started with. A real
`--busy` trial was measured emptying its composer 23813 ms after the carriage
return: with a four-second window that trial is recorded as
`pending_after_settle=True`, which reads as a swallowed submit when what
actually happened is a pane that had not finished its previous answer yet. A
window shorter than the slowest accept does not measure the race, it
manufactures it.

The receipt is watched on its own, shorter deadline (`--ack-deadline-s`, 12 s,
production's). The two signals are on different clocks — a busy pane empties its
composer in under a second while `UserPromptSubmit` does not fire until the
queue drains — so each is polled until its own deadline instead of the loop
stopping at whichever arrives first.

## The typed receipt column

When `experiments/briefing-receipt-hook` is present, each trial mints a nonce,
installs the `UserPromptSubmit` receipt hook through an ephemeral
`claude --settings` overlay (commit 8749887's mechanism, imported rather than
reimplemented), and records the nonce plus one of three values:

- `ack` — a receipt keyed to *this trial's* nonce exists: the application
  acknowledged the prompt;
- `absent` — the hook was installed and wrote no such receipt;
- `unavailable` — no hook was installed (`--no-receipt`, or the sibling
  experiment is not in the checkout).

`absent` and `unavailable` are not the same fact and are never folded together:
the first is evidence about the submit, the second is evidence about nothing.
The nonce is stamped *after* the `--busy` warm-up prompt, since that prompt goes
through the same hook and would otherwise claim the receipt.

## The permission-mode x load axis

`--permission-modes` and `--loads` cross the grid with `claude --permission-mode`
values and with N spinning CPU hogs per trial. Both default to a single cell —
flag unset, machine as found — so the default run is exactly the grid above;
they exist because the accepted-submit rate plausibly depends on both and
neither had ever been varied deliberately.

```sh
python3 matrix.py ... --permission-modes ,plan,acceptEdits --loads 0,8
```

Each trial presses submit **exactly once**. That is the point: the production
retry loop is what makes the phenomenon invisible, so a harness that retried
would measure the loop instead of the race.

## The two controls that make the grid mean something

- `--no-cr` — paste and never submit. Every cell must report `pending=True`; if
  it does not, the composer detector cannot see an unsubmitted briefing and a
  grid full of `pending=False` is vacuous rather than green.
- `--busy` — put the TUI into a long response *before* pasting. The reported
  symptom is a nudge sent to a worker that is already thinking, so an
  idle-composer grid alone cannot speak for it.
