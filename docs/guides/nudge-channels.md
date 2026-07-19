# Nudge channels — every path that speaks into a live worker

Cosmon can push a sentence into a running worker's terminal without the worker
asking for it. This document is the **complete inventory** of those paths and
the rule that governs all of them.

It exists because an incomplete inventory is how the 2026-07-19 regression
happened twice. The first repair (thinking-worker spam) landed in `patrol.rs`
and left two sibling emitters carrying their own untouched copy of the same
heuristic. The second (a worker gated on an operator, nudged dozens of times
toward the action the gate withheld) proved the pattern was structural, not a
one-off miss.

## The rule

> **Every unbidden nudge passes through `cosmon_core::propel::decide_nudge`.**
> No emitter decides "is this worker idle?" for itself.

An emitter assembles a `NudgeView` — its `NudgeChannel`, the molecule's status,
whether the worker is gated on the operator, the two clocks, and its attempt
ledger — and obeys the verdict. Per-channel tuning goes into the `stale_after`
the emitter passes. It never goes into a second copy of the rule.

Adding a new channel means adding a `NudgeChannel` variant and calling the
judge. If you find yourself writing `if age > threshold` next to a
`send_input`, you are re-creating the bug.

## The inventory

Three **unbidden** channels — cosmon decides, the worker did not ask:

| Channel | Emitter | Message | Judge |
|---|---|---|---|
| `Propulsion` | `cs patrol --propel` → `propel_stale_molecules` | `PROPULSION_NUDGE` | ✅ `decide_nudge`, per-stall ledger + exponential backoff + ceiling |
| `Briefing` | `cs patrol --nudge` → `nudge_stalled_molecules` | `nudge_message(briefing)` | ✅ `decide_nudge`, idempotence window, no ceiling |
| `Heal` | `cs patrol --heal` remedy `A2` → `apply_remedy` | `nudge_text(briefing)` | ✅ operator gate; the diagnosis supplies the rest |

Everything else that reaches a worker's terminal is **operator- or
lifecycle-initiated** and is deliberately *not* subject to this gate — a human
or an explicit verb asked for it:

| Path | Why it is not gated |
|---|---|
| `cs tackle` (bootstrap prompt) | the molecule's opening contract |
| `cs resume`, `cs thaw` | explicit operator verbs resuming a suspension |
| `cs whisper` | the operator's own message to a worker (ADR-038 ch. 6) |
| `cs done` auto-propel on merge conflict | bounded escalation the operator asked for with `--auto-propel` |
| `patrol --heal` remedy `A1` (bare Enter) | zero bytes; submits a paste the worker already composed |

If you add a path to the left column, add it to the table *and* to the judge.

## Why the operator gate outranks the clocks

A worker parked at `cs await-operator` has both clocks cold: no progress events,
a silent terminal. It is indistinguishable from a crashed worker by any
mechanical test — which is why every idleness heuristic gets it wrong.

Everywhere else in the judge, being wrong in the permissive direction costs a
wasted sentence. Here it costs the boundary. A gate that opens under enough
repetition was never a gate, and "continue execution immediately", repeated
indefinitely at a worker holding a signature, is exactly that repetition.

So `NudgeSkip::AwaitingOperator` is checked first, before the status and before
both clocks. Two witnesses count, either one sufficient:

- the `temp:awaiting-op` tag (`cosmon_core::operator_block::AWAITING_OP_TAG`),
  stamped by `cs await-operator`;
- the durable `blocked_on.json` in the molecule dir.

Belt and suspenders on purpose: a reconcile or hand-edit that drops tags must
not silently re-open the door.

A gated candidate also skips the liveness projection. Its process is healthy and
its silence is the intended behaviour; stamping it `Unresponsive` would
manufacture a health anomaly out of a correct pause.

## Reading the report

`cs patrol --propel` prints every decline, never just the sends:

```
  PROPEL 1 worker(s) propelled (4 running, threshold 300s):
    - w-a12f ← task-20260719-abcd (stale 640s)
    · w-b31c ← task-20260719-bcde (working — pane active 3s ago, not nudged)
    · w-c02a ← task-20260719-cdef (awaiting operator — questions pending, not nudged)
    · w-d55e ← task-20260719-defa (backoff — next nudge in 420s)
    ! w-e77b ← task-20260719-efab (4 nudges ignored — tagged `propel-exhausted` for `cs patrol --heal`)
```

The `awaiting operator` line is the one an operator must act on. That molecule
is not stuck. It is waiting on *them*.

## See also

- `crates/cosmon-core/src/propel.rs` — the judge and its rationale
- ADR-123 — `cs await-operator`, the only sanctioned worker→operator block
- ADR-137 §2 — why the pane signal is a *duration*, never rendered text
- ADR-062 — why a `Starved` molecule must never be re-prompted
