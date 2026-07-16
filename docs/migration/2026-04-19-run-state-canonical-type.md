# Migration — `RunState` canonical type and `fleet.json` projection

**ADR:** [052 — One Ledger, One Writer, One Witness per Field](../adr/052-one-ledger-one-writer-one-witness.md)
**Child task:** `task-20260419-b06d` — the child #1 of the ADR's
decomposition plan.
**Status:** first landing (additive, non-breaking). Persisted schema
unchanged; legacy reads unaffected.

## What changed

A new module `cosmon-core::run_state` introduces the canonical type:

```rust
pub struct RunState {
    pub intent: Intent,            // pilot-owned (I2)
    pub witness: Option<Witness>,  // probe-owned (I2 + I8)
}
```

together with `Intent`, `Terminus`, `Liveness`, `BranchState`, `Witness`,
`GhostKind`, and `DriftError`. The `RunState::ghost(now, probe_ttl)`
pattern-match maps the three-source drift of the 18–19 April incident log
onto named variants — detection, not prevention, per ADR-052 §D5.

`MoleculeStatus` is now annotated `#[doc(hidden)]` and documented as a
**legacy projection**. It remains public in the source (existing callers
compile unchanged) but rustdoc no longer surfaces it; new code must go
through `RunState`. **No new variants may be added** to `MoleculeStatus`
before the persisted-state migration completes — this is the Tolnay §5
semver hazard the ADR names explicitly.

The `#[serde(alias = "active")]` on `MoleculeStatus::Running` is
**preserved**. It stays in place through the next major bump so every
`fleet.json` and `state.json` on disk continues to deserialize.

## What did not change

- `fleet.json` on-disk schema — still carries the legacy `desired`
  / `status` / worker rows.
- `.cosmon/state/molecules/<id>/state.json` — still uses
  `MoleculeStatus`.
- `cs tackle`, `cs evolve`, `cs done`, `cs patrol` — CLI entry points
  untouched in this child; their migration lands in children #3–#7.

This landing is a pure superset: any code reading `MoleculeStatus` or
`DesiredState` today keeps working; code that wants the new shape gets
it via the `impl From<…>` bridges below.

## Migration plan for `.cosmon/state/fleet.json`

The file is read by `cosmon-state` and `cosmon-filestore`. Today each
worker row carries both `status` (`WorkerStatus`) and `desired`
(`DesiredState`); molecule rows carry `MoleculeStatus`. The canonical
projection is:

```text
worker.desired             -> RunState.intent
worker.status              -> RunState.witness.process (via liveness_from_worker_status)
probe(tmux has-session)    -> RunState.witness.process  (override, fresher)
probe(git rev-list --count-in HEAD...) -> RunState.witness.branch
molecule.status            -> Intent::from(MoleculeStatus)
```

**Step A — Readers (this child).** Ship `cosmon_core::run_state::{Intent,
RunState, Witness, GhostKind}` and the `impl From<…>` bridges so any
consumer that wants the new shape can derive it *on read*:

```rust
use cosmon_core::run_state::{Intent, RunState, Witness, BranchState, Liveness, liveness_from_worker_status};
use cosmon_core::worker::WorkerStatus;

fn to_run_state(ws: &WorkerStatus, desired: cosmon_core::worker::DesiredState, tmux_alive: Option<bool>) -> RunState {
    let intent = Intent::from(desired);
    let witness = tmux_alive.map(|alive| {
        Witness::new(
            if alive { Liveness::Alive } else { Liveness::Dead },
            BranchState::Unmerged, // branch state probed separately
        )
    }).or_else(|| {
        // Fall back to the coarser WorkerStatus mapping.
        Some(Witness::new(liveness_from_worker_status(ws), BranchState::Unmerged))
    });
    RunState { intent, witness }
}
```

No on-disk change required; the projection is read-only.

**Step B — Writers (child #3, events.jsonl integrity).** Once the
event-log writer gains `flock(2)` + monotonic seq, every intent write
emits an `IntentWritten` event, every probe emits a `WitnessRecorded`
event. Readers rebuild `RunState` by fold over the ledger.

**Step C — Dual-write fleet.json (child #4, pane-died mandatory).** When
`cs tackle` installs the `pane-died` hook, each worker row gains a new
optional field `run_state`:

```json
{
  "id": "ep-quartz",
  "definition": "polecat",
  "status": "active",
  "desired": "running",
  "run_state": {
    "intent": "run",
    "witness": {
      "observed_at": "2026-04-19T14:23:00Z",
      "process": "alive",
      "branch": "unmerged"
    }
  }
}
```

The `run_state` field is written additively; legacy fields stay for one
minor cycle so older `cs` binaries can still read the file. After the
next major bump (`0.2.0`), legacy fields are dropped from the emitter
but still accepted on read.

**Step D — Flip to `RunState` as authoritative (child #5, git pre-merge
hook).** The git hook refuses merges without a recorded `cs done`, which
means the `Merged` branch state has a provenance chain. At that point
`RunState` becomes the source of truth; `MoleculeStatus` writes are
removed; the `#[doc(hidden)]` alias enters its one-cycle deprecation.

## Rollback

Every step is independently rollback-able:

- Step A: delete the new module. No on-disk state changed.
- Step B: revert the event-log writer. Readers fall back to the
  Step-A projection.
- Step C: remove the `run_state` field from the emitter. Legacy fields
  still present; older binaries keep reading.
- Step D: re-enable `MoleculeStatus` writes. The `#[serde(alias)]`
  kept on `Running` ensures both spellings deserialize.

No destructive operation occurs before child #5 (the git hook), and
that one is opt-in per cosmon-tracked galaxy.

## Why this is strictly additive

Three guarantees keep the current landing safe:

1. **No field in `MoleculeStatus` was removed or renamed.** The
   `#[doc(hidden)]` attribute does not affect compilation, only
   rustdoc rendering. Every downstream crate that imports the type
   still compiles.
2. **No field in `WorkerStatus`, `DesiredState`, or `fleet.json` was
   removed or renamed.** The new type is read-only in this landing;
   it never writes to disk.
3. **`RunState` serde output is a fresh JSON shape.** It cannot
   collide with any existing field because it is not written yet.

The migration fits the ADR-052 constraint *"propose mechanisms of
verification, do not impose them"*: this landing proposes `ghost()`
as the detection primitive. Imposition happens in later children,
each gated by its own ADR-level review.
