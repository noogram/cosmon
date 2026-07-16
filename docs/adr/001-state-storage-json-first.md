# ADR-001: State Storage — JSON First

## Status
Accepted

## Context

Cosmon needs persistent state for agents, workers, molecules, and sessions.
The choice of storage backend affects complexity, debuggability, and
operational burden.

Gas Town's experience with Dolt is instructive: a distributed database
consumed ~17% of infrastructure effort for maintenance, diagnostics, and
recovery. This overhead is disproportionate for an orchestration framework
whose primary value is simplicity.

## Decision

### Trait-first design

Cosmon defines a `StateStore` trait as the storage abstraction. All code
depends on the trait, never on a concrete backend. This is the safety net
(Thesis P15: Morphological Plasticity) — the backend can be swapped without
touching domain logic.

### Phased backend strategy

| Phase | Backend | Trigger | Rationale |
|-------|---------|---------|-----------|
| **1 (now)** | `FileStore` (JSON) | Default | Zero dependency, git-friendly, debuggable |
| **2 (if needed)** | `SqliteStore` | Concurrent writers or >50 molecules | Pattern validated by OxyMake's state.db |
| **3 (probably never)** | `DoltStore` | Distribution across machines | Lesson from GT: high operational cost |

### Why JSON first

Cosmon v0.1 does not have concurrent write contention:
- A single mayor dispatches work
- Molecules are modified one at a time (state transitions are sequential)
- Workers report back through the mayor, not directly to state

JSON files provide:
- **Zero setup** — no database server, no migrations, no connection pool
- **Git-friendly** — state files are diffable, committable, inspectable
- **Debuggable** — `cat state/molecules/mol-abc123.json | jq .`
- **Portable** — works on any OS, any filesystem, no binary dependencies

### When to upgrade

Move to `SqliteStore` when ANY of these become true:
- Multiple processes write state concurrently (e.g., distributed workers)
- State queries need indexing (e.g., "find all molecules in status Active")
- Volume exceeds ~50 active molecules (file-per-entity becomes noisy)

The trait boundary makes this a one-line change in the composition root.

## Consequences

### Positive
- Zero operational burden for v0.1
- State is human-readable and version-controllable
- No infrastructure to maintain, monitor, or recover
- Fastest path to a working system

### Negative
- No concurrent write safety (acceptable for v0.1 single-mayor model)
- No indexed queries (acceptable for small molecule counts)
- File-per-entity creates many small files (acceptable up to ~50)

### Risks
- If concurrent writes are needed sooner than expected, FileStore will
  corrupt. Mitigation: the trait boundary allows a weekend migration to
  SqliteStore.

## References
- Thesis P15: Morphological Plasticity — "the system can change form
  without changing identity"
- OxyMake state.db — validates the SQLite pattern for Phase 2
- Gas Town Dolt — cautionary tale for Phase 3 (17% infra overhead)
