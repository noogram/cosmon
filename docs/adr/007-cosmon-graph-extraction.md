# ADR-COS-007: cosmon-graph Crate Extraction

## Status
Accepted (Implemented)

## Context

Cosmon and OxyMake both need directed acyclic graph (DAG) primitives:
topological sort, cycle detection, ready-frontier computation. Today these
are implemented independently:

| Location | What | How |
|----------|------|-----|
| `cosmon-core/src/toposort.rs` | `toposort()`, `ready_frontier()` on `(StepId, StepId)` edge slices | Custom Kahn's algorithm |
| `cosmon-core/src/formula.rs` | Private `topological_sort()` on `RawStep` slices | Duplicate Kahn's algorithm |
| `ox-core/src/dag.rs` | `RuleGraph` with topo order, cycle detection, upstream/downstream | petgraph wrapper |
| `ox-core/src/job_graph.rs` | `JobGraph` with topo order, ready jobs, skip marking | petgraph wrapper |

The bead description identifies **three consumers that need shared toposort**.
After investigation, they are:

1. **Formula step ordering** (`formula.rs:300`) — topological sort during
   parsing to validate and order steps.
2. **Standalone toposort** (`toposort.rs:62`) — public API for runtime
   ready-frontier computation used by the evolve/dispatch path.
3. **Convoy coordination** (`convoy.rs`) — currently a flat ordered Vec, but
   upcoming convoy-level dependency graphs will need DAG primitives for
   inter-molecule ordering.

The immediate duplication is between consumers 1 and 2: both implement Kahn's
algorithm independently. Consumer 3 is the trigger for extraction — when convoy
gains dependency edges, copy-pasting Kahn's a third time is the wrong answer.

## Analysis

### What cosmon-core has today

The `toposort` module provides two functions operating on `(StepId, StepId)`
edge slices:

```rust
pub fn toposort(edges: &[(StepId, StepId)]) -> Result<Vec<StepId>, ToposortError>;
pub fn ready_frontier(edges: &[(StepId, StepId)], completed: &HashSet<StepId>) -> Vec<StepId>;
```

Design properties (per ADR-COS-001):
- No traits, no structs — plain functions on data
- Deterministic tie-breaking (lexicographic `StepId` order)
- Zero dependencies beyond `std` and `StepId`

The private `formula.rs::topological_sort()` is structurally identical but
operates on `&str` step IDs instead of `StepId` newtypes.

### What ox-core has

OxyMake's `ox-core` provides rich graph types backed by petgraph:

- `RuleGraph` — bipartite DAG of rules and patterns (logical level)
- `JobGraph` — bipartite DAG of jobs and outputs (physical level)
- Both expose: `topological_order()`, `is_acyclic()`, `upstream()`,
  `downstream()`, `find_cycle()`

ox-core uses `petgraph::algo::toposort` directly. The graph structs own the
topology and provide domain-specific query methods (e.g., `ready_jobs()`,
`producers_of()`).

### The sharing boundary question

There are three possible extraction scopes:

**Option A: Generic toposort functions (minimal)**
Extract only Kahn's algorithm as `cosmon-graph`, generic over node ID type.
Both cosmon-core and formula.rs use this. OxyMake stays on petgraph.

```rust
// cosmon-graph/src/lib.rs
pub fn toposort<N: Ord + Clone + Hash>(edges: &[(N, N)]) -> Result<Vec<N>, CycleError<N>>;
pub fn ready_frontier<N: Ord + Clone + Hash>(edges: &[(N, N)], completed: &HashSet<N>) -> Vec<N>;
```

**Option B: Edge-list graph type (moderate)**
A lightweight `EdgeGraph<N>` struct that owns the edge list and provides
topo-sort, ready-frontier, cycle detection, upstream/downstream queries.
Still no petgraph dependency. Cosmon and OxyMake can both use it for simple
cases; OxyMake keeps petgraph for its richer bipartite structure.

```rust
// cosmon-graph/src/lib.rs
pub struct EdgeGraph<N> { edges: Vec<(N, N)> }
impl<N: Ord + Clone + Hash> EdgeGraph<N> {
    pub fn toposort(&self) -> Result<Vec<N>, CycleError<N>>;
    pub fn ready_frontier(&self, completed: &HashSet<N>) -> Vec<N>;
    pub fn upstream(&self, node: &N) -> Vec<N>;
    pub fn downstream(&self, node: &N) -> Vec<N>;
    pub fn is_acyclic(&self) -> bool;
}
```

**Option C: Shared petgraph wrapper (maximal)**
Unify cosmon and OxyMake on a shared petgraph-based graph crate. Both use
the same `DagGraph<N, E>` with domain-specific extensions.

### Recommendation: Option A now, Option B when convoy lands

**Start with Option A.** Rationale:

1. **ADR-COS-001 compliance.** Cosmon's "no traits, no structs" philosophy
   for graph primitives is intentional. The functions-on-slices approach is
   the simplest thing that works and aligns with the codebase ethos.

2. **Minimal extraction surface.** Two generic functions, one error type,
   zero external dependencies. The crate is trivially auditable and has no
   upgrade risk.

3. **Eliminates the duplication.** `formula.rs` can replace its private
   `topological_sort()` with `cosmon_graph::toposort()`. `toposort.rs` in
   cosmon-core becomes a thin re-export layer that binds the generic to
   `StepId`.

4. **OxyMake stays independent.** ox-core's petgraph-backed types serve a
   different abstraction level (bipartite graphs with typed edges). Forcing
   unification would be premature — the domain semantics diverge.

5. **Option B is a natural next step.** When convoy gains dependency edges,
   the `EdgeGraph<N>` struct emerges organically from usage. Extracting it
   then is low-cost because Option A already established the crate boundary.

**Option C is premature.** OxyMake's graph needs (bipartite structure, typed
edges, execution state annotation) are fundamentally different from cosmon's
(simple node-to-node DAGs). A shared petgraph wrapper would either be too
generic to be useful or too specific to serve both.

## Decision

### Extraction trigger

Extract `cosmon-graph` when the **third consumer** (convoy inter-molecule
dependencies) is implemented. Until then, the duplication between `toposort.rs`
and `formula.rs` is tolerable — it's ~50 lines of Kahn's algorithm, well-tested
in both locations.

**Do NOT extract preemptively.** The crate boundary should be drawn by real
usage, not by anticipation.

### Extraction plan (when triggered)

1. Create `crates/cosmon-graph/` with:
   - `Cargo.toml`: no dependencies beyond `std`, `thiserror`
   - Generic `toposort<N>()` and `ready_frontier<N>()` functions
   - `CycleError<N>` error type

2. Update `cosmon-core`:
   - `toposort.rs` → re-exports `cosmon_graph::toposort` bound to `StepId`
   - `formula.rs` → replaces private `topological_sort()` with
     `cosmon_graph::toposort` (mapping `&str` → owned keys)

3. Add `cosmon-graph` as workspace dependency, no version coupling with
   OxyMake (separate publish cadence).

4. **Do not** add `cosmon-graph` as a dependency of `ox-core`. OxyMake's
   petgraph usage is correct for its domain. Cross-pollination happens at
   the design level, not the dependency level.

### API sketch

```rust
// crates/cosmon-graph/src/lib.rs
#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Lightweight DAG primitives: topological sort and ready-frontier.
//!
//! Generic over node type. No external dependencies.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// A cycle was detected during topological sort.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cycle detected involving node \"{0:?}\"")]
pub struct CycleError<N>(pub N);

/// Topologically sort nodes given dependency edges.
///
/// Each edge `(a, b)` means "a must complete before b can start."
/// Deterministic: ties broken by `Ord` ordering of `N`.
pub fn toposort<N>(edges: &[(N, N)]) -> Result<Vec<N>, CycleError<N>>
where
    N: Eq + Hash + Ord + Clone,
{ /* Kahn's algorithm */ }

/// Compute nodes ready to execute given completed set.
///
/// A node is "ready" when all its dependencies are in `completed`
/// and it is not itself completed.
pub fn ready_frontier<N>(edges: &[(N, N)], completed: &HashSet<N>) -> Vec<N>
where
    N: Eq + Hash + Ord + Clone,
{ /* dependency check */ }
```

### What this means for convoy

When convoy gains dependency edges (e.g., molecule A must complete before
molecule B starts), the usage looks like:

```rust
use cosmon_graph::ready_frontier;

let edges: Vec<(MoleculeId, MoleculeId)> = convoy.dependency_edges();
let completed: HashSet<MoleculeId> = convoy.completed_molecules();
let dispatchable = ready_frontier(&edges, &completed);
```

This is the same pattern as `toposort.rs::ready_frontier` but parameterized
over `MoleculeId` instead of `StepId`.

## Consequences

### Positive
- Eliminates duplicated Kahn's algorithm (formula.rs, toposort.rs)
- Establishes clean crate boundary for graph primitives
- Convoy gets DAG support without reinventing topological sort
- Zero new external dependencies

### Negative
- One more crate in the workspace (minimal overhead)
- cosmon-core gains a workspace dependency (cosmon-graph)

### Neutral
- OxyMake continues using petgraph independently
- No cross-repo dependency coupling
- Option B (EdgeGraph struct) remains available as a future evolution

## References

- `crates/cosmon-core/src/toposort.rs` — current standalone toposort
- `crates/cosmon-core/src/formula.rs:300` — duplicate Kahn's in formula parsing
- `crates/cosmon-core/src/convoy.rs` — future consumer (flat Vec today)
- `crates/cosmon-core/src/evolve.rs:156` — dependency validation in evolve
- OxyMake `ox-core/src/dag.rs` — petgraph-based RuleGraph (different abstraction)
- OxyMake `ox-core/src/job_graph.rs` — petgraph-based JobGraph
- ADR-COS-001 (referenced in toposort.rs) — "no traits, no structs" principle
