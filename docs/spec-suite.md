# Cosmon Spec Suite — Scenario Harness

The spec suite is the **executable specification** of cosmon's lifecycle.
A scenario is a small TOML document describing:

- `[given]` — the initial molecule graph (molecules + typed links)
- `[[actions]]` — the lifecycle operations applied to that graph
- `[[assert]]` — observable postconditions or trace properties

Scenarios run entirely **in-memory** via the `cosmon-scenario` crate — no
tmux, no Claude, no subprocess. Each scenario executes in under 2 ms, so the
entire suite is a fast pre-commit gate.

Run the whole suite:

```
cs test
```

Run a subset by glob:

```
cs test 'tests/scenarios/freeze-*.toml'
```

CI-friendly (exit 0 iff every scenario passes):

```
cs test >/dev/null && echo "spec suite green"
```

## Authoring a new scenario

1. Create `tests/scenarios/<slug>.toml` with the schema below.
2. Run `cs test` — a new file is picked up automatically by the default
   glob.
3. When the scenario maps onto a Constitution clause, also add it to the
   binding table in `crates/cosmon-cli/src/cmd/test.rs::BINDINGS` and
   regenerate `docs/spec-bindings.md` via `cs test --binding-report`.

### Schema

```toml
[scenario]
name = "my-scenario"
description = "one-line intent"

[binds]                      # optional — links to Constitution / Lean
constitution_clause = "merge-before-dispatch"
foundry_proposition  = "MergeBeforeDispatch_monotone"

# -- Given -----------------------------------------------------------------
[[given.molecules]]
id = "A"
kind = "task"                # task | idea | decision | issue | signal | deliberation
status = "pending"           # default: pending
steps = [
  { name = "s1", native = "cosmon::test::noop"   },
  { name = "s2", native = "cosmon::test::record" },
]

[[given.links]]
from = "A"
to   = "B"
kind = "Blocks"              # Blocks | DecayProduct | Entangled | Refines

# -- Actions ---------------------------------------------------------------
# Each table has a required `op` tag; additional fields are op-specific.

[[actions]]
op = "run_root"              # drive the given molecule (and ready
target = "B"                 #  predecessors) to a terminal state.

[[actions]]
op = "collapse"              # terminal collapse; cascades to `Blocks`-downstream.
target = "A"
reason = "manual-reject"

[[actions]]
op = "activate"              # pending → running (simulated tackle).
target = "M"

[[actions]]
op = "freeze"                # running → frozen.
target = "M"

[[actions]]
op = "thaw"                  # frozen → running (idempotent on running).
target = "M"

[[actions]]
op = "tick"                  # drain exactly one native step on one ready mol.

[[actions]]
op = "snapshot_frontier"     # record ready_frontier into the trace.

# -- Assertions ------------------------------------------------------------
# Per-molecule:
[[assert]]
molecule = "A"
status = "completed"         # pending | running | frozen | completed | collapsed
step = "2/2"                 # current_step / total
collapse_reason = "manual"   # substring match

# Trace properties:
[[assert]]
property = "merge_before_dispatch"

[[assert]]
property = "ready_frontier_monotone"

[[assert]]
property = "target_branch_base_is_blocker"
target = "B"

[[assert]]
property = "record_count_ge_1"
```

### Test-only native builtins

Registered under the `cosmon::test::*` namespace (separate from the
production `cosmon::smoke::*` natives):

| Key                    | Outcome                                   |
|------------------------|-------------------------------------------|
| `cosmon::test::noop`   | always succeeds, advances the step        |
| `cosmon::test::fail`   | returns failure → molecule collapses      |
| `cosmon::test::record` | advances + appends to the trace record log |

### Execution semantics

The harness models cosmon's lifecycle as a pure state machine:

1. **`run_root <target>`** loops until `target` is terminal or no progress.
   In each round it collects the `ready_frontier` (non-terminal molecules
   whose `Blocks`-predecessors are all **terminal** — `completed`,
   `collapsed`, or `frozen`), activates any pending members, and drains
   native steps one by one. A completed molecule is *merged* before any
   dependent dispatches (merge-before-dispatch).
2. **`collapse`** marks the target `collapsed`. It does **not** cascade:
   `blocked-by` releases on *done*, not on *verdict* (task-20260706-4d1e),
   so a collapsed blocker *releases* its forward `Blocks` dependents (they
   become ready and run) rather than dragging them into `collapsed`.
3. **`freeze` / `thaw`** are idempotent on the target's current status.
4. **Assertions** run after all actions. `property = ...` assertions
   consult the trace (frontier snapshots, dispatch events, record log).

## Founding scenarios

| File                                 | Invariant bound                                  |
|--------------------------------------|--------------------------------------------------|
| `merge-before-dispatch.toml`         | Predecessor merged before dependent dispatched.  |
| `collapse-releases-successors.toml`  | Collapse of a blocker releases (not cascades) its successors. |
| `native-step-drain.toml`             | Consecutive native steps drain in one pass.      |
| `freeze-thaw-loop.toml`              | Freeze/thaw is idempotent.                       |
| `ready-frontier-monotone.toml`       | `ready_frontier(t)` loses members only via termination. |

These files form the L2 layer of the spec suite — executable witnesses
for the L1 prose clauses in `CLAUDE.md`. Track C (Lean propositions)
will eventually discharge each one formally.
