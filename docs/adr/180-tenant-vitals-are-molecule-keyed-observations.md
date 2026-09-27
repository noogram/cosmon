<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# ADR-180 — Tenant vitals are molecule-keyed observations

**Status:** Accepted  
**Date:** 2026-09-25  
**Issue:** noogram/cosmon#78  
**Implementation:** `task-20260925-9c70`

## Context

`GET /v1/workers` publishes the process records the pilot persisted. That is
the correct contract for a worker inventory, but a process record is a belief:
it can survive the worker it names. A remote dashboard that renders those
records as “running” cannot distinguish a live worker from an orphaned
molecule. Local operator surfaces reconcile transport state before rendering,
so the remote and local readings diverged.

The tenant also needs the whole non-terminal workload, including pending and
frozen molecules that have no worker. A worker-keyed endpoint cannot express
those rows without inventing workers.

## Decision

Add authenticated `GET /v1/vitals` as an adapter-only, per-noyau read surface
under `cosmon:molecule:read` (or write, which implies visibility).

The document has one row per non-terminal molecule. Each row keeps persisted
`status` separate from observed `health`:

- `unassigned`: the molecule has no process record;
- `live`: the tenant transport confirms the recorded worker exists;
- `orphaned`: a process is recorded and the transport confirms it is absent;
- `unknown`: a process is recorded but the transport probe failed.

The distinction is fail-honest: transport failure never becomes “orphaned”
and never removes the row. `awaiting_operator` reads only the declared
`temp:awaiting-op` control-plane tag. The route does not inspect a pane or
transcript for undeclared waiting or mute hangs; clients may request
`/v1/molecules/{id}/session` for that more expensive, per-molecule question.

The complete fold lives in `cosmon_state::ops::vitals`. It receives a
`StateStore`, an injected worker-health port, the lease-molecule set, and the
current instant. The core operation therefore performs no transport,
filesystem, or clock I/O. It lists state once, filters non-terminal rows,
classifies observed health, computes counts, and calls
`cosmon_core::staleness::backlog_age`. The HTTP adapter supplies the
per-tenant filestore, transport backend, lease ledger reading, and clock.

The response carries the observation instant and a backlog summary. The
staleness threshold is not restated at the HTTP boundary: it remains the one
shared definition used by `cs status` and `cs peek`.

## Consequences

- A remote principal can distinguish a live worker from an orphan without
  host-global tmux access.
- Tenant isolation follows admission before either the store or transport is
  selected; one noyau cannot probe another noyau's workers.
- Persisted lifecycle state is never presented as observed liveness.
- `GET /v1/workers` remains unchanged. It is still useful as a process-record
  inventory and does not silently acquire a second contract.
- Renderers can share one typed aggregation instead of independently joining
  state, tags, liveness, counts, and backlog age.
- The fleet endpoint stays cheap enough to poll because it does not scan
  transcripts or capture panes.

## Alternatives refused

### Enrich `GET /v1/workers`

Refused because the requested cardinality is molecule-keyed. Pending,
unassigned, frozen, and starved molecules would remain invisible.

### Expose `cs peek`

Refused because `peek` is an operator-wide terminal rendering containing the
operator's sensorium. It is not a tenant document and its raster is not an API
schema.

### Detect mute hangs in the fleet fold

Refused because that verdict requires pane or transcript inspection. Paying
that cost for every molecule on every dashboard poll conflates a cheap fleet
index with an on-demand session diagnosis.
