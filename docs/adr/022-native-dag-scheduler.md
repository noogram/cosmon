# ADR-022: Native DAG Scheduler in `cosmon-graph`

## Status
Proposed (2026-04-09)

**Supersedes** Phase 2 of an internal idea note
(the `ox-sched` extraction).
**Amends** [ADR-016](016-autonomy-regimes-and-resident-runtime.md) §4
(scheduler reuse).

## Context

[ADR-016](016-autonomy-regimes-and-resident-runtime.md) introduced the
Resident Runtime and declared that its `DagPolicy` would not rewrite a
scheduler — it would wrap a mature one extracted from OxyMake
(`ox-sched`, §4). That section in turn adopted the recommendation of
`idea-20260408-589f`
Phase 2, which proposed factoring OxyMake's topological-dispatch core
into a small generic crate that both projects would depend on.

A five-persona panel (architect, tolnay, knuth, feynman, torvalds)
deliberated this question on 2026-04-09 as
`delib-20260409-c422`.
The panel was asked: Option A (extract `ox-sched`), Option B (depend on
`ox-core` directly), or Option C (grow `cosmon-graph` natively).

The vote landed **4–1 in favor of C**, with tolnay the lone dissenter
(a drastically smaller A than the feasibility study sketched). Option
B received zero support.

The convergent findings, load-bearing for this decision:

1. **OxyMake's scheduler is tokio-driven.** It is built around
   `Semaphore`, `JoinSet`, `Notify`, `DiskWriterHandle`, and assumes
   one `JobGraph` is consumed by one `run_scheduler(...).await`. It is
   an async executor, not a pure reducer. Wrapping it N times inside
   the Resident Runtime gives N mini-executors stapled together — the
   exact shape [ADR-016 §6](016-autonomy-regimes-and-resident-runtime.md)
   (multi-context strategy) was written to avoid.

2. **OxyMake's `JobGraph::build` is a batch constructor.** Verified by
   reading `oxymake/crates/ox-core/src/job_graph.rs`: no `add_job`,
   `insert_node`, or `extend_with` API exists; optimization passes
   take ownership of a frozen graph. This is correct for build systems
   where targets are known at plan time. Cosmon is the opposite
   regime — children appear during execution via decay
   ([ADR-016 §5](016-autonomy-regimes-and-resident-runtime.md)). Under
   Option A, every decay event would force a rebuild-from-scratch of
   the `JobGraph` and re-run of every optimization pass.

3. **`SchedulableJob` as sketched is leaky.** The trait sketched in
   the feasibility study must smuggle in `run_reasons`, `force_rerun`,
   and `CachePruningPass` invariants from `ox-core` to stay meaningful
   for OxyMake consumers. `critical_path_jobs` is used inside OxyMake
   for in-memory materialization eligibility — a concern cosmon does
   not share. The trait is "a window onto 3% of the module"
   (knuth, feynman independently) and cannot abstract over the I6
   invariant (progress under mid-run mutation).

4. **The actual cosmon scheduler workload is small.** 5–20 molecules
   per convoy, N small contexts. At that scale Kahn's O(V+E) is
   microseconds; there is no performance argument for any option.
   Re-running toposort after a decay event is sub-millisecond.

5. **`cosmon-graph` already does most of the job.** ~169 LOC of
   working code (toposort + ready-frontier, generic, deterministic via
   `Ord` tie-break). The delta to a `DagPolicy`-ready library is
   small — estimates converge on 130–200 LOC of pure algorithm.

6. **OxyMake is in a hot perf-refactor regime.** Recent commits
   include `O(1) promote_downstream — 27x speedup` and lock-acquisition
   consolidation in the dispatch loop. This is precisely the
   signature-churn window where a shared trait hurts the most. Option A
   pays maximum coupling cost in the year OxyMake is least stable
   (torvalds, with a specific plausible PR — oxymake#187 circa
   2026-05-14 — refactoring `dependencies()` to return `impl Iterator`).

The panel explicitly framed the choice as binding to
[ADR-016 §5](016-autonomy-regimes-and-resident-runtime.md) (decay-aware
re-planning). Either this is a real requirement, in which case only
Option C survives, or it is not, in which case ADR-016 itself needs
revision. The panel affirmed §5.

## Decision

### 1. Grow `cosmon-graph` natively

The Resident Runtime's `DagPolicy` will use a native `cosmon-graph`
implementation. No crate is extracted from OxyMake; no dependency is
added on `ox-core`, `ox-sched`, or any future derivative.

Three categories of pure-algorithm primitive are added to
`cosmon-graph`:

- **`Plan<Id>` reducer** — an owned, mutable execution plan with
  `mark_done`, `mark_running`, `splice` (insert subgraph), and
  `is_drained`. No event loop, no tokio, no async. Multi-context
  composition is free because each context owns its own `Plan`.
- **`critical_path`** — a ≤ 50 LOC dynamic-programming pass over a
  toposort result, returning the longest chain by edge weight
  (default: unit weight; pluggable for future energy-budget weighting).
- **Graph-mutation primitives** — `prune_completed(completed_set)`
  and `insert_subgraph(parent, subgraph)`, each ≤ 50 LOC with stated
  invariants and a proptest for I6 (progress under random mid-run
  mutation).

`cosmon-graph` remains **zero-domain**, generic over `Id: Eq + Hash +
Ord + Clone`. Domain-aware compilation
(`compile_convoy(store, convoy_id) -> Plan<MoleculeId>`) lives in
`cosmon-runtime` / `cosmon-core`, not in `cosmon-graph`.

### 2. Three flip-conditions to Option A

The decision is reversible. If **all three** of the following become
true, the panel's recommendation is to reopen the question and
re-deliberate Option A (in its smaller, 8-public-item form):

1. **OxyMake independently refactors its scheduler into a pure
   reducer** with no tokio, no `Executor`, and no `DiskWriter`. As of
   2026-04-09 this is not on the OxyMake roadmap. If it lands, the
   dynamic-DAG obstruction (finding 2) disappears.

2. **A third independent consumer of generic DAG scheduling appears.**
   The rule-of-three applies: one use site is code, two is coincidence,
   three is a library. Cosmon + OxyMake is two.

3. **Cosmon's native ready-frontier is benchmarked as a measurable
   bottleneck** at `N > 1000` nodes per execution context. At today's
   scale (5–20 nodes per convoy) Kahn's is microseconds; a bottleneck
   here is a hypothetical future regime, not a current constraint.

All three must hold simultaneously. Any single condition (for
example, a second perf-refactor in OxyMake without a third consumer)
is insufficient.

### 3. Load-bearing invariants from ADR-016

This decision is bound to three sections of
[ADR-016](016-autonomy-regimes-and-resident-runtime.md):

- **§1 (Two layers, not four levels)** — The Resident Runtime is a
  single event loop with pluggable policies. A `DagPolicy` that
  imports a tokio-driven scheduler from OxyMake would introduce a
  second event loop inside the first. The two-layer model requires
  that the policy layer be passive (no clock of its own). A pure
  `Plan<Id>` reducer satisfies this; `run_scheduler(...).await` does
  not.

- **§5 (Dynamic DAG: decay-aware re-planning)** — "On worker
  completion, check whether the molecule emitted any `decayed_to`
  links. If yes, load the new children, compile them into the
  existing plan, recompute ready-frontier, and continue." This is the
  I6 invariant. OxyMake's `JobGraph::build` is a batch constructor
  with no incremental-mutation API; `Plan<Id>::splice` is a ≤ 40 LOC
  method on a struct cosmon owns.

- **§6 (Multi-graph / multi-dag strategy)** — "N contexts in one
  supervisor." A pure reducer composes trivially because there is no
  shared mutable state between contexts. `run_scheduler(...).await`
  cannot offer this property without its own tokio refactor.

If any of §1, §5, or §6 is weakened in a successor ADR, this
decision's premises must be re-examined.

### 4. What ships, and in what order

Phase 1 of `idea-20260408-589f` (`MoleculeLink::Blocks`) is
**unchanged**. It is independently valuable, is a prerequisite for
any scheduler shape, and ships first.

Phases 2 and 3 of that idea are **superseded** by this ADR:

- **(was Phase 2 — extract `ox-sched`)** → `Plan<Id>` reducer +
  `critical_path` + `prune_completed` + `insert_subgraph` added to
  `cosmon-graph`. ~130–200 LOC of pure algorithm + proptests.
- **(was Phase 3 — `cosmon-convoy` bridge)** → `compile_convoy(store,
  convoy_id) -> Plan<MoleculeId>` in `cosmon-runtime`, consuming
  `cosmon-graph` primitives directly.

The six child units nucleated from `delib-20260409-c422` track the
concrete work: this ADR, the supersession note on the idea doc, the
successor note on ADR-016 §4, the `Plan<Id>` reducer, `critical_path`,
and the mutation primitives.

## Consequences

**Positive:**

- `cosmon-graph` stays zero-I/O and zero-domain, consistent with
  [ADR-007](007-cosmon-graph-extraction.md)'s extraction rationale.
- The Resident Runtime's `DagPolicy` becomes a pure function of
  `(Plan<Id>, event) -> Plan<Id>`. Multi-context composition is free.
- Decay-aware re-planning ([ADR-016 §5](016-autonomy-regimes-and-resident-runtime.md))
  is a one-method addition (`Plan::splice`) rather than a
  rebuild-from-scratch per decay event.
- Cosmon and OxyMake remain decoupled in the exact year OxyMake is
  least stable. No cross-repo semver coordination is required.
- The I6 invariant (progress under mid-run mutation) can be tested
  with a proptest over random `mark_done` / `splice` sequences — the
  strongest correctness witness available.

**Negative:**

- The `cosmon-graph` crate grows by 130–200 LOC of pure algorithm
  plus tests. This is additional surface area for cosmon to own.
- tolnay's observation stands: "the algorithm is the same algorithm."
  If a third consumer ever materializes, the shared kernel inside
  `cosmon-graph` must be promoted to its own crate — a refactor, not
  a rewrite, but real work.
- The feasibility study in `idea-20260408-589f` is partially
  superseded and must be read with its Phase 2/3 sections marked.
  Future readers may be confused if the idea doc is consulted
  without this ADR.

**Neutral:**

- The feasibility study's Phase 1 (`MoleculeLink::Blocks`) is
  unchanged and remains a hard prerequisite for any of Options A/B/C.
  Its work is not affected by this decision.
- tolnay's "eight-item contract" (1 trait, 1 struct, 3 accessors, 1
  free function, 1 error enum, 2 variants) is compatible with C: the
  same eight shapes can live inside `cosmon-graph` as the *internal*
  contract of the reducer, and can be promoted to a shared crate
  later if flip-condition #2 holds.

## References

- **`delib-20260409-c422` synthesis**
  — five-persona deliberation that produced this decision (architect,
  tolnay, knuth, feynman, torvalds). 4–1 in favor of Option C.
- [ADR-007: `cosmon-graph` extraction](007-cosmon-graph-extraction.md)
  — precedent for isolating generic graph algorithms in a dedicated
  crate.
- [ADR-016: Autonomy Regimes and the Resident Runtime](016-autonomy-regimes-and-resident-runtime.md),
  §1 (two-layer model), §5 (decay-aware re-planning), §6 (multi-context
  strategy) — the load-bearing constraints this decision is bound to.
- `idea-20260408-589f` — OxyMake Scheduler Integration
  — Phase 1 remains live; Phase 2 (`ox-sched` extraction) and Phase 3
  (`cosmon-convoy` bridge via `ox-sched`) are superseded by this ADR.
- `oxymake/crates/ox-core/src/job_graph.rs` — batch-constructor
  evidence for finding 2 (no incremental-mutation API).
- `oxymake/crates/ox-core/src/scheduler.rs:139–160` and
  `oxymake/crates/ox-core/src/prune.rs:83` — leakage evidence for
  finding 3 (`run_reasons`, `force_rerun`, in-place `JobGraph`
  mutation).
