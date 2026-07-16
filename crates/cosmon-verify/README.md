# cosmon-verify — trace validator

`cosmon-verify` replays an append-only `events.jsonl` log through a
*scheduler model* and either (a) certifies the trace as a refinement of the
model's invariants, or (b) reports the first invariant violation with
enough context for an operator to diagnose it.

It is the Phase 3 deliverable from deliberation `delib-20260414-e6b8` —
wheeler's insight (I1) that **`events.jsonl` is already the
labeled-transition-system trace** of a cosmon run, so the shortest path to
a CI gate is not full model checking but trace validation.

## Binary

The `cs verify-trace` subcommand is the operator-facing entry point:

```
cs verify-trace path/to/events.jsonl
cs verify-trace - < events.jsonl
cs verify-trace --skip-unknown .cosmon/state/events.jsonl
cs verify-trace path/to/events.jsonl --json
```

Exit code is `0` on certification, `1` on the first invariant violation or
a parse error. `--skip-unknown` tolerates historical line shapes that pre-
date the `EventV2` schema (counted in the output's `skipped_unknown`).

## Baseline invariants

Phase 1 ships a conservative baseline (see `src/invariants.rs`):

| id | description |
|----|-------------|
| `molecule_exists_before_use` | every event that names a `molecule_id` must be preceded by a `molecule_nucleated` for that id |
| `status_transition_legal` | `molecule_status_changed.from` must equal the current status and `to` must be a legal successor |
| `no_events_after_terminal` | after `molecule_completed` / `molecule_collapsed`, no further lifecycle events may reference the molecule |
| `step_monotone` | consecutive `molecule_step_completed` events must have non-decreasing `step` indices |
| `step_within_total` | `molecule_step_completed.step` must be `< total` and `total` must not change across events |
| `worker_spawned_before_killed` | `worker_killed` must follow a prior `worker_spawned` for the same `worker_id` |
| `merge_completion_pairs_dispatch` | `merge_completed` must follow a prior `merge_dispatched` with the same molecule and branch |

Each predicate is an `Invariant` implementation — the set is pluggable, so
`task-20260414-2f95` (Phase 1 TLA+ spec) can later lower `TypeOK` / `Next`
into the same trait without touching the CLI.

## CI integration

The intended Phase 3 gate runs `cs verify-trace` against a curated set of
historical `events.jsonl` files on every push:

```yaml
# .github/workflows/trace-gate.yml (sketch)
- name: Validate historical traces
  run: |
    for f in tests/fixtures/traces/*.jsonl; do
      cargo run -q -p cosmon-cli --bin cs -- verify-trace "$f"
    done
```

A newly-added invariant that rejects a historical trace is a **signal**
— either the spec is wrong or the historical behaviour was buggy. The
operator then chooses: widen the invariant, fix the code, or curate the
trace out of the gate.

## Architecture

- `model.rs` — `SchedulerState`: the projection of an event stream onto
  the variables invariants read.
- `invariants.rs` — the `Invariant` trait and the baseline set.
- `validator.rs` — `TraceValidator`: a linear replay that runs every
  invariant against the pre-transition state, then mutates the state.
- `error.rs` — `Violation` (first failing invariant) and `ValidationError`
  (parse / I/O).

All state mutation happens in a private `apply` function inside the
validator — invariants see an immutable snapshot, matching the shape of
TLA+'s `Next` action.
