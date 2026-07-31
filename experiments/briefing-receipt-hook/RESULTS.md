<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Event-driven briefing receipt — measured, and what it costs

Claude Code 2.1.220, tmux 3.5a, macOS 25.5.0 (arm64), 2026-08-01. Harness:
`experiments/briefing-receipt-hook/`. Raw trials beside this file in the
molecule directory (`trials-*.jsonl`).

**Nothing in production changed.** The 500 ms pause, the 300 ms poll, the retry
budget and the composer check in `cosmon-transport/src/tmux.rs` are untouched;
the hook is installed nowhere outside a scratch directory.

## The question

`TmuxBackend::send_input` learns that a briefing was submitted by pausing a
fixed 500 ms, pressing the carriage return, then reading the composer every
300 ms until the pasted text is no longer there. Both numbers exist because
cosmon has no way to be *told*. `experiments/briefing-submit-race` measured
that path and opened this follow-up: Claude Code fires a `UserPromptSubmit`
hook, and `claude --settings <file>` can install one per session, so the
application could sign a receipt instead of cosmon inferring one from pixels.

## The mechanism

An ephemeral settings file in the molecule's own scratch directory, handed to
the process with `claude --settings`, registers one `UserPromptSubmit` hook.
Per dispatch, cosmon stamps a nonce (write-then-rename), pastes, presses the
carriage return **immediately**, and waits for a receipt named for that nonce.
The hook writes `{nonce, session_id, event, timestamps}` and nothing else.

`--settings` is additive and file-scoped. Hashes of `~/.claude/settings.json`,
`~/.claude/settings.local.json` and the project's `.claude/settings.local.json`
were taken before the matrix and re-checked after; no managed-settings file
exists on this host. The check is reported at the end.

## How it was measured

Two arms, each a fresh `claude` in a fresh tmux session on a private
`cosmon-test-` socket, running under `ptyspy.py` so the carriage returns are
counted from the bytes the *application* received rather than from what the
driver believes it sent:

- **prod** — `send_input` reproduced: `C-u`, paste, sleep 500 ms, CR, then
  `capture-pane` every 300 ms, re-pressing CR while the composer still shows
  the paste, up to the auto-scaled retry budget.
- **event** — the candidate: stamp nonce, `C-u`, paste, CR at once, poll for
  the receipt every 50 ms, re-press CR only while none exists.

### The condition this ran in, stated up front

This host carries a live fleet. The 1-minute load average, recorded per trial
from the point the harness started recording it, ranged from **123 to 381**
across runs. Two consequences, and both bound what may be read out of the
numbers below:

- **Cross-run latency comparisons of a few hundred milliseconds are not
  resolvable.** A run at load 135 and a run at load 320 differ by more than the
  effect being looked for.
- **Within-run comparisons are sound**, because the arms are interleaved trial
  by trial and therefore share whatever the machine was doing. Every
  head-to-head number below comes from an interleaved run.

This is not only a caveat. `experiments/briefing-submit-race` named "a loaded
fleet" as the first thing it could not speak for, and the reported stalls come
from dispatch storms — so a loaded host is the condition of interest, not a
spoiled one.

## Table 1 — head-to-head, interleaved (runB, 32 trials; runD, 12 trials)

`CR total` is every carriage return the application received across the cell,
counted from the PTY stream.

| run | arm | scenario | n | latency ms med [min–max] | CR total | CR med | receipts |
|---|---|---|---|---|---|---|---|
| B | prod | idle | 6 | 1002 [874–1668] | 9 | 1 | — |
| B | event | idle, retry 300 ms | 6 | 682 [215–8057] | 53 | 2 | 4/6 |
| B | event | idle, retry 2000 ms | 6 | 1001 [378–2300] | 7 | 1 | 6/6 |
| B | prod | busy | 5 | 920 [910–928] | 5 | 1 | — |
| B | event | busy, retry 300 ms | 5 | 7507 [5818–8062] | 109 | 23 | 3/5 |
| B | event | busy, retry 2000 ms | 4 | 6080 [5169–7333] | 14 | 3.5 | 4/4 |
| D | prod | idle, +8 CPU hogs | 4 | 890 [859–897] | 4 | 1 | — |
| D | event | idle, +8 CPU hogs | 4 | **253 [244–622]** | 4 | 1 | 4/4 |

Run B ran during the host's heaviest window; run D ran at load ~200 with the
load generator on top. The receipt is three to four times faster than the
composer read when the machine can deliver it promptly (run D), and no faster
at all when it cannot (run B). Both readings are real; neither generalises over
the other.

## Table 2 — the finding that changes the design: a busy pane queues

The composer and the receipt do not become true at the same time.

| pane state | composer clears at | receipt arrives at |
|---|---|---|
| idle | ~0.9–1.0 s (prod arm) | ~0.15–0.7 s |
| mid-response | **~0.9 s** (prod arm) | **~5.2–6.6 s** |

On a pane that is already thinking, Claude Code *queues* the pasted message.
The composer empties almost immediately — the pane says `Press up to edit queued
messages` — while `UserPromptSubmit` does not fire until the queue drains,
five to six seconds later. A loop that presses submit until the receipt arrives
therefore keeps pressing into an empty composer for the whole gap: **109
carriage returns across 5 trials, median 23 per dispatch.**

The two signals answer different questions, and the loop needs both:

- the composer clearing says **stop pressing**;
- the receipt says **it was accepted**.

`await_receipt(stop_pressing_on_clear=True)` is that, and it is what the
recommendation below is built on.

## Table 3 — carriage returns per dispatch, by loop design

| loop | idle | busy |
|---|---|---|
| prod (500 ms pause, composer poll) | 1 | 1 |
| receipt, 300 ms retry, no composer check | 2 | **23** |
| receipt, 2000 ms retry, no composer check | 1 | 3.5 |
| receipt, 300 ms retry, stop-pressing-on-clear | 1–2 | **1** |

Stop-pressing-on-clear removes the storm without slowing the idle case, which
neither of the timing-only variants does. Measured across 21 trials of the
stop-on-clear loop (runs E, F, I): median 1 carriage return per dispatch idle
*and* busy, with the receipt still delivered in 20/21.

That loop had a bug of its own worth recording, because a Rust port would
inherit it: the first version *latched* — one `capture-pane` that caught the
composer mid-redraw ended the pressing for the rest of the dispatch, and one
`resume` trial sent a single carriage return and still had the paste sitting in
the composer eight seconds later. Re-reading the composer every cycle instead
of latching fixed it: the same scenario is 5/5 afterwards, 1 carriage return
each, 277 ms median. A signal that says "stop" must be re-checked, not
remembered.

## Table 4 — extra carriage returns do not create extra submissions

The receipt is also the instrument that answers the question
`experiments/briefing-submit-race` left open: what do the duplicate carriage
returns do?

Across every trial where the hook was working, the number of receipts equals
the number of dispatches — including the busy cells where **23 carriage returns
per dispatch** were delivered to the application. In the trial with the most,
20 CRs produced exactly one receipt for our nonce (plus one `ack-nokey` for the
busy-inducing prompt, which is the keying working, not a duplicate).

So a carriage return arriving into a settled or empty composer is **inert**: it
does not submit an empty prompt, and it does not fire `UserPromptSubmit` again.
The scope of that claim is this TUI on this build with no modal on screen; a CR
into a *dialog* is a different question this harness does not ask.

## Table 5 — every failure mode, and what the typed outcome reported

| scenario | what was forced | evidence reported | fallback reason | receipts |
|---|---|---|---|---|
| `user_hook` | an operator hook already on `UserPromptSubmit` | `event_ack` | — | 1 |
| `blocking_hook` | a second hook exits 2 | `event_ack` | — | 1 |
| `hook_fail` | hook command cannot execute | `composer_cleared` | `ack_absent_composer_cleared` | 0 |
| `hooks_disabled` | no overlay at all | `composer_cleared` | `ack_absent_composer_cleared` | 0 |
| `unwritable_ack` | receipt directory mode 0500 | `composer_cleared` | `ack_absent_composer_cleared` | 0 |
| `malformed_dest` | receipt destination is a regular file | `composer_cleared` | `ack_absent_composer_cleared` | 0 |
| `suppress_first_cr` | first CR never sent | `event_ack` | — | 1 |
| `two_dispatches` | two briefings, one session | `event_ack` ×2 | — | 2, distinct nonces |
| `resume` | `claude --continue` | `event_ack` ×5 | — | 5/5 |

**8/8 broken-hook trials demoted correctly.** Not one of them reported an event
acknowledgement it did not have, and every demotion carried the reason. That is
the property the typed outcome exists for, and it is the only one in this
document that is deterministic rather than statistical.

`suppress_first_cr` is the retry-recovery test: the first carriage return is
never sent, so only the receipt-driven retry can rescue the dispatch. It did,
in 2/2, and the receipt proves the rescue rather than inferring it.

## Table 6 — what a receipt does *not* prove

`blocking_probe`, 3 trials: our receipt hook and a second hook that writes a
sentinel to stderr and exits 2, both registered on `UserPromptSubmit`.

| trials | receipt written | block sentinel visible in the pane |
|---|---|---|
| 3 | 3/3 | 3/3 |

The prompt was **rejected** — Claude Code surfaced the blocking hook's reason —
and the receipt was written anyway, in every trial. This is the measured
counterexample to the strongest reading of the mechanism:

> A receipt proves the prompt entered Claude Code's `UserPromptSubmit`
> lifecycle. It does **not** prove the model began processing it.

So the receipt retires "did the submit land?" and leaves "is the worker
working?" exactly where it was: with the `Working` / `⏺` acceptance signal.

## Table 7 — receipt delivery rate, by condition

| condition | n | receipt | composer fallback | unobserved |
|---|---|---|---|---|
| event arm, with retry (the operational shape) | 71 | 67 (94 %) | 4 (6 %) | 0 |
| event arm, single CR, no retry | 62 | 53 (85 %) | 0 | 9 (15 %) |
| hook broken by design | 8 | 0 | 8 (100 %) | 0 |
| prod arm | 15 | — | 15 | 0 |

Rows exclude four trials lost to two harness bugs found and fixed mid-run (an
unquoted environment value that stopped the hook from running at all, and a
latched flag that disarmed the retry after one transient composer reading);
both are described in the git history and re-measured after the fix.

Two readings:

1. **The fallback is not hypothetical.** Even with the hook installed and
   working, 6 % of dispatches in the operational shape got no receipt inside an
   8 s deadline and had to fall back — all of them during the host's heaviest
   window. A design that treats the receipt as always-available is wrong.
2. **The single-CR row is the swallowed-Enter rate at 0 ms delay under load:
   15 %**, against 3.6 % in the race harness's near-idle matrix. That is the
   loaded-fleet effect the earlier experiment named as its first unknown, and it
   is the reason the pause exists at all. The receipt-driven retry recovers it;
   a single unretried carriage return does not.

## The hook implementation costs more than the mechanism

Isolated benchmark, 30 invocations each, hook run directly rather than through
the TUI:

| hook command | median | p90 | max |
|---|---|---|---|
| `/usr/bin/env python3 ack_hook.py` | 368 ms | 689 ms | 1068 ms |
| `/bin/sh ack_hook.sh` | **50 ms** | 101 ms | 270 ms |

Almost all of the Python hook's cost is the pyenv shim plus interpreter
startup, before a single line of the hook runs.

It is tempting to conclude that this is what loses receipts under load, and the
controlled comparison does **not** support that. Run H measured both hooks with
a single carriage return and the same load generator: 162 ms median (8/8
receipts) for Python against 154 ms (7/8) for sh — indistinguishable end to
end, and the sh run happened to sit on a *lighter* host (load ~135 vs ~250) and
still was not faster. What loses receipts on this host correlates with overall
host load, not with which hook is installed.

The recommendation to use a compiled hook therefore stands on a bounded tail
and on not depending on a version-manager shim — not on a measured defect.

## Correlation, forgery, and the window this leaves open

The nonce is stamped by cosmon and read by the hook; it is *not* derived from
the prompt. A different prompt submitted into the same session between the
stamp and cosmon's paste would therefore sign cosmon's nonce.

- In fleet operation cosmon is the only writer to the pane, so the window is
  empty. On a pane an operator is also typing into, it is not.
- The busy trials show the keying doing its job in the direction that matters:
  the busy-inducing prompt produced `ack-nokey-<pid>`, never a receipt for the
  dispatch's nonce, because no nonce had been stamped yet.
- Closing the window exactly would mean keying on the prompt itself. A keyed
  digest (`COSMON_RECEIPT_MEASURE`, off by default) is not prompt content and is
  not invertible without the key — but the payload's `prompt` field is not
  byte-identical to what was pasted, so an exact match cannot be assumed. It is
  measurable, and it is not proposed for a first version.

## Confidentiality and blast radius

| property | how it is held | checked by |
|---|---|---|
| no prompt content persisted | only `session_id` and `hook_event_name` are read; `prompt`, `cwd`, `transcript_path` never copied | `test_ack_hook.py` searches the whole scratch tree for the prompt and the transcript path |
| no stdout | fd 1 → `/dev/null` on the first statement, before any import can warn | asserted on 8 payload/environment variations |
| cannot block a prompt | every path exits 0 | asserted on unwritable dir, non-directory dir, unset dir, missing nonce, empty/non-JSON/non-object stdin |
| receipt never half-read | temp file in the destination, `fsync`, `os.rename` | no `.ack-tmp-` residue after any case |
| hostile nonce contained | filtered to `[A-Za-z0-9_-]`, truncated to 64 | `../../../../tmp/escaped` writes inside the receipt directory and nowhere else |
| operator settings untouched | the overlay is a new 0600 file in the molecule scratch dir | SHA-256 of `~/.claude/settings.json`, `~/.claude/settings.local.json` and the project `.claude/settings.local.json` taken before the matrix and re-checked after: **identical**; no managed-settings file exists on this host, and the trials' own workspace never grew a `.claude/` directory |

The stdout property deserves its own sentence, because it is the one hazard
that would be invisible if it fired: `UserPromptSubmit` stdout is injected into
the model's context, so a hook that printed a stray line would be feeding the
worker text nobody wrote. `capture-pane` cannot see injected context, so the
harness measures it the only way it can be measured — a deliberately leaky hook
emits an *instruction* naming a token the briefing never mentions, and the
token appearing in the model's reply is the leak. The result of that probe is
reported below.

## Recommendation

**Add the receipt; do not replace the composer check with it; do not ship it
from this molecule.**

1. **Keep the 500 ms pause and the composer poll where they are.** Nothing here
   justifies removing them: the composer read observed a submit in 15/15 prod
   trials across idle, busy and loaded conditions, and the receipt did not
   arrive at all in 6 % of dispatches where it was expected to.
2. **The receipt is worth adding as a stronger, earlier signal**, not as a
   replacement. It is 3–4× faster when the machine can deliver it, it is a
   statement by the application instead of an inference from pixels, and it is
   the only instrument that could answer what the duplicate carriage returns do
   (Table 4).
3. **A receipt-driven loop must consult the composer before every re-press.**
   This is not optional tuning. Without it, a nudge into a busy worker sends 23
   carriage returns instead of 1 (Table 3) — the receipt would *cause* the
   duplicate-CR problem it was proposed to remove.
4. **The typed outcome is the load-bearing part.** `event_ack` and
   `composer_cleared` must stay distinct variants with a recorded
   `fallback_reason`. 8/8 broken-hook trials demoted correctly, and a boolean
   `submitted` would have thrown that away.
5. **Never let `event_ack` mean "the worker is working".** Table 6 is the
   counterexample, measured, 3/3.
6. **If it is implemented, the hook should be a subcommand of the already-built
   `cs` binary, invoked by absolute path** — not an interpreter, and above all
   not through a version-manager shim.
7. **The deadline needs to accommodate a queued prompt, and 8 s is already
   marginal.** A busy pane's receipt arrives 5–6 s after the paste, and one
   busy trial in five drained at 8.1 s and was demoted to composer evidence by
   the deadline rather than by anything going wrong. A shorter deadline would
   manufacture fallbacks on exactly the workers that are busiest; the deadline
   should be generous, since nothing waits on it — the composer has already
   said "stop pressing" seconds earlier.

## What this experiment does not cover

- **A managed-policy refusal.** Testing it means writing a machine-wide
  `managed-settings.json`, which is shared system state this molecule is not
  authorised to change. `hooks_disabled` models the *consequence* (no receipt,
  typed fallback) and not the mechanism.
- **A pane showing a modal** — permission prompt, trust dialog. The harness
  waits for a composer and would score those as "composer never rendered". What
  a stray carriage return does to a dialog is exactly the question Table 4
  cannot answer.
- **Any host but this one.** One machine, one build, one tmux, and a load
  average that moved by a factor of three during the run.
- **Cold start and remote/container panes**, which is where keystroke races were
  seen before (issue #20).
- **Long-run stability.** The longest session here handled two dispatches. A
  worker takes hundreds, and the receipt directory grows one small file per
  dispatch with nothing in this prototype pruning it.
