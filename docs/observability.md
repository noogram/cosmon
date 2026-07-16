# Observability — EventV2 Schema

**This is the source of truth for any post-hoc reconstruction of a fleet run.**

## Overview

Every meaningful state change in a cosmon fleet emits an `EventV2` record to
`.cosmon/state/events.jsonl`. The log is append-only, JSONL-formatted, and
designed for replay: given the event log alone, you can reconstruct a
frame-by-frame animation of the fleet's lifecycle.

## Envelope

Every record is wrapped in an `Envelope`:

```json
{
  "seq": 42,
  "timestamp": "2026-04-11T14:30:00.123456Z",
  "causal_parent": 7,
  "type": "molecule_nucleated",
  "molecule_id": "task-20260411-abcd",
  "formula_id": "task-work"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `seq` | `u64` | Monotone sequence number, unique per log file. Authoritative for ordering. |
| `timestamp` | ISO8601 UTC | Wall-clock time when the event was recorded. Non-decreasing in practice. |
| `causal_parent` | `u64?` | `seq` of the event that caused this one. `null` for externally triggered events. |
| `type` | string | Discriminator tag (see variants below). |

## Variants

### `molecule_nucleated`

A molecule was created via `cs nucleate`.

| Field | Type |
|-------|------|
| `molecule_id` | `MoleculeId` |
| `formula_id` | `string` |

**Emission point:** `crates/cosmon-cli/src/cmd/nucleate.rs`

### `molecule_status_changed`

A molecule transitioned between lifecycle statuses.

| Field | Type |
|-------|------|
| `molecule_id` | `MoleculeId` |
| `from` | `string` (snake_case status) |
| `to` | `string` (snake_case status) |

**Emission points:** `complete.rs`, `collapse.rs`

### `molecule_step_completed`

A formula step was completed (via `cs evolve`).

| Field | Type |
|-------|------|
| `molecule_id` | `MoleculeId` |
| `step` | `usize` (zero-based) |
| `total` | `usize` |
| `duration_ms` | `u64?` (wall-clock, if measured) |

**Emission point:** `crates/cosmon-cli/src/cmd/evolve.rs`

### `molecule_completed`

A molecule reached the `Completed` terminal status.

| Field | Type |
|-------|------|
| `molecule_id` | `MoleculeId` |
| `duration_ms` | `u64?` (total lifecycle duration, if known) |
| `reason` | `string` |

**Emission points:** `evolve.rs` (auto-complete on last step), `complete.rs`

### `molecule_collapsed`

A molecule reached the `Collapsed` terminal status.

| Field | Type |
|-------|------|
| `molecule_id` | `MoleculeId` |
| `reason` | `string` |

**Emission point:** `crates/cosmon-cli/src/cmd/collapse.rs`

### `molecule_stuck`

A molecule was marked stuck (needs human intervention).

| Field | Type |
|-------|------|
| `molecule_id` | `MoleculeId` |
| `reason` | `string` |

**Emission point:** `crates/cosmon-cli/src/cmd/stuck.rs`

### `decay_spliced`

A parent molecule was spliced into child molecules (1 → N).

| Field | Type |
|-------|------|
| `parent` | `MoleculeId` |
| `children` | `[MoleculeId]` |

**Emission point:** `crates/cosmon-cli/src/cmd/interaction.rs` (`run_decay`)

### `merge_dispatched`

`cs done` began merging a molecule's branch into the base branch.

| Field | Type |
|-------|------|
| `molecule` | `MoleculeId` |
| `branch` | `string` |

**Emission point:** `crates/cosmon-cli/src/cmd/done.rs` (before `try_merge_branch`)

### `merge_completed`

A merge attempt finished (successfully or with an error).

| Field | Type |
|-------|------|
| `molecule` | `MoleculeId` |
| `branch` | `string` |
| `result` | `string` (`"ok"`, `"conflict"`, `"error:..."`) |

**Emission point:** `crates/cosmon-cli/src/cmd/done.rs` (after each `MergeOutcome` arm)

### `worker_spawned`

A worker was created by `cs tackle`.

| Field | Type |
|-------|------|
| `worker_id` | `WorkerId` |
| `molecule` | `MoleculeId?` |
| `session_name` | `string` |
| `role` | `string` |

**Emission point:** `crates/cosmon-cli/src/cmd/tackle.rs` (`register_tackle_worker`)

### `worker_killed`

A worker was terminated (by `cs kill`, `cs purge`, or operator action).

| Field | Type |
|-------|------|
| `worker_id` | `WorkerId` |
| `reason` | `string` |

**Emission points:** `crates/cosmon-cli/src/cmd/kill.rs`, `crates/cosmon-cli/src/cmd/purge.rs`

### `energy_tick`

Periodic token consumption snapshot for a running worker.

| Field | Type |
|-------|------|
| `worker_id` | `WorkerId` |
| `input_tokens` | `u64` |
| `output_tokens` | `u64` |
| `cost_usd` | `f64` |

**Emission point:** Runtime tick loop (future — emitted by the resident runtime
once per poll interval per running worker by reading their current token totals).

## Querying the Log

### `cs events tail`

Live-stream the last N events and optionally follow new ones:

```bash
cs events tail -n 20         # last 20 events
cs events tail -n 5 -f       # last 5 + follow
```

### `cs events query`

Filter events by type and/or time range:

```bash
cs events query --kind molecule_nucleated
cs events query --kind energy_tick --since 2026-04-11T10:00:00Z
cs events query --kind worker_spawned --limit 10
cs events query --json --kind merge_completed
```

### `cs events stats`

Aggregate counts per variant:

```bash
cs events stats
cs events stats --json
```

### `cs events validate`

Check log integrity — monotone sequences, parseable lines:

```bash
cs events validate
```

## Migration from Legacy Format

Legacy `events.jsonl` lines (pre-EventV2) used inconsistent shapes: some
had `"type"`, others `"kind"`, some had neither. The `migrate_legacy_line`
function in `cosmon_core::event_v2` coerces recognised shapes into `EventV2`
envelopes with `seq=0` and no causal parent. Unrecognised lines are skipped
by `read_all`.

During the grace window, both legacy and EventV2 records coexist in the
same `events.jsonl` file. The EventV2 writer assigns monotone sequences
starting from the last EventV2 record; legacy lines are ignored for
sequencing purposes.

## Invariants

1. **Monotone `seq`**: Within a single `events.jsonl`, `seq` values are
   strictly increasing for EventV2 records.
2. **Append-only**: Records are never modified or deleted.
3. **Causal chain**: When `causal_parent` is set, it refers to an earlier
   `seq` in the same log file.
4. **Dual-write**: During migration, both legacy `Event` and `EventV2`
   records are emitted for the same operation. The EventV2 record is
   authoritative; the legacy record exists for backward compatibility.
