# ADR-013: Particle Convergence -- Unified Workspace and Project-Local Nervous System

## Status
Proposed

## Context

### The real-world failure

Cosmon was tested on an external project (Jordan's audio plugins) -- the first
usage outside the Noogram ecosystem. The results revealed five interconnected
failures:

1. **Agents misuse cosmon tools.** Without understanding the workflow (nucleate
   before evolve, molecules belong to formulas), agents called tools in the wrong
   order and produced corrupted state. The problem is not documentation -- it is
   that the tools expose a flat API over a stateful protocol.

2. **Global state, not project-local.** Molecules from unrelated projects were
   visible in the same global store. This has been partially addressed by the
   `.cosmon/` directory and walk-up discovery (see `cosmon-filestore/src/resolve.rs`),
   but the principle is not yet fully applied to the nervous system.

3. **Orphaned molecules.** Molecules float free, unassociated with any fleet.
   There is no way to ask "show me everything in fleet X" because molecules
   do not know their fleet. The `Fleet` struct tracks agents and workers, but
   molecules live in a separate directory with no structural link to their fleet.

4. **Invisible to non-participants.** A developer who opens the repository and
   does not use cosmon sees nothing. No `STATUS.md`, no `ISSUES.md`, no standard
   files. The project's state is locked inside JSON files under `.cosmon/state/`
   that only cosmon can read. The external surface area is zero.

5. **Three repos, three CI pipelines, three deploy targets.** Cosmon (~15 crates),
   neurion (~5100 lines), and topon (~2200 lines) are independent repos that share
   a common consumer: the agent orchestration stack. Cross-repo changes require
   coordinated releases. Neurion and topon are small enough to be workspace crates,
   and keeping them separate adds deployment friction without architectural benefit.

### The deeper pattern

These five failures share a root cause: **Cosmon manages internal state but has
no model for how that state becomes legible at the boundary.**

In Wheeler's participatory framework (THESIS.md, "The Informational Foundation"),
reality is constituted at the measurement boundary -- the surface where an observer
interacts with the system. Cosmon has rich internal state (molecules, fleets,
energy budgets, state machines) but no measurement boundary. External observers --
humans, CI systems, other agents -- cannot see what the system knows.

The Bekenstein bound offers a useful analogy: the information accessible to an
external observer is proportional to the **surface area**, not the volume. A
system can have arbitrarily rich internal state, but what matters to the outside
world is what appears at the surface. Cosmon currently has volume but no surface.

### The neurion connection

Neurion already solves a version of this problem. Its semantic layer models
exactly this relationship: a **referent** is a logical piece of information
(a knowledge domain, a status, an issue list), and its **reaches** are the
physical materializations where that information can be accessed (a Markdown
file, a SQLite row, a GitHub Issue, an MCP tool response). The `Reachable`
trait formalizes this:

```
Referent (what information exists)
  --> Reach 1: STATUS.md (surface: filesystem)
  --> Reach 2: GitHub Issue (surface: API)
  --> Reach 3: .cosmon/state/molecules/... (surface: internal JSON)
```

This is the founding insight, expressed as a principle: **"un bit d'information
peut avoir plusieurs materialisations"** -- one bit of information can have
multiple materializations. This IS neurion's referent/reaches model, and it is
exactly what cosmon needs for surface projection.

### Topon's role

Topon provides structural topology maps: PageRank-ranked symbol graphs, file
outlines, cross-reference navigation. In a converged workspace, topon becomes
the "where is it in the code" reach for neurion referents -- a structural
complement to neurion's semantic map. Its crates (`topon-core`, `topon-mcp`)
already have the right granularity for workspace integration.

### Why now

Three forces converge:

- **External usage failure** demands project-local state and surface observability.
- **Neurion's referent/reaches model** is exactly the abstraction cosmon needs
  for surface projection, but the two repos cannot share types without
  published crate dependencies or path dependencies across repos.
- **Daily reconciliation** is the forcing function that makes neurion
  indispensable. If the nervous system is used only occasionally for
  `how_to_access("email")`, it remains a curiosity. If every `cs evolve`
  triggers a reconciliation pass that updates surfaces via neurion reaches,
  the nervous system becomes load-bearing infrastructure.

## Decision

Seven interconnected decisions, presented in dependency order.

### Decision 1: Merge neurion and topon into the cosmon workspace

Neurion becomes two workspace crates:
- `crates/neurion-core/` -- pure domain types (`Referent`, `Reach`, `Reachable`
  trait, `Intent`, `Category`, `HealthStatus`, scoring functions). Zero I/O.
  Extracted from `neurion/src/domain/`.
- `crates/neurion-mcp/` -- MCP server, SQLite adapter, discovery, HTTP transport.
  Extracted from `neurion/src/adapter/` and `neurion/src/main.rs`.

Topon becomes two workspace crates:
- `crates/topon-core/` -- symbol extraction, graph construction, PageRank.
  Direct move from `topon/crates/topon-core/`.
- `crates/topon-mcp/` -- MCP tool exposure.
  Direct move from `topon/crates/topon-mcp/`.

The `topon-cli` crate merges into cosmon-cli as a `cs topology` subcommand.

The original `neurion` and `topon` repos are archived with a pointer to cosmon.
Their founding theses are imported as `THESIS_NEURION.md` and `THESIS_TOPON.md`
to preserve lineage and rationale.

```
cosmon/crates/
  cosmon-core/           # Domain types, state machines, traits
  cosmon-cli/            # CLI binary (cs)
  cosmon-state/          # State store trait
  cosmon-filestore/      # JSON file backend
  cosmon-transport/      # Transport (tmux)
  cosmon-graph/          # DAG operations
  cosmon-mcp/            # Cosmon MCP server
  cosmon-bridge-claude/  # Claude Code bridge
  claudion/              # Session energy probe
  neurion-core/          # NEW: referents, reaches, Reachable trait
  neurion-mcp/           # NEW: nervous system MCP server
  topon-core/            # NEW: code topology, PageRank
  topon-mcp/             # NEW: topology MCP tools
  cosmon-surface/        # NEW: surface projection engine (Decision 4)
```

**Rationale.** Neurion is ~5100 lines. Topon is ~2200 lines. Neither is large
enough to justify independent release cycles. Cross-crate type sharing (e.g.,
cosmon-core using `Referent` from neurion-core) requires either published
crates or path dependencies. Path dependencies within one workspace are simple.
The merge eliminates this friction.

**Reversibility.** Workspace crates can be extracted to independent repos at
any time by publishing to a registry. The merge is low-risk and high-reversibility.

### Decision 2: Project-local nervous system

Each `.cosmon/` directory gets a `registry.sqlite` -- a project-scoped neurion
instance. This is in addition to the global `~/.cosmon/registry.db`.

```
project-root/
  .cosmon/
    registry.sqlite      # Project-local nervous system
    surfaces.toml        # Surface projection declarations (Decision 4)
    formulas/            # Git-tracked formula templates
    molecules/           # Git-tracked molecule declarations (TOML)
    state/               # Git-ignored runtime state
      fleets/{name}/
        fleet.json
        molecules/{id}/state.json
```

The project-local registry uses the same schema as the global registry
(referents, reaches, inventory tables), scoped to project-relevant entries.
`cs init` creates the `.cosmon/` directory AND initializes the local registry.

Resolution precedence for the nervous system mirrors state dir resolution:

1. Explicit `--registry` flag
2. `COSMON_REGISTRY` environment variable
3. Walk-up discovery: `.cosmon/registry.sqlite`
4. Global fallback: `~/.cosmon/registry.db`

**Schema tracking.** The `registry.sqlite` schema is versioned and migrations
are applied automatically. The schema file (SQL DDL) is checked into the repo
under `crates/neurion-core/migrations/`. The runtime database file is
`.gitignore`d (it is reconstructed from declarations).

### Decision 3: Fleet-scoped molecules

Molecules must belong to a fleet. The current flat layout:

```
.cosmon/state/ops/molecules/{molecule-id}/state.json
```

Becomes:

```
.cosmon/state/fleets/{fleet-name}/molecules/{molecule-id}/state.json
.cosmon/state/fleets/{fleet-name}/fleet.json
```

Consequences for the domain model:

- `MoleculeData` gains `fleet_id: FleetId` (required, not optional).
- `MoleculeDeclaration` gains `fleet: Option<String>`.
- `cs nucleate` requires `--fleet` (or infers from context if single fleet).
- `cs ensemble` groups molecules by fleet.
- `FileStore` path construction includes fleet in the path.
- A fleet must be declared before molecules can be nucleated into it.
- Migration: existing orphaned molecules go to a `default` fleet.

### Decision 4: Surface projection via neurion reaches

A new crate, `cosmon-surface`, implements the surface projection engine.
Surfaces are declared materializations of internal state -- the boundary where
Cosmon's internal representation becomes externally legible.

**Declaration.** `.cosmon/surfaces.toml` declares the projection targets:

```toml
# Each surface is a neurion reach for an internal referent.

[[surface]]
referent = "project.status"
kind = "markdown"
path = "STATUS.md"
template = "default-status"

[[surface]]
referent = "project.issues"
kind = "markdown"
path = "ISSUES.md"
template = "default-issues"

[[surface]]
referent = "project.decisions"
kind = "directory"
path = "docs/adr/"
template = "adr-index"

[[surface]]
referent = "project.issues"
kind = "github-issues"
repo = "owner/repo"
label = "cosmon"
```

**Projection trigger.** After every state transition (evolve, collapse, freeze,
thaw), the surface projection engine:

1. Reads the changed referent (e.g., molecule state changed -> `project.status`
   referent is dirty).
2. Queries `surfaces.toml` for all reaches of that referent.
3. For each reach, renders the template with current state and writes to the
   target (file, API, etc.).

This hooks into the existing `HookEventFilter` system as a built-in
post-transition hook, not an external shell command.

**Idempotency.** Surface projection is idempotent. `cs reconcile` can be run
at any time to force a full re-projection of all surfaces.

**Two reconciliation modes:**

- **Mechanical reconciliation** (default): deterministic projection, pure
  function `state -> surface`. Zero tokens, zero cognition. Fast and
  idempotent. This handles 95% of cases.

- **Cognitive reconciliation** (on-demand or on ambiguity): when mechanical
  reconciliation detects conflicts (human edits on projected files,
  desynchronization from external pushes, schema drift), it spawns an
  **ephemeral worker** whose mission is to:
  1. Analyze the divergence between internal state and surface state.
  2. Attempt auto-resolution (merge, rebase, re-project).
  3. Escalate to a human if the ambiguity cannot be resolved automatically.

  This follows the existing worker pattern (a molecule with steps) but the
  molecule is short-lived and self-collapsing. The escalation chain is:
  mechanical -> cognitive auto-resolve -> human review.

  The term in the thesis vocabulary: "mechanical reconciliation" is transport,
  "cognitive reconciliation" is cognition. The founding principle (Transport /
  Cognition) applies here: try transport first, invoke cognition only when
  transport cannot converge.

### Decision 5: Standard project files as the universal interface

Cosmon projects state INTO standard files. The files are the universal interface
for non-participants:

| File | Content | Audience |
|------|---------|----------|
| `STATUS.md` | Active fleets, running molecules, health | Any developer |
| `ISSUES.md` | Known issues, blockers, tracked work items | Any developer |
| `docs/adr/` | Architecture decisions (index, not content) | Any developer |
| `docs/IDEAS.md` | Captured ideas, future directions | Any developer |

**Directionality.** These files are **derived views**, not sources of truth.
Cosmon is the source of truth. The projection is one-way. If a human edits
`ISSUES.md` directly, the next `cs reconcile` overwrites the edit. To change
the data, change the source (create a molecule), not the view.

**Exception: ADRs.** Architecture Decision Records are human-authored. The
surface projection for ADRs is an **index** (list of ADRs with status), not
the ADR content itself.

**External links.** Some reaches point outside the repo: GitHub Issues, Linear
tickets. The surface projection engine writes to these via adapter traits
(hexagonal pattern).

### Decision 6: Observability as a first-class architectural principle

The THESIS.md mentions observability but does not elevate it to a principle
for non-participants. This ADR proposes adding **surface observability** as a
corollary to the three founding principles:

> **Surface Observability.** A system's value to external observers is
> determined by what appears at its boundary, not by the richness of its
> internal state. Every piece of internal state that matters to a
> non-participant MUST have a declared surface projection. If it cannot
> be observed externally, it does not exist externally.

This is the Bekenstein bound applied to software: the information accessible
to external observers is bounded by the surface area, not the volume.

### Decision 7: The neurion-cosmon feedback loop

Merging neurion into cosmon and using it for daily reconciliation creates a
reinforcing loop:

```
cs evolve (state change)
  --> post-transition hook
    --> surface projection engine (cosmon-surface)
      --> reads surfaces.toml (which referents changed?)
        --> queries local registry (which reaches exist?)
          --> renders and writes to each reach
            --> updates registry health status
```

Every `cs evolve` exercises the nervous system. Every `cs reconcile` validates
the referent/reach mappings. This daily usage:

- **Stress-tests neurion's data model.** Real usage reveals edge cases.
- **Validates the Reachable trait.** Surface projection is the first real
  Cosmon implementation of that trait.
- **Makes neurion load-bearing.** A registry consulted on every state
  transition stays accurate because inaccuracy is immediately painful.

## Consequences

### What becomes easier

- **Cross-repo type sharing.** `cosmon-core` can depend on `neurion-core`
  types directly. No published crate coordination.
- **Single CI pipeline.** One `cargo test --workspace`, one release.
- **Project isolation.** Each project gets its own `.cosmon/` with local state
  and local registry. No cross-project contamination.
- **Fleet-level queries.** "Show me all molecules in fleet X" is a directory
  listing, not a scan-and-filter.
- **External legibility.** A developer who has never used cosmon opens the repo
  and sees `STATUS.md`, `ISSUES.md`, `docs/adr/`.
- **Agent tool correctness.** Fleet-scoped molecules enforce the workflow:
  identify fleet -> nucleate molecule -> evolve.

### What becomes harder

- **Workspace build times.** Adding ~7300 lines increases compile times.
  Mitigated by incremental compilation.
- **Crate count.** 9 crates -> 14. Mitigated by clear boundaries.
- **Surface staleness.** If post-transition hook fails, surfaces drift.
  Mitigated by `cs reconcile --check` (dry-run for CI).
- **Migration of existing state.** `cs migrate` handles one-time conversion.
- **One-way projection discipline.** Projected files include a header:
  `<!-- Generated by cosmon. Do not edit. Source: .cosmon/ -->`.

### What stays the same

- **Neurion's MCP interface.** Same tools (`how_to_access`, `list_services`,
  `get_config_for`, `query_registry`). No agent-visible change.
- **Topon's MCP interface.** Same tools (`map`, `outline`, `symbols`).
- **Cosmon's domain model.** Molecules, formulas, workers, fleets unchanged.
  Fleet-scoping adds a field; it does not change the state machine.
- **The founding thesis.** This ADR adds a corollary (surface observability),
  not a new principle.

## Migration Plan

### Phase 1: Workspace convergence (no behavior change)

**Preservation principle:** Nothing is lost from neurion or topon during
migration. All source code, tests, documentation, THESIS, CLAUDE.md, ADRs,
ideas, git history (via subtree or full import) are preserved. The merge
is additive -- the converged workspace contains a strict superset of what
the three repos contained separately.

1. Import neurion git history into cosmon (subtree merge or equivalent).
2. Create `crates/neurion-core/` with types from `neurion/src/domain/`.
3. Create `crates/neurion-mcp/` with adapter code from `neurion/src/adapter/`.
4. Import topon git history into cosmon.
5. Move `topon/crates/topon-core/` and `topon/crates/topon-mcp/` into cosmon.
6. Import `THESIS_NEURION.md` and `THESIS_TOPON.md` at workspace root.
7. Import neurion and topon CLAUDE.md, README.md, docs/, tests/.
8. Add `cs topology` subcommand from `topon-cli`.
9. Verify: `cargo test --workspace` passes. All MCP tools work.
10. Archive `neurion` and `topon` repos with pointer to cosmon.

### Phase 2: Fleet-scoped molecules

1. Add `fleet_id: FleetId` to `MoleculeData`.
2. Update `FileStore` to `fleets/{name}/molecules/{id}/` layout.
3. Add `cs migrate` for existing molecules -> `default` fleet.
4. Update `cs nucleate` to require `--fleet`.
5. Update `cs ensemble` to group by fleet.

### Phase 3: Project-local registry

1. Add `registry.sqlite` creation to `cs init`.
2. Implement registry resolution (walk-up, env var, global fallback).
3. Populate local registry with project referents on `cs init`.

### Phase 4: Surface projection

1. Create `crates/cosmon-surface/` with `Surface`, `SurfaceKind`, rendering.
2. Define `surfaces.toml` schema and parser.
3. Implement filesystem surface writer (STATUS.md, ISSUES.md).
4. Wire into post-transition hook system.
5. Implement `cs reconcile` and `cs reconcile --check`.
6. Add GitHub Issues surface adapter.

### Phase 5: Thesis update

1. Add surface observability corollary to THESIS.md.
2. Add Bekenstein analogy and surface projection section.
3. Distinguish internal observability (Galilean, for participants) from
   surface observability (for non-participants).

## Prior Art

| System | Surface projection mechanism |
|--------|------------------------------|
| **Kubernetes** | `kubectl get` reads etcd; dashboards are derived views |
| **Terraform** | `terraform plan` projects desired state into readable diff |
| **Git** | Working tree is a surface projection of the object store |
| **dbt** | SQL models projected to materialized views and docs site |

## References

- Wheeler, J.A. (1990). "Information, Physics, Quantum: The Search for Links."
- Bekenstein, J.D. (1973). "Black holes and entropy."
- ADR-003: Multi-Channel Nervous Tissue.
- ADR-011: Content-Identity Principle.
- `neurion/src/domain/reachable.rs`: The `Reachable` trait.
- `cosmon-filestore/src/resolve.rs`: Walk-up discovery pattern.
