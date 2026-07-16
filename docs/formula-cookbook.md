# Formula Cookbook — Planning Patterns for Mission Controllers

This cookbook documents **planning patterns** — strategies that a mission
controller (or any planner agent) uses at decomposition time to build DAGs
from existing cosmon primitives. These patterns require **zero code changes**;
they compose `cs nucleate --blocked-by`, `cs observe --json`, `cs complete`,
and filesystem reads.

**Audience:** Formula authors and mission-plan workers who need to express
iteration or incremental re-planning as DAG structures.

**Prerequisites:** Familiarity with the [DAG Guide](DAG-GUIDE.md) and the
[`mission-plan` formula](../.cosmon/formulas/mission-plan.formula.toml).

> **Bridge.** A formula is the template of *one* molecule. The patterns here
> wire many such molecules into a DAG by hand. The shareable, sealed template of
> that *whole* DAG is a [`spore`](vocabulary.md#spore)
> ([ADR-139](adr/139-spore-shareable-polymer-template.md)): same wiring these
> patterns build live, frozen into one portable unit. Packaging a proven
> planning pattern as a spore is its natural future home.

---

## 1. Feedback Loop Pattern

### Problem

A pipeline needs bounded iteration: a writer produces a draft, a reviewer
checks it, and if the draft is rejected the writer must revise and the
reviewer must re-check — up to N times. The DAG must be finite (no
unbounded cycles), but the number of actual iterations should emerge from
quality judgment, not be hardcoded.

### Key insight

**Unroll the loop at planning time.** A feedback loop of max N iterations
becomes a chain of 2N gate molecules pre-nucleated by the planner. Each
gate either does real work or passes through instantly via `cs complete`.

### Structure

```
writer → reviewer → gate-1 (arbiter) → gate-2 (revision) → gate-3 (re-review)
                                      → gate-4 (revision) → gate-5 (re-review)
                                      → ... → gate-2N → editor
```

The planner nucleates the full chain upfront. Each odd-numbered gate is an
arbiter/re-review; each even-numbered gate is a potential revision. If
gate-K's worker finds no issues, it runs `cs complete` immediately — a
**pass-through** that costs nothing but keeps the DAG well-formed.

### Concrete example

A wiki article goes through writer → reviewer with up to 2 revision rounds
before reaching the editor.

```sh
# Step 1: nucleate the writer
cs nucleate task-work \
  --var 'topic=Write wiki article on market microstructure' \
  --blocked-by mission-20260411-abcd \
  # → task-20260411-w001

# Step 2: nucleate the first reviewer
cs nucleate task-work \
  --var 'topic=Review wiki/market-microstructure.md — first pass' \
  --blocked-by task-20260411-w001 \
  # → task-20260411-r001

# Step 3: nucleate gate-1 — arbiter decides: revision needed or pass-through
cs nucleate task-work \
  --var 'topic=GATE [market-microstructure] round 1: read review at
    contributors/reviews/market-microstructure-review-v1.md.
    If revision needed: write revision instructions to
    contributors/reviews/market-microstructure-revision-v1.md.
    If no revision needed: cs complete immediately (pass-through).' \
  --blocked-by task-20260411-r001 \
  # → task-20260411-g001

# Step 4: nucleate gate-2 — revision (may be a pass-through)
cs nucleate task-work \
  --var 'topic=REVISION [market-microstructure] round 1: if
    contributors/reviews/market-microstructure-revision-v1.md exists,
    apply feedback to wiki/market-microstructure.md.
    Otherwise cs complete immediately (pass-through).' \
  --blocked-by task-20260411-g001 \
  # → task-20260411-g002

# Step 5: nucleate gate-3 — re-review (may be a pass-through)
cs nucleate task-work \
  --var 'topic=RE-REVIEW [market-microstructure] round 1: if revision was
    applied, review wiki/market-microstructure.md again.
    Otherwise cs complete immediately (pass-through).' \
  --blocked-by task-20260411-g002 \
  # → task-20260411-g003

# Step 6: nucleate gate-4 — second revision round (may be a pass-through)
cs nucleate task-work \
  --var 'topic=REVISION [market-microstructure] round 2: if
    contributors/reviews/market-microstructure-revision-v2.md exists,
    apply feedback. Otherwise cs complete immediately (pass-through).' \
  --blocked-by task-20260411-g003 \
  # → task-20260411-g004

# Step 7: nucleate gate-5 — second re-review (may be a pass-through)
cs nucleate task-work \
  --var 'topic=RE-REVIEW [market-microstructure] round 2: if revision was
    applied, review wiki/market-microstructure.md again.
    Otherwise cs complete immediately (pass-through).' \
  --blocked-by task-20260411-g004 \
  # → task-20260411-g005

# Step 8: nucleate the editor — blocked by the last gate
cs nucleate task-work \
  --var 'topic=Final edit of wiki/market-microstructure.md' \
  --blocked-by task-20260411-g005 \
  # → task-20260411-e001
```

### How it works at runtime

1. The writer produces `wiki/market-microstructure.md` and completes.
2. The reviewer reads the article, writes feedback, and completes.
3. **Gate-1 (arbiter):** the worker reads the review. Two outcomes:
   - **Revision needed:** writes revision instructions to a known path,
     then completes. Gate-2's worker will find the file and do the revision.
   - **No revision needed:** runs `cs complete` immediately. Gate-2's
     worker finds no revision file and also passes through. The chain
     collapses to near-zero cost.
4. Gates 2–5 follow the same logic: check if upstream produced work,
   act or pass through.
5. The editor receives the final version — whether it went through 0, 1,
   or 2 revision rounds.

### Design notes

- **Max iterations = N** is a planning decision. The planner chooses N
  based on the fleet template or mission goal. More gates = more possible
  iterations but also more molecules in the DAG.
- **Pass-through cost is minimal.** A molecule that runs `cs complete`
  immediately consumes ~one API call (the `cs tackle` bootstrap). This is
  the "price" of a bounded loop — pre-allocated but only consumed if needed.
- **No dynamic DAG mutation.** The chain is fully determined at planning
  time. This preserves the DAG's static analyzability (you can always
  `cs deps --transitive` to see the full structure).
- **Communication is via the filesystem.** The arbiter signals "revision
  needed" by writing a file. The revision worker checks for that file.
  No mailboxes, no message passing — just the data plane.

### Alternative: dynamic revision via DAG growth

The `mission-plan` formula's Revision Protocol (see the `decompose` step)
uses a different approach: the reviewer dynamically nucleates revision and
re-review molecules at runtime. This is more flexible (unbounded iterations)
but produces DAGs whose final shape is not known at planning time. Choose
based on your needs:

| Approach | Iterations | DAG shape | Best for |
|----------|-----------|-----------|----------|
| Unrolled gates (this pattern) | Bounded (N) | Static, known at planning time | Predictable pipelines, cost control |
| Dynamic nucleation (Revision Protocol) | Unbounded | Grows at runtime | Quality-critical content, unknown revision depth |

---

## 2. Fleet Continuity Pattern

### Problem

A planner runs `just mission` (or is invoked by `cs tackle` on a
mission-plan molecule) against a project that already has completed work.
For example, a wiki project where 8 of 12 articles already exist as
completed molecules. The planner must **compute the delta** — nucleate
only the missing work, not re-do what's already done.

### Key insight

**Read the current state before decomposing.** The planner uses
`cs observe --json` to enumerate existing molecules, reads the filesystem
to see what outputs already exist, and nucleates only the gaps. Existing
completed molecules are visible in the state store — the planner's prompt
must say "read the current state before decomposing."

### Structure

```
┌─────────────────────────────────────────────┐
│  Planner (mission-plan molecule)            │
│                                             │
│  1. cs observe --json --all                 │
│     → list of existing molecules + status   │
│                                             │
│  2. Read wiki/*.md                          │
│     → list of existing outputs              │
│                                             │
│  3. Compute delta:                          │
│     desired_topics - existing_completed     │
│     = missing_topics                        │
│                                             │
│  4. cs nucleate only for missing_topics     │
└─────────────────────────────────────────────┘
```

### Concrete example

A cosmopedia mission with a fleet template that defines researcher →
writer → reviewer pipeline. The project already has completed articles
for 3 topics. The planner needs to add 2 new topics.

```sh
# Step 1: the planner queries existing state
cs observe --json --all --formula task-work
```

Output (abbreviated):
```json
[
  {"id": "task-20260410-a1b2", "status": "completed",
   "variables": {"topic": "Write wiki article on order flow"}},
  {"id": "task-20260410-c3d4", "status": "completed",
   "variables": {"topic": "Write wiki article on market impact"}},
  {"id": "task-20260410-e5f6", "status": "completed",
   "variables": {"topic": "Write wiki article on LOB dynamics"}},
  {"id": "task-20260410-g7h8", "status": "running",
   "variables": {"topic": "Review wiki/order-flow.md"}}
]
```

```sh
# Step 2: the planner reads the filesystem
ls wiki/
# → order-flow.md  market-impact.md  lob-dynamics.md
```

```sh
# Step 3: compute the delta
# Desired topics (from source material analysis): 5
# Existing completed writers: 3 (order-flow, market-impact, lob-dynamics)
# In-progress: 1 (order-flow review)
# Missing: 2 (hawkes-processes, propagator-models)
```

```sh
# Step 4: nucleate ONLY the missing work

# Researcher for hawkes-processes
cs nucleate task-work \
  --var 'topic=Research hawkes processes for wiki article' \
  --blocked-by mission-20260411-xyz0 \
  # → task-20260411-rh01

# Writer for hawkes-processes (blocked by researcher)
cs nucleate task-work \
  --var 'topic=Write wiki article on hawkes processes' \
  --blocked-by task-20260411-rh01 \
  # → task-20260411-wh01

# Reviewer for hawkes-processes (blocked by writer)
cs nucleate task-work \
  --var 'topic=Review wiki/hawkes-processes.md' \
  --blocked-by task-20260411-wh01 \
  # → task-20260411-rvh01

# Researcher for propagator-models
cs nucleate task-work \
  --var 'topic=Research propagator models for wiki article' \
  --blocked-by mission-20260411-xyz0 \
  # → task-20260411-rp01

# Writer for propagator-models (blocked by researcher)
cs nucleate task-work \
  --var 'topic=Write wiki article on propagator models' \
  --blocked-by task-20260411-rp01 \
  # → task-20260411-wp01

# Reviewer for propagator-models (blocked by writer)
cs nucleate task-work \
  --var 'topic=Review wiki/propagator-models.md' \
  --blocked-by task-20260411-wp01 \
  # → task-20260411-rvp01

# Cross-cutting roles blocked by ALL reviewers (existing + new)
cs nucleate task-work \
  --var 'topic=Horizontal edit: consistency across all 5 wiki articles' \
  --blocked-by task-20260410-g7h8 \
  --blocked-by task-20260411-rvh01 \
  --blocked-by task-20260411-rvp01 \
  # → task-20260411-he01
```

### How to encode this in a mission-plan formula

The `mission-plan` formula's `analyze` step is the natural place for delta
computation. The planner's briefing should include:

```markdown
## Fleet Continuity

Before decomposing, read the current project state:

1. Run `cs observe --json --all` to list every molecule in this project.
2. Read the output directory (e.g. `wiki/`) to see what artifacts exist.
3. For each desired work unit from the source material:
   - If a completed molecule already produced this artifact → skip.
   - If a molecule is in-progress (running/active) → do not duplicate.
   - If no molecule exists for this work unit → nucleate the full pipeline.
4. Wire new molecules' `--blocked-by` edges to include any existing
   in-progress molecules that must complete first (e.g., a new
   horizontal-editor must wait for both old and new reviewers).
```

### Design notes

- **Idempotent re-planning.** Running the mission planner twice on the
  same project should produce zero new molecules the second time (all
  work already exists). This is the key property.
- **No special "resume" command.** Fleet continuity is a planning
  pattern, not a runtime feature. The planner reads state and acts
  accordingly — the same `cs nucleate` / `cs observe` primitives used
  for first-run planning.
- **Cross-cutting roles must account for all predecessors.** When the
  planner nucleates a horizontal-editor, it must block on **all**
  reviewers — both from previous runs (already completed or in-progress)
  and from the current decomposition. Use `cs observe --json --formula
  task-work --status completed` to find existing gate molecules.
- **The planner prompt is the mechanism.** There is no code-level
  enforcement of delta computation. The fleet template's planner role
  prompt must explicitly instruct: "read the current state before
  decomposing." This is by design — the planner is an agent making
  judgment calls about what constitutes a duplicate, not a mechanical
  diff engine.

### Cross-reference

- **`mission-plan` formula:** The `analyze` and `decompose` steps in
  [`.cosmon/formulas/mission-plan.formula.toml`](../.cosmon/formulas/mission-plan.formula.toml)
  define the standard mission planning lifecycle.
- **Fleet templates:** `.fleet.toml` files define roles and pipeline
  structure. The planner reads these to know which pipeline stages to
  create for each work unit.
- **`cs observe --json --all`:** The `--all` flag includes completed and
  collapsed molecules, which is essential for delta computation (without
  it, finished work is invisible to the planner).

---

## 3. Auto-Freeze Gate Pattern

### Problem

A formula needs the molecule to freeze at the end (e.g. a persistent
mission-controller that will be thawed later). The `freeze_on_last_step`
flag exists in the formula, but when a Claude worker reaches the last
agent step, it must **execute** `cs freeze` — a cognitive instruction in
prose that workers consistently fail to follow. Observed in wiki2
(mission-64a1), wiki3 (mission-3c36), foundry (mission-842e).

### Key insight

**Replace cognitive instructions with mechanical gates.** A shell gate step
(`command = "cs freeze ..."`) fires automatically after the agent step
completes. The worker never needs to decide whether to freeze or complete —
the gate does it mechanically.

### Structure

```toml
[[steps]]
id = "verify"
title = "Verify the DAG"
description = """
Do the verification work. After this step, auto-freeze fires
automatically. Just call `cs evolve` when done.
"""
needs = ["decompose"]

[[steps]]
id = "auto-freeze"
title = "Freeze the molecule (mechanical)"
command = "cs freeze $(cs observe --json | jq -r .id) --reason 'verified — waiting for feedback'"
timeout = 30
needs = ["verify"]
```

### How it works

1. Worker completes the verify step and calls `cs evolve`.
2. `cs evolve` advances to the auto-freeze step.
3. Since auto-freeze is a shell gate, `cs evolve` auto-executes it inline.
4. The `cs freeze` command runs, setting the molecule status to Frozen.
5. The molecule is now frozen — no cognitive compliance required.

### When to use

Use this pattern whenever a formula has `freeze_on_last_step = true`.
The `freeze_on_last_step` flag ensures correct behavior in the DAG runtime
(`cs run`), while the auto-freeze gate ensures correct behavior when a
worker calls `cs evolve` from within its session.

The pattern applies to:
- `mission-controller` — freezes after initial decomposition, thawed by
  citation-arbiter for feedback cycles.
- `mission-plan` — freezes after DAG verification, operator thaws to
  run the DAG.
- Any formula where the molecule must persist beyond its last step.

### Design notes

- **Belt and suspenders.** The formula keeps `freeze_on_last_step = true`
  (for the runtime's DAG policy, which treats frozen as completed-for-DAG-
  purposes) AND has the shell gate (for worker-initiated evolve). Both
  paths converge on the same outcome.
- **`cs evolve` auto-executes gates.** When `cs evolve` advances to a
  step that has a `command` field, it runs the command inline via `sh -c`.
  This makes mixed agent→gate sequences seamless.
- **The verify step prose should mention freeze for context** ("you will
  be thawed when...") but no longer relies on the worker executing it.

---

## Summary

All three patterns share the same principle: **the DAG is the program.** Complex
behaviors (iteration, resumption, lifecycle transitions) are expressed as DAG
topology and shell gates, not as cognitive instructions to agents. The planner
is the compiler that translates intent into molecule graphs using existing
primitives.

| Pattern | Primitive used | Planning-time cost | Runtime cost |
|---------|---------------|-------------------|-------------|
| Feedback loop | `cs nucleate --blocked-by` chain + pass-through `cs complete` | 2N extra molecules | Near-zero for unused gates |
| Fleet continuity | `cs observe --json --all` + filesystem reads + selective `cs nucleate` | One state query | Zero (no redundant work) |
| Auto-freeze gate | Shell gate step with `cs freeze` command | One extra step | One shell invocation |
