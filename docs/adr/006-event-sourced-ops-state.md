# ADR-GOV-006: Event-Sourced Operational State Model

**Status:** Accepted
**Date:** 2026-04-04
**Deciders:** Cosmon core team

## Context

Cosmon's operational state (workers, molecules, assignments) was previously
managed through direct mutation via the `StateStore` trait — a CRUD-style
interface that overwrites the previous state on every write. This approach has
three problems:

1. **No audit trail.** When a worker transitions from Active to Stale, we lose
   the record of when and why. The patrol can only see current state, not the
   history that led to it.

2. **No replay/debugging.** When the fleet enters an unexpected configuration,
   there is no way to reconstruct how it got there. You can inspect the current
   snapshot, but the causal chain is lost.

3. **Mutation hazards.** Multiple agents writing to the same state store can
   overwrite each other's changes. The `load → modify → save` cycle is a
   classic lost-update pattern.

## Decision

Adopt an **event-sourced model** for operational state. Agent actions are
**commands** that produce **events** after validation. Current state is a
**projection** derived by folding the event log. No direct state mutation.

### Architecture

```
Command → decide(state) → Event → persist(store) → apply(state)
```

- **Commands** (`OpsCommand`): typed requests from agents (spawn worker,
  assign molecule, transition status). Validated against current state before
  producing events.

- **Events** (`OpsEvent`): immutable facts that happened. Each carries a
  timestamp. Once persisted, never modified or deleted.

- **Envelopes** (`OpsEnvelope`): events wrapped with a monotonically
  increasing sequence number for total ordering.

- **Projection** (`OpsState`): derived state built by folding events.
  Pure function: same events in same order produce the same state.
  Contains `WorkerView` and `MoleculeView` projections.

- **Event Store** (`OpsEventStore`): hexagonal port for append-only
  persistence. Implementations provide the storage backend.

### Key Properties

- **Deterministic replay:** `OpsState::replay(events)` reproduces any
  historical state from an event prefix.
- **Audit trail:** the event log is the complete history of every state change.
- **Testability:** projections are pure functions, testable without I/O.
- **Incremental catch-up:** `read_since(sequence)` enables efficient polling.

### What This Does NOT Replace

The existing `Event` enum in `cosmon_core::event` and the `StateStore` trait in
`cosmon_state` remain. `Event` handles observability/logging (JSONL output).
`StateStore` handles snapshot persistence. The new `ops` module provides the
event-sourced layer for operational state specifically.

Over time, `StateStore` may be phased out in favor of materializing projections
from the ops event log, but that migration is out of scope for this ADR.

## Consequences

### Positive

- Full audit trail for debugging fleet anomalies
- Replay enables time-travel debugging and state reconstruction
- Commands provide a single point for validation logic
- Projection pattern keeps read models decoupled from write model

### Negative

- Event log grows indefinitely (mitigated by snapshots + truncation later)
- Two state models coexist temporarily (StateStore and OpsEventStore)
- Learning curve for contributors unfamiliar with event sourcing

### Risks

- Event schema evolution: adding fields to events requires forward-compatible
  serde (already standard practice in this codebase)

## Implementation

- Module: `cosmon_core::ops`
- Types: `OpsCommand`, `OpsEvent`, `OpsEnvelope`, `OpsState`, `WorkerView`,
  `MoleculeView`, `OpsEventStore`
- Zero I/O in core (consistent with `cosmon-core` conventions)
- All types are `Serialize + Deserialize` for persistence
