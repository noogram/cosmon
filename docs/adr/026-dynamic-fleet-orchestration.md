# ADR-026: Dynamic Fleet Orchestration Architecture

## Status
Accepted (2026-04-10)

**Bound to:** [ADR-016](016-autonomy-regimes-and-resident-runtime.md) (Autonomy Regimes),
[ADR-022](022-native-dag-scheduler.md) (Native DAG Scheduler),
[ADR-024](024-worker-output-yield-protocol.md) (Worker Output Yield Protocol).

## Context

A six-persona panel (`delib-20260410-f312`: architect, von-neumann, jobs,
feynman, torvalds, knuth) deliberated the architecture for dynamic fleet
orchestration — specifically, how cosmon advances from single-molecule dispatch
(`cs tackle`) to multi-agent DAG execution (`cs run`).

The question: given `DagPolicy` + `compile_plan` + `BlockedBy`/`Blocks` links +
decay splicing (all implemented per ADR-022), what architectural decisions bind
the orchestration layer?

The panel achieved 6/6 or 5/6 convergence on six load-bearing findings. This
ADR records those decisions as architectural constraints.

## Decisions

### 1. The planner is a deliberation molecule, not runtime code (6/6)

The delegation planner — the component that decomposes a goal into a DAG of
typed, role-assigned molecules — is itself a **deliberation molecule**
(kind: `🧠`). It executes as an agent (via `cs tackle`) and produces task
molecules through `cs nucleate --blocked-by` or `cs decay`.

**Rationale:** Every panelist independently arrived at this conclusion.
feynman: "the planner is literally the editor-in-chief agent." torvalds:
"it's a script that calls `cs nucleate` N times." von-neumann: "the planner
is a compiler, not an optimizer — role specialization forces assignment,
making the problem linear."

**Constraint:** The runtime MUST NOT contain planning logic. It schedules
(via `DagPolicy`) and dispatches (via `Executor`). It never decomposes work.
The two-layer model ([ADR-016 §1](016-autonomy-regimes-and-resident-runtime.md))
is preserved: the resident runtime is a pure scheduler with pluggable policies,
not a planner.

**Implication:** No new molecule kind, formula type, or CLI command is needed
for the planner. An agent running a deliberation formula with MCP access to
`cosmon_nucleate` and `cosmon_decay` IS the planner.

### 2. `assigned_role: Option<AgentRole>` on `MoleculeData` — phase 2 (5/6)

For fully autonomous `cs run` (where the runtime spawns workers without human
intervention), the runtime needs to know which role a molecule requires.

**Decision:** Add `assigned_role: Option<AgentRole>` to `MoleculeData` as a
single optional field. NOT a new `RoleTemplate` type, NOT a new abstraction.

**Phasing:**
- **Phase 1 (propelled regime):** Not needed. `cs tackle` already binds
  molecules to workers; the human/operator chooses which worker. The `Executor`
  trait dispatches without role awareness.
- **Phase 2 (autonomous regime):** Required. The runtime's `Executor`
  implementation resolves `assigned_role` → available worker at dispatch time.
  The planner (deliberation molecule) sets `assigned_role` when nucleating tasks.

**Why not now:** torvalds and feynman agree that for the propelled regime,
`cs tackle <id>` is sufficient — the operator or patrol selects the worker.
Adding `assigned_role` before the autonomous regime exists would be premature
sugar (feynman: "a field with no consumer is a lie in the type system").

### 3. `Executor` trait for worker dispatch (5/6)

The gap between "schedule what to run next" (`DagPolicy`) and "actually run it"
is bridged by the `Executor` trait, now implemented in `cosmon-runtime`:

```rust
pub trait Executor {
    fn dispatch(&self, id: &MoleculeId) -> Result<(), RuntimeError>;
}
```

`Runtime::apply_evolve` transitions a molecule to `Running` then calls
`self.executor.dispatch(id)`. The default implementation (`SubprocessExecutor`)
invokes `cs tackle <id>` as a subprocess.

**Why this shape:**
- torvalds: "inject an `Executor` trait into `Runtime` — simpler than a
  `FleetDagPolicy` wrapper and more aligned with the two-layer model."
- The runtime is a **client** of the transactional core. It calls `cs tackle`
  (a transactional command) rather than reimplementing tackle's logic.
- The trait is the single extension point for phase 2: a `RoleAwareExecutor`
  that reads `assigned_role` and selects the worker can replace
  `SubprocessExecutor` without changing any other code.

**Rejected alternative:** architect's `FleetDagPolicy` wrapping `DagPolicy`
to intercept `Evolve` actions and emit `Tackle`. This adds indirection
(policy returning a new action type that the runtime must re-interpret) and
blurs the separation between scheduling decisions and dispatch mechanics.

### 4. Output aggregation via `variables` convention (5/6)

When a molecule produces output that downstream molecules consume, the
convention is:

```toml
[variables]
output_path = ".cosmon/state/fleets/default/molecules/<id>/output/"
```

Workers write their outputs to this path. Downstream molecules read upstream
outputs by resolving `BlockedBy` links and reading the referenced molecule's
`variables.output_path`.

**Why convention over type:** architect proposed `MoleculeLink::Yields { artifact, kind }`
as a structured variant. feynman and torvalds counter: "don't build
infrastructure for a problem you haven't demonstrated exists." The `variables`
map already exists on `MoleculeData`; adding a key is zero new code.

**Promotion path:** If the convention proves error-prone (e.g., workers
forget to set it, downstream can't discover outputs), promote to a `Yields`
link variant. This is a backwards-compatible addition, not a rewrite.

### 5. Signal bus (ADR-015) is not mandatory for v1 (5/6)

File-based mailboxes + typed `MoleculeLink` edges are sufficient for
inter-agent coordination in the propelled and early autonomous regimes.
The signal bus (ADR-015) adds latency improvement
(push-based notification) and cancellation propagation but is a
second-phase enhancement.

**v1 coordination model:**
- `DagPolicy` polls completion by checking molecule status in the store.
- Workers signal completion via `cs complete` (writes status to store).
- The runtime loop detects completions and dispatches newly-ready molecules.
- No push mechanism is required because the runtime loop already exists.

**When signal bus becomes necessary:**
- Cross-convoy coordination (molecule in convoy A triggers molecule in convoy B).
- Sub-second reaction time requirements.
- Cancellation cascades (abort downstream when upstream collapses).

### 6. Convoy stays flat — it is NOT the fleet orchestration primitive (5/6)

A convoy is a flat grouping of molecules for batch observation and lifecycle
management. It is NOT a DAG, NOT hierarchical, and NOT the right abstraction
for fleet execution.

**Fleet orchestration uses:**
- `MoleculeLink::Blocks`/`BlockedBy` — DAG edges (ordering constraints).
- `DagPolicy` + `compile_plan` — scheduling (what's ready to run).
- `Executor` — dispatch (start the work).
- Convoy — optional grouping for human observation ("show me everything
  related to this goal").

**Constraint:** Do not add DAG semantics to convoys. Do not make convoy
membership imply execution order. The convoy is a lens, not a scheduler.

## Phasing

| Phase | Regime | Scope | New code |
|-------|--------|-------|----------|
| **1** (current) | Propelled | `cs run` dispatches hand-wired DAGs via `Executor` trait. Human picks workers. | ~70 LOC (done: `Executor` trait + `SubprocessExecutor` + `apply_evolve` dispatch) |
| **2** | Autonomous | `assigned_role` field + `RoleAwareExecutor` selects workers by role. Planner = deliberation molecule. | ~150 LOC estimated |
| **3** | Autonomous + coordination | Signal bus for push notifications, cancellation cascades, cross-convoy triggers. | ADR-015 scope |

## Consequences

**Positive:**
- The architecture requires no new domain types for phase 1. The existing
  primitives (`DagPolicy`, `compile_plan`, `BlockedBy`, decay, `Executor`)
  compose into full fleet orchestration.
- The planner-as-deliberation-molecule means any agent with MCP access can
  be a planner. No special infrastructure is needed.
- Phase 2 is a single optional field + a new `Executor` impl — not a
  rewrite. The extension point is already in place.

**Negative:**
- The `variables["output_path"]` convention is stringly typed. Typos or
  missing keys produce runtime failures, not compile-time errors.
- Without the signal bus, the runtime must poll for completions. At small
  scale (5–20 molecules) this is sub-millisecond; at large scale it may
  become a latency concern.

**Neutral:**
- The Cosmopedia fleet (6 agents, 10 GAP features) is explicitly a v2/v3
  showcase, not the v1 demo. The v1 demo is a 3-molecule chain or
  4-molecule diamond (jobs).

## References

- **`delib-20260410-f312` synthesis**
  — six-persona deliberation (architect, von-neumann, jobs, feynman, torvalds,
  knuth). Source evidence for all decisions.
- [ADR-016: Autonomy Regimes and the Resident Runtime](016-autonomy-regimes-and-resident-runtime.md)
  — two-layer model, three regimes, decay-aware re-planning.
- [ADR-022: Native DAG Scheduler](022-native-dag-scheduler.md) — `Plan<Id>`
  reducer, `cosmon-graph` as native scheduler.
- [ADR-024: Worker Output Yield Protocol](024-worker-output-yield-protocol.md)
  — worker output verification, yield semantics.
- ADR-015: Signal Bus — deferred to phase 3.
- `crates/cosmon-runtime/src/lib.rs:193–206` — `Executor` trait definition.
- `crates/cosmon-runtime/src/lib.rs:562–575` — `apply_evolve` with real dispatch.
