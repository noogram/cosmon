# ADR-041: Atomic Frontier Projection

## Status

Accepted (2026-04-14) — implemented in task-20260414-d631.

Derived from deliberation `delib-20260414-e6b8` (TLA+ vs foundry-tla vs
hybride). See the synthesis file's insights I2 (torvalds) and I4 (einstein)
for the full convergence trail. This ADR is the **pre-spec refactor** that
gates the Phase 1 TLA+ scheduler specification in task-20260414-2f95.

## Context

Cosmon's resident runtime (`cs run`, Layer B) decides on every poll tick
whether a molecule is **safe to dispatch** by combining two independent
facts:

1. **DAG readiness.** Every upstream `BlockedBy` predecessor has reached a
   terminal cosmon state (`Completed`, `Frozen`, or `Collapsed`). This is
   computed inside `DagPolicy::next_actions` from the in-memory
   `Plan<MoleculeId>` produced by `compile_plan`.

2. **Predecessor branch merged.** `cs done` has fast-forwarded or
   three-way-merged the predecessor's feature branch back onto the base
   branch, so the dependent worker — which `cs tackle`s a fresh worktree
   branched off the predecessor's branch — can see the predecessor's code
   committed. Merge-before-dispatch is documented as an invariant in
   `docs/architectural-invariants.md`.

Fact (2) is enforced **only** by the temporal ordering of the runtime
loop: the loop calls `Executor::on_complete` (which runs `cs done`) for
every freshly-observed completion **before** calling
`Policy::next_actions`. Nothing on disk records that both facts hold
simultaneously. The upcoming Phase 1 TLA+ scheduler specification
(`task-20260414-2f95`) would therefore have to model two state variables
(`dagReady`, `branchMerged`) and carry a temporal invariant relating
them; torvalds' audit of the synthesis estimated this at **~1/3 of the
total proof obligations**.

## Decision

Collapse the two facts into **one atomic filesystem projection**:

```
.cosmon/state/frontier.json
```

The projection is a plain JSON document with shape:

```json
{
  "version": 1,
  "computed_at": "2026-04-14T21:07:42.312Z",
  "ready": ["task-20260414-aaaa", "task-20260414-bbbb"]
}
```

and is produced by a **pure reducer**,
`cosmon_state::frontier::compute_from_molecules`, that runs in a single
pass over the molecule list. A molecule enters `ready` iff every upstream
predecessor is in a **merged-terminal** state:

- `status ∈ {Completed, Frozen} ∧ merged_at.is_some()`, or
- `status == Collapsed` (decay-collapse releases successors).

The `merged_at: Option<DateTime<Utc>>` field is added to
`cosmon_state::MoleculeData` as the structural half of "branch merged":
it is stamped **once**, by the runtime loop immediately after
`Executor::on_complete` returns `Ok` for a given molecule (see
`Runtime::run` in `crates/cosmon-runtime/src/lib.rs`). `cs done` also
stamps it from the post-merge path, so humans invoking `cs done`
directly produce the same on-disk fact as the resident runtime.

`frontier.json` itself is written at two complementary points:

1. **`cs done`** — after the post-merge hook runs, `cs done` computes the
   new frontier and writes it. This is the one instant both facts are
   simultaneously true for a freshly-merged molecule.
2. **`cs reconcile`** — rebuilds the frontier from authoritative state,
   independently of any lifecycle event. This keeps `cs reconcile` the
   canonical "reproject everything" command and lets operators recover
   from a stale or missing `frontier.json` without restarting the
   runtime.

The `DagPolicy::next_actions` scheduler is refactored to intersect its
DAG-derived ready set with the frontier reducer's output **in the same
pass**, so the policy emits `Evolve` actions only for molecules that
satisfy both conditions structurally. The file on disk is a cache for
external observers (TLA+ verification, `cs peek`, dashboards); the
scheduler trusts the in-memory reducer because it runs against the same
snapshot it uses for other decisions.

## Consequences

### Positive

- **Proof obligation count drops by ~1/3.** The Phase 1 TLA+ scheduler
  spec (`task-20260414-2f95`) can model a single `frontier` state
  variable instead of `dagReady` + `branchMerged` + temporal interleaving
  invariants. This is the explicit gate torvalds' insight unlocks.
- **Structural, not temporal.** Merge-before-dispatch becomes a fact on
  disk (`merged_at`), not an invariant about loop ordering. Crash
  recovery, mixed human+runtime operation, and external observers all
  see the same truth.
- **One reducer, two clients.** `compute_from_molecules` is a pure
  function used by both the scheduler (in-memory) and the persistence
  layer (`cs done`, `cs reconcile`). Adding a new consumer (TUI,
  dashboard) is a single call, not a re-derivation.
- **Observable.** Operators can `cat .cosmon/state/frontier.json` to
  inspect what the scheduler thinks is dispatchable right now, without
  running `cs run` or reading the event log.
- **Idempotent.** `reconcile ≡ reconcile ∘ reconcile` is enforced by a
  property test in `crates/cosmon-state/src/frontier.rs` and by the
  atomic temp-file-plus-rename write in `save`.

### Negative

- **New field on `MoleculeData`.** Every construction site (tests and
  real code) must set `merged_at: None`. Backward-compatible
  deserialization (`#[serde(default)]`) means legacy JSON files
  continue to load.
- **Two write-sites for the projection.** `cs done` and `cs reconcile`
  both write `frontier.json`. This is intentional — `cs done` catches
  the fast path, `cs reconcile` is the authoritative rebuild — but
  requires that both sites call the same reducer (they do).
- **The runtime still holds a `Plan`.** The DAG plan is still used for
  absorption, critical-path ordering, and splice rebuilds. The frontier
  reducer is an additional filter, not a replacement. This is
  deliberate: the plan's edge structure encodes ordering that the flat
  reducer doesn't. A future ADR may consolidate them further.

### Neutral

- **Best-effort read.** `load` returns `Ok(None)` for missing or
  corrupt files. Consumers must fall back to `compute` on the live
  store. This keeps a stale `frontier.json` from blocking the system
  and makes `rm .cosmon/state/frontier.json; cs reconcile` a safe
  recovery procedure.

## Alternatives considered

- **No projection, keep two-phase check.** Rejected: ships the temporal
  invariant into the TLA+ spec and inflates proof obligations.
- **Stamp `merged_at` inside `cs complete`.** Rejected: `cs complete`
  runs while the branch is still unmerged (workers call it from inside
  their worktree). Conflating completion and merge would violate the
  merge-before-dispatch invariant.
- **Compute the frontier only at runtime tick, no file.** Rejected:
  loses observability, TLA+ reference point, and cross-process sharing
  between `cs done` and `cs run`.
- **Store `merged_at` as an event, not a field.** Rejected: the
  scheduler would have to re-scan the event log on every tick to derive
  the current merge state — exactly the two-phase check this ADR
  eliminates.

## Implementation

- New module `cosmon_state::frontier` with `Frontier`,
  `compute_from_molecules`, `compute`, `save`, `load`, `path`.
- New field `merged_at: Option<DateTime<Utc>>` on
  `cosmon_state::MoleculeData`.
- Stamping sites: `Runtime::run` (post `on_complete`), `cs done` (post
  successful merge).
- Projection write sites: `cs done`, `cs reconcile`.
- Scheduler refactor: `DagPolicy::next_actions` intersects its ready
  frontier with `frontier::compute_from_molecules`.
- Property test `compute_is_idempotent` and roundtrip test
  `save_then_recompute_yields_same_ready` in `frontier.rs`.
