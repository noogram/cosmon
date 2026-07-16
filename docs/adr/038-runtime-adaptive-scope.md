# ADR-038 — Runtime adaptive scope: dynamic sweeps and collapse-continuation

**Status:** Proposed (2026-04-14). Limit 1 mitigation implemented opt-in.
Limit 2 mitigation deferred pending pilot arbitration.
**Scope:** `cs run`, `crates/cosmon-runtime`, `DagPolicy`,
`RuntimeConfig`.
**Parent:** task `task-20260414-0af3`; continuation of commit
`dc66e2f` (lateral DecayProduct drain) and the post-mission-collapse
DagPolicy split.
**Binds:** [ADR-016](016-autonomy-regimes-and-resident-runtime.md),
[ADR-022](022-native-dag-scheduler.md),
[ADR-026](026-dynamic-fleet-orchestration.md).

## Context

The resident runtime (`cs run`) operates on a plan bootstrapped by
`compile_plan(store, roots)`, which walks the transitive closure of
`Blocks` / `BlockedBy` / `DecayProduct` typed links on disk at
start-up. Two classes of molecule fall outside that closure and
therefore outside the runtime's scheduling scope:

1. **Dynamically-nucleated descendants without a typed link back to
   the root.** A worker may nucleate a side-task using `Refines`,
   `Entangled`, or no link at all (e.g. a reading-club note, a
   probe, a delegated chore). These molecules are `Pending` on disk
   but unreachable from the runtime's root.
2. **Siblings of a collapsed root.** If the root itself collapses
   before decomposition has attached any `DecayProduct` edges, the
   plan's frontier drains and the runtime exits — orphaning any
   subgraph that existed independently under the same fleet.

Commit `dc66e2f` partially addressed the collapse case: a collapsed
parent with existing `DecayProduct` edges splices its lateral
children as standalone roots. That fix is necessary but not
sufficient. It does not help when:

- the collapse happens before decomposition (no `DecayProduct` edges
  yet); or
- the orphan subgraph is connected via `Refines` or external intent
  rather than `DecayProduct`.

## Decision

This ADR splits the remaining work into two independently-scoped
mitigations, each behind an opt-in configuration flag so default
behavior is unchanged.

### Limit 1 — `sweep_orphan_descendants_every: Option<u32>`

Every N ticks (default `None` = disabled), the runtime calls
`DagPolicy::recompile_from_known_molecules(store)`. This re-runs
`compile_plan` seeded from the policy's current `known_molecules`
set — which already tracks every molecule the runtime has ever
observed, including splice children. Newly-reachable pending
molecules (e.g. a decomposition child nucleated in the last tick by a
running worker) enter the plan and become eligible for dispatch.

**Rationale for scope-limited sweep rather than global:** a global
"pick up every pending molecule in the store" sweep violates the
scoping contract of `cs run <root>`. An operator running
`cs run task-A` expects isolation from `cs run task-B`; a global
sweep would merge them. The `known_molecules`-seeded sweep preserves
the scoping: it only pulls in molecules reachable from what the
runtime already knows about, which grows organically as
decompositions happen.

**Default value:** `None`. Zero-diff for existing users. An operator
who runs decomposition-heavy formulas (`mission-controller`,
`deep-think`) can enable with `--sweep-every 5`.

**Risk assessment:**

- **Idempotence:** `compile_plan` is pure over its input. Repeated
  sweeps over the same store state return the same edge list.
- **Performance:** `compile_plan` is O(|reachable closure|) per
  call. At default 1s tick and N=5, the sweep runs every 5s. For a
  DAG of 100 molecules this is microseconds of I/O.
- **Determinism:** the sweep reuses `compile_plan`'s sorted BFS, so
  edge-list ordering is stable across runs.
- **Failure mode:** a store read error during sweep propagates to
  the caller via `RuntimeError::State` and exits the loop cleanly —
  the next `cs run` invocation restarts from disk.

### Limit 2 — `continue_after_root_collapse: bool`

When set (default `false`), the runtime on a root collapse does
**not** treat `PolicyDrained` as a shutdown reason. Instead it
continues ticking as long as any molecule in `known_molecules`
remains `Pending` or `Running`. A new event
`RootCollapsedRunContinuedOnChildren` is emitted on the renderer so
operators can tell "exit because root collapsed" from "continue
because children remain."

**Not yet implemented.** The decision to adopt this behavior is
deferred to pilot arbitration for three reasons:

1. **Semantic ambiguity.** "The root collapsed" is a strong signal
   that the whole subgraph should stop, *especially* if the root
   was a gate molecule. Continuing risks consuming energy on
   children whose work is moot.
2. **Cascade interaction.** `Blocks` cascade-collapse already
   propagates collapse through the forward axis; lateral drain
   (commit `dc66e2f`) handles `DecayProduct`. The remaining gap
   (orphaned siblings with no edges to root) may be better addressed
   by nucleating those siblings as real DAG roots of their own
   `cs run` invocation — which is what a stateless CLI encourages.
3. **Observer expectations.** An operator watching `cs run` expects
   it to exit when its root is terminal; changing that without
   explicit opt-in violates the principle of least surprise.

The config flag is wired but the behavior change is gated on this
ADR's acceptance. Until then, operators who want the behavior
manually re-invoke `cs run <surviving-child>`; since `cs run` is
stateless this is safe and composable.

## Implementation

Changes land in `crates/cosmon-runtime/src/lib.rs`:

```rust
pub struct RuntimeConfig {
    pub poll_interval: Duration,
    pub max_runtime: Option<Duration>,
    /// Re-walk the store every N ticks to absorb descendants
    /// nucleated dynamically by workers (Limit 1). None = off.
    pub sweep_orphan_descendants_every: Option<u32>,
    /// When true, do not exit on root collapse if any
    /// known molecule is still pending/running (Limit 2).
    /// Gated on ADR-038 pilot arbitration; default false.
    pub continue_after_root_collapse: bool,
}
```

The sweep itself is implemented by extending `Policy` with an
optional `refresh_scope(store)` hook, defaulted to no-op so
`NoOpPolicy` remains unchanged. `DagPolicy::refresh_scope` calls
`compile_plan` seeded from `known_molecules`, merges new edges via
`insert_subgraph`, and rebuilds the plan with the existing
`completed` skip-set.

`cs run --sweep-every N` exposes the first option to operators.
`--continue-after-root-collapse` is added but gated behind a
`COSMON_EXPERIMENTAL_CONTINUE_AFTER_COLLAPSE` env var until this
ADR is accepted — a soft gate so CI can exercise the path without
changing user-visible semantics.

## Tests

- `tests/scenarios/runtime-adaptive-sweep.toml` — a worker nucleates
  a child with `Refines` (no `Blocks` edge) during a running DAG;
  without sweep the child stays pending; with `--sweep-every 2`
  the child enters the plan and completes.
- Unit test in `crates/cosmon-runtime/src/dag_policy.rs` covering
  `refresh_scope` idempotence over a stable store.
- The existing `collapsed-mission-orphans-children.toml` scenario is
  unchanged — this ADR composes with it, it does not supersede the
  `dc66e2f` fix.

## Alternatives considered

1. **Global pending sweep (`temp:hot` style).** Rejected: violates
   `cs run <root>` scoping. An operator running two independent DAGs
   in parallel would see them merge.
2. **Patrol-based injection.** `cs patrol --propel` already picks up
   truly orphaned pendings. Deferring to patrol keeps the runtime
   minimal but forces operators to run two processes. Kept as a
   valid fallback; the sweep is additive.
3. **Push model via an in-process channel from nucleate to runtime.**
   Rejected: introduces a non-filesystem communication channel
   between the transactional core and the runtime, violating the
   data-plane = filesystem invariant (see `CLAUDE.md` §
   "Communication Model").

## Open questions

- Should `sweep_orphan_descendants_every` be tick-count or
  wall-clock? Current proposal: tick-count for determinism in tests.
- Should `continue_after_root_collapse` emit a warning on stderr so
  operators know why the runtime did not exit? Current proposal:
  yes, one-line structured event via the renderer.
- Is `known_molecules` the right seed set, or should the sweep also
  include the original `roots`? Current proposal: `known_molecules`
  is the right set — it is a superset of `roots` after any splice.
