<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Event-driven briefing receipt — measured, and what it costs

Claude Code 2.1.220, tmux 3.5a, macOS 25.5.0 (arm64), 2026-08-01. Harness:
`experiments/briefing-receipt-hook/`. Raw trials beside this file in the
molecule directory (`trials-*.jsonl`).

*Tables and verdict are filled in from the matrix; see the sections below.*

## The question

`TmuxBackend::send_input` learns that a briefing was submitted by pausing a
fixed 500 ms, pressing the carriage return, and then reading the composer every
300 ms until the pasted text is no longer there. Both numbers exist because
cosmon has no way to be *told*. `experiments/briefing-submit-race` measured
that path, found the 500 ms pause load-bearing at 0 ms and unimplicated at its
own value, and opened this follow-up: Claude Code fires a `UserPromptSubmit`
hook, and `claude --settings <file>` can install one per session, so the
application could sign a receipt instead.

## The mechanism

An ephemeral settings file in the molecule's own scratch directory, handed to
the process with `claude --settings`, registers one `UserPromptSubmit` hook.
Per dispatch, cosmon stamps a nonce (write-then-rename), pastes, presses the
carriage return **immediately**, and waits for a receipt named for that nonce.
The hook writes `{nonce, session_id, event, timestamps}` and nothing else.

`--settings` is additive and file-scoped. The user, project, local and managed
settings are never read or written by anything in this experiment; that is
checked by hash, and the check is reported below.

## How it was measured

Two arms per trial, each a fresh `claude` in a fresh tmux session on a private
`cosmon-test-` socket, running under `ptyspy.py` so the carriage returns are
counted from the bytes the *application* received rather than from what the
driver believes it sent:

- **prod** — `send_input` reproduced: `C-u`, paste, sleep 500 ms, CR, then
  `capture-pane` every 300 ms, re-pressing CR while the composer still shows
  the paste, up to the auto-scaled retry budget.
- **event** — the candidate: stamp nonce, `C-u`, paste, CR at once, then poll
  for the receipt every 50 ms, re-pressing CR only while none exists.

The two arms answer the same question — *did the briefing get submitted?* — so
their latencies are comparable; what differs is what each one is allowed to
conclude, which is the point of the typed outcome in `receipt.py`.

## What a receipt proves

Three statements, in descending strength. Only the first two survive:

1. **A prompt entered this session's `UserPromptSubmit` lifecycle after cosmon
   stamped nonce N.** Supported directly: the hook runs on that event and reads
   the nonce at the moment it runs.
2. **Therefore a carriage return cosmon sent was accepted by the composer.**
   Supported under fleet conditions, with the correlation caveat below.
3. **Therefore the model is processing the briefing.** *Not* supported, and the
   `blocking_hook` scenario is the counterexample: a second hook on the same
   event exits 2 and the prompt is rejected — after ours has already written
   its receipt.

So the receipt retires "did the submit land?" and leaves "is the worker
working?" exactly where it was: with the `Working` / `⏺` acceptance signal.

## Confidentiality and blast radius

| property | how it is held |
|---|---|
| no prompt content persisted | only `session_id` and `hook_event_name` are read out of the payload; `prompt`, `cwd` and `transcript_path` are never copied |
| no stdout | fd 1 is redirected to `/dev/null` before any other statement runs, so a warning from a future import cannot leak into model context |
| cannot block a prompt | every path returns 0, including unwritable directory, missing nonce, malformed stdin |
| receipt is never half-read | temp file in the destination directory, `fsync`, `os.rename` |
| a hostile nonce cannot escape | the nonce is filtered to `[A-Za-z0-9-_]` and truncated before it becomes a filename |
| operator settings untouched | the overlay is a new 0600 file in the molecule's scratch directory; nothing reads or writes user, project, local or managed settings |

`test_ack_hook.py` checks each of these in about a second, without a TUI in the
loop.
