# Spark Capture — the discipline

> **Cosmon is a machine for catching sparks.**
> The axiom `P_external`
> asserts that some bits of system value cannot be generated from the
> inside. Spark-capture is the operational discipline that makes those
> bits catchable — with the same rigor as any engineered primitive.

## TL;DR — one screen

| Question | Answer |
|---|---|
| What is a spark? | A bit of value entering the system from *outside* its designed primitives. |
| Where does it come from? | Substrate affordances (internal), emergent synthesis (cognitive), or outside observers (external). |
| How do you know you caught one? | A gesture you have already made three times, with no name. |
| What do you do with it? | Name it. Instrument it. Chronicle the moment that made it visible. |
| What do you NOT do? | Invent features. Chronicle routine fixes. Canalise one-shots. |
| How often? | Weekly sweep via `cs nucleate spark-capture`, or on-demand after an intense session. |

## Three classes of sparks

The 2026-04-14 trilogy establishes three classes. Each has a distinct
source and a distinct canalisation path.

| Class | Source | Canalisation |
|---|---|---|
| **Internal** (pilot) | The substrate reveals an affordance in use. The operator does the gesture *before* it has a name. | Name → `cs` verb or formula step. Chronicle if the gesture exposes a channel. |
| **Cognitive** (worker) | A `synthesis.md` from a deliberation exceeds the sum of its inputs. Emergence. | Record as pattern; chronicle if it illuminates a principle. |
| **External** (vetoer) | A non-insider signs an observation about the whole universe. | Cryptographic artifact → hash chain → chronicle on contest or confirmation. |

Only the third class can certify the universe itself. The first two
improve it; the third proves it is coherent.

## When to capture

A spark is worth capturing when **all three** conditions hold:

1. **Recurrence** — the gesture has appeared ≥ 3 times across distinct
   molecules, commits, or sessions. One-shots are quirks, not sparks.
2. **No name** — there is no `cs <verb>`, no formula step, no documented
   primitive for it. If `cs help` already lists it, you are late.
3. **Illuminates a principle** — naming the gesture would expose an
   invariant (a channel boundary, a regime transition, a control/data
   plane separation). Pure feature requests fail this test.

If a candidate fails any of the three, it is **not** a spark. Log it in
the sweep's anti-list, do not chronicle it, do not nucleate a formula
for it.

## When NOT to chronicle

From an internal chronicle and reinforced here:

- Routine bug fixes, build errors, feature additions — changelog, not
  chronicle.
- Generic refactors — commit message is enough.
- "We added a command" — only chronicle if the *act of adding* exposed a
  principle.
- Any sweep report — the report is the artifact; the chronicle is a
  later, reflective act.

**Err on the side of not writing.** The density of the Chronicles is
their value.

## The sweep (`spark-capture` formula)

Run weekly, or on-demand:

```
cs nucleate spark-capture
cs tackle <id>
cs wait <id> &
# ...worker writes scan.md, candidates.md, capture-report.md...
cs done <id>
```

Three steps:

1. **scan** — collect raw signals from `events.jsonl` (last 14 days),
   `git log` (last 14 days), and recently-modified chronicle notes.
   Group into clusters by gesture.
2. **detect** — apply the three-criterion filter. Classify surviving
   candidates by class. Record the anti-list explicitly.
3. **report** — produce `capture-report.md` with per-candidate table,
   top recommendation, anti-list, and suggested chronicle slug.

The sweep does **not** auto-nucleate follow-ups. The operator reads
the report and decides — canalise now (nucleate a `task-` molecule),
park (tag `temp:warm`), or reject (add to the anti-list permanently).

## Oracle heuristics — what makes a gesture visible

From the trilogy:

- **Fear** — when the operator fears a scenario ("the probe will be
  lost"), they are naming a latent primitive. Fear is a spark oracle.
  Do not repress it; transcribe it.
- **Accidental usage** — when the operator reaches for tmux, tail, or
  cat to do something the system *almost* supports, they are standing
  on an affordance. The third time, name it.
- **External surprise** — when a non-insider says "this does not mean
  what you think it means", they have just supplied a bit the inside
  could not generate. Sign it, chronicle it, fold it in.

## Hazard

The discipline invites rigour, not laxity. A **harmful** affordance
(shell injection into an idle non-Claude worker, for example) is also
a spark candidate — and therefore deserves the same treatment: name
it, instrument it, guard-rail it. Unnamed hacks become ghosts that
get reinvented in loops. Naming them is the first line of defence.

## Relation to other sweeps

| Sweep | Curates | Horizon |
|---|---|---|
| `temp-review` | the backlog (molecules on the shelf) | weekly |
| `spark-capture` | the conceptual frontier (unnamed primitives) | weekly |
| `oversee` | a running mission (pathologies, interventions) | per mission |
| `absorb` | post-mission mutations worth folding back | per mission |

Spark-capture is the **meta** sweep — it watches the gestures the
system itself is making, and asks whether any of them has earned a
name.

## References

- Chronicle trilogy — anthropic channels,
  resurrection,
  sparks & P_external.
- Formula — [`.cosmon/formulas/spark-capture.formula.toml`](../.cosmon/formulas/spark-capture.formula.toml).
- Sibling discipline — [`.cosmon/formulas/temp-review.formula.toml`](../.cosmon/formulas/temp-review.formula.toml).
- Governing thesis — [`THESIS.md`](../THESIS.md), architectural
  invariants in [`docs/architectural-invariants.md`](architectural-invariants.md).

## Worked example — three candidates from the 2026-04-14 state

These are the first three concrete candidates extracted from the
chronicles and events as of 2026-04-14. They are the reference
output of a spark-capture sweep run against the current state.

### 1. `cs whisper` — paste a semantic nudge into a running worker

**Class:** internal (pilot).

**Why it is a spark.** On 2026-04-14 the pilot needed to inject a
live case into a running deliberation panel (`delib-20260414-cb40`).
Nothing in `cs` exposed the gesture, so the pilot used
`tmux load-buffer` + `tmux paste-buffer` + `Enter × 2` to queue a
message into Claude's CLI. The affordance existed in the substrate
— tmux paste is real, Claude robustly queues pasted input — but the
gesture had no name. The same gesture had already been invoked for
the Enter-nudge watchdog (`scripts/cs-paste-nudge.sh`) and again for
the *voyager anthropic* probe. Three distinct uses, zero primitives.
This illuminates a principle: cosmon has a sixth communication
channel (live semantic injection into a worker) that is not the DAG,
not the filesystem, and not the artifact chain.

**Canalisation sketch.** Introduce `cs whisper <mol_id> <msg-file>`
that resolves the worker's tmux socket + session via the fleet
registry, pastes the buffer, presses Enter twice, and records the
event as `whisper_sent` in `events.jsonl`. Guard-rail: refuse on
non-live sessions, refuse on completed molecules. Document the
channel in [`docs/handbook.md`](handbook.md) as *channel 6 —
whisper*. Chronicle the first production use.

### 2. `cs sanity-probe` — verify a delegated binary before dispatch

**Class:** internal (pilot), born from fear.

**Why it is a spark.** On 2026-04-14 `cs tackle` regressed in a
nightly refactor ("fleet-scoped tmux socket" + "intent+receipt
pattern"). The regression was invisible until a `delib-20260414-b8e2`
session failed to spawn a worker, turning a nucleated molecule into
a seabed wreck (chronicle:
resurrection-and-fear-driven-discovery).
The pilot has repeatedly asked, on three occasions, "did cs tackle
still work?" before dispatching. The gesture — *probe the binary's
golden path before relying on it* — has no name. This illuminates a
principle: the transactional core is a delegated oracle, and
delegated oracles need a liveness probe (Turing would call it a
self-consistency check).

**Canalisation sketch.** Introduce `cs sanity-probe` that runs a
minimal round-trip — nucleate a throwaway molecule, tackle, evolve,
complete, done — inside a sandboxed `.cosmon/` under `/tmp`, and
emits a machine-readable verdict. Wire into the `cs done` post-merge
hook (so every merge to main attests the binary still works end-to-
end). On failure, freeze `cs tackle` in a visible way and force the
operator to `cs resurrect` the next dispatch (see candidate 3).

### 3. `cs resurrect` — re-differentiate a molecule from its artifacts

**Class:** internal (pilot), born from fear and chronicled before
being implemented.

**Why it is a spark.** When `delib-20260414-b8e2`'s session died,
the artifacts survived: `prompt.md`, `briefing.md`, `log.md`,
partial `synthesis.md`, `responses/`, `events.jsonl`. The pilot
realised these files are the worker's cognition compressed by the
act of their generation — the bottleneck through which the thinking
passed. This is the inverse of `/compact`: instead of heuristically
summarising, the system *hands back* the proof-of-work trail to a
fresh worker, which resumes at the clean edge of a step. The
gesture has been imagined three times (whisper delib, task-17a8,
the resurrection delib itself — now self-referentially stranded)
with no name. It illuminates a principle: artifacts are not just
audit trail, they are the *cognitive compression* of a session, and
therefore the canonical handoff format.

**Canalisation sketch.** Introduce `cs resurrect <mol_id>` that
rehydrates the molecule state, re-tackles into a new worktree/tmux
with the original branch restored, and injects a briefing that tells
the fresh worker it is a continuation (not a replay) — listing which
steps are already done, which artifacts are load-bearing, and where
to resume. Emit `resurrected` events with a link to the predecessor
worker. Chronicle the first successful resurrection. Treat the
*artifacts / context-window* ratio as an operational metric of
cognitive compression efficiency.

## Principle

**A spark is a bit of value the closed system could not have produced.
The discipline is to notice it, name it, and fold it back — with the
same rigour as any engineered primitive. Do this and cosmon becomes a
machine for catching sparks; skip it and those bits get re-invented
as hacks, in loops, forever.**
