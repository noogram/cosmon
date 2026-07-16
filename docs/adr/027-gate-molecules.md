# ADR-027: Gate Molecules as First-Class DAG Nodes

## Status
Accepted (2026-04-11)

**Bound to:** [ADR-016](016-autonomy-regimes-and-resident-runtime.md) (Autonomy Regimes),
[ADR-022](022-native-dag-scheduler.md) (Native DAG Scheduler),
[ADR-026](026-dynamic-fleet-orchestration.md) (Dynamic Fleet Orchestration).

**Origin:** Deliberation `delib-20260411-3e6d` (six-persona panel: architect,
von-neumann, jobs, feynman, torvalds, knuth).

## Context

Cosmon verification operates at two tiers:

- **Tier 1 (intra-step):** `VerificationSpec` wired into `cs evolve` — a step
  cannot advance unless its verification command passes. This is handled by
  `task-20260411-3f18` and lives inside the formula.
- **Tier 2 (inter-molecule):** Between a writer completing and a reviewer
  starting, a mechanical check must run (DOI-check, zotero-ref-check,
  link-checker, equation-validator). These checks verify the *output* of one
  molecule before the next molecule consumes it.

Tier 2 cannot be a formula step because: (a) it must run *after* the
predecessor's branch merges (merge-before-dispatch), so it sees the merged
output on disk; (b) it must be visible in `cs deps` as an explicit pipeline
stage; (c) its failure must cascade via `cs collapse` to block downstream
dependents; (d) it has its own lifecycle (pending → running → completed or
collapsed) — observable, restartable, debuggable.

The deliberation panel achieved convergence: Tier 2 gates are **separate
molecules in the DAG**, not formula steps, not hooks, not a new mechanism.
This follows the composability principle: everything tracked by cosmon is a
molecule.

## Decisions

### 1. Gate molecules use kind `signal` (⚡)

Gate molecules are fast mechanical checks, not cognitive work. The `signal`
kind already exists for low-latency, non-deliberative molecules. A gate is
a signal that either passes (completed) or fails (collapsed).

### 2. Gate molecules use the `gate` formula

A new formula `gate.formula.toml` defines a single step whose verification
command IS the gate check. The formula has one step: "run the gate command."
The command itself is encoded in the molecule's topic field.

### 3. Topic encodes the gate command

A gate molecule's topic contains the command prefixed with `GATE:`:

```
GATE: cargo run -p doi-check -- wiki/{slug}.md
```

The executor extracts the command from the topic and runs it as a subprocess.
This keeps gate molecules self-describing — `cs observe` shows what the gate
does without reading external config.

### 4. The planner nucleates gate molecules between pipeline stages

The delegation planner (a deliberation molecule per ADR-026 §1) inserts gate
molecules as explicit DAG nodes between writer and reviewer:

```
writer --[Blocks]--> doi-check-gate --[Blocks]--> reviewer
```

Gate molecules participate in the normal DAG topology. `cs deps` shows them.
`cs wait` respects them. `ready_frontier` skips them until predecessors
complete.

### 5. Gate molecules execute without a tmux session

The executor (SubprocessExecutor or future runtime) detects gate molecules
by kind (`signal`) + topic prefix (`GATE:`) and runs them as a pure
subprocess — no Claude session, no tmux pane, no token expenditure. A gate
costs ~0.01s of CPU; a cognitive molecule costs ~$0.50 in tokens. This is
the key cost asymmetry that justifies mechanical gates as separate molecules
rather than cheap cognitive molecules.

Execution semantics:
- **Success (exit 0):** `cs complete <gate-id> --reason "gate passed"`
- **Failure (exit non-zero):** `cs collapse <gate-id> --reason "gate failed: <stderr>"`

### 6. Future: `[[gates]]` shorthand in fleet.toml

A future enhancement may allow declaring gates declaratively:

```toml
[[gates]]
after = "writer"
before = "reviewer"
command = "cargo run -p doi-check -- wiki/{slug}.md"
```

This is **sugar over the molecule primitive** — the planner expands it into
gate molecules at decomposition time. It does not introduce a new mechanism,
a new state store, or a new execution path. This shorthand is out of scope
for the initial implementation.

## Consequences

### Positive

- **No new mechanism.** Gates reuse molecules, formulas, DAG links, and the
  existing lifecycle. The composability principle holds.
- **Visible in the DAG.** Operators see the full pipeline including
  mechanical checks via `cs deps`.
- **Cascade on failure.** A failed gate collapses, blocking all downstream
  dependents — the reviewer never starts on broken input.
- **Zero token cost.** Gate execution is a subprocess, not a Claude session.
  Mechanical verification scales to hundreds of gates per pipeline.
- **Merge-before-dispatch respected.** The gate runs after the predecessor's
  branch merges, so it sees the actual merged output.
- **Observable and restartable.** Gate molecules have full lifecycle —
  `cs observe`, `cs thaw` (to retry), `cs collapse` (to fail permanently).

### Negative

- **DAG verbosity.** Pipelines gain additional nodes for each gate. Mitigated
  by the `[[gates]]` shorthand (future) and by `cs deps` collapsing signal
  molecules visually.
- **Planner complexity.** The planner must know which gates to insert between
  stages. Initially this is explicit in the mission prompt; the `[[gates]]`
  shorthand automates it later.
- **New formula.** A `gate.formula.toml` must be written and maintained.
  However, it is trivial (single step, no verification beyond the command
  itself).

### Neutral

- Gate molecules do not change the Tier 1 (intra-step) verification mechanism.
  Both tiers coexist: Tier 1 guards step transitions within a molecule;
  Tier 2 guards molecule transitions within a DAG.
- The `signal` kind gains a sub-semantics (gate signals vs. notification
  signals). This is acceptable — kind is a hint, not a type constraint.

## References

- Deliberation: `delib-20260411-3e6d`
- Related: `task-20260411-3f18` (Tier 1 VerificationSpec)
- Composability Principle: [CLAUDE.md](../../CLAUDE.md) §Composability Principle
- Merge-before-dispatch: [architectural-invariants.md](../architectural-invariants.md)
