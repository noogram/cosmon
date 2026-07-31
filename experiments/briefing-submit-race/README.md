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
