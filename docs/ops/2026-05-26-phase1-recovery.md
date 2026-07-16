# Ops — ADR-110 Phase 1 recovery (single-writer-trunk + event-fold + TLA+ deadlock fix)

**Date:** 2026-05-26
**Molecule:** `task-20260525-0b25` (*recovery-phase1-abda-trunk-lock-3134-event-fold-tla-deadlock*).
**Branch:** `feat/task-20260525-0b25-phase1-recovery` (cosmon).
**Resolves:** an internal audit.
**Aligns:** [ADR-110](../adr/110-single-writer-trunk-and-coordination-invariants.md).

---

## TL;DR

A branch-wipe (erratum 6166) erased ADR-110's three "already on `main`"
Phase 1 commits. `cs stitch` (a364) was recovered earlier (`3856c00ce`).
This molecule recovers the **other two coupled pieces** — the trunk lock
(abda) and the API-surface event-fold (3134) — and **bakes the deadlock
fix** the smithy TLA+ model (98e2) found preventively. All three lost
SHAs were still in the object DB, so the code was re-derived from the
dangling commits rather than re-written from scratch, then adapted:

- the trunk lock gained the **deadlock-free lock order** it never had;
- the event-fold was migrated from **22 → 29 routes** (the surface grew
  while 3134 was off-branch).

## What was lost, what came back

| Lost SHA | Reachable from `main`? (2026-05-25) | In object DB? | Recovery |
|---|---|---|---|
| `8655dad5a` trunk lock | NO | yes | re-derived + deadlock-free order |
| `1c1270b70` cs stitch | (now on `main` via `3856c00ce`) | yes | switched to trunk-only lock |
| `950986727` event-fold | NO | yes | re-derived for 29 routes |

## Piece 1 — trunk write-discipline lock (I1 + I2) + deadlock fix

**Code:** `crates/cosmon-filestore/src/lib.rs`,
`crates/cosmon-cli/src/cmd/{done,tackle,stitch}.rs`,
`crates/cosmon-filestore/examples/trunk_lock_holder.rs`,
`crates/cosmon-filestore/tests/trunk_lock_concurrent.rs`.

- `FileStore::acquire_trunk_lock(cmd_hint) -> TrunkLockGuard` +
  `with_trunk_lock(cmd_hint, f)` — advisory `flock` on
  `<state_dir>/trunk.lock` (sibling of `fleet.lock`). RAII release +
  holder-hint clear on drop. Non-blocking probe; blocks by default,
  fast-fails under `COSMON_TRUNK_LOCK_NONBLOCKING=1` with the holder's
  pid+cmd surfaced.
- `cs done`: the merge phase (relocate → merge → frontier → hook →
  archive) runs **under the trunk lock**. `--no-merge` keeps it
  unacquired.
- `cs tackle --no-worktree` is **refused** unless
  `COSMON_ALLOW_NO_WORKTREE=1` (I2 ISOLATION). Dry-run exits before the
  guard, so existing `--no-worktree --dry-run` fixtures keep working.

### The deadlock fix (the load-bearing decision)

The smithy TLA+ model `smithy/docs/formal/MCStitch.tla` found,
*preventively*, a 3-step circular-wait:

```
cs done m1:  trunk = d_m1, fleet = ⊥     ← holds trunk, wants fleet
cs stitch:   trunk = d_m1, fleet = st    ← holds fleet, wants trunk
=> DEADLOCK (Coffman circular-wait)
```

…IF `cs stitch` is integrated naïvely (keep its `with_fleet_lock` outer
wrapper and add a trunk lock inside → `fleet ⊃ trunk`, opposite to
`cs done`'s `trunk ⊃ fleet`).

**Fix implemented — a single global lock order (trunk before fleet),
realised two ways for belt-and-braces:**

1. **`cs stitch` holds the trunk lock alone** (TLA+ option 1, the
   recommended one). It rewrites no fleet/molecule state, so it never
   needs the fleet lock and can never be the fleet-side of an inversion.
   This also *strengthens* exclusion: the old `with_fleet_lock` did **not**
   serialise against `cs done`'s merge (which never ran under the fleet
   lock); both now serialise on the **same** trunk lock.
2. **`cs done` drops the trunk lock before its terminal fleet-purge**
   (the original abda evidence rule *« dropped before fleet-purge to avoid
   lock-order inversion »*). So `cs done` never holds `trunk ⊃ fleet`
   across the purge boundary either.

Net: no code path acquires `fleet ⊃ trunk`; the order is total;
circular-wait is impossible.

**Tests:**
- `trunk_lock_concurrent.rs::two_concurrent_holders_serialise_via_trunk_lock`
  — cross-process race (2 holders → one waits, one passes; lock cleared).
- `trunk_lock_concurrent.rs::nonblocking_env_fast_fails_with_holder_hint`
  — fast-fail surface with holder hint.
- `trunk_lock_concurrent.rs::lock_order_done_and_stitch_terminate_without_deadlock`
  — `cs done` (trunk ⊃ fleet, drop-before-purge) + `cs stitch`
  (trunk-only) under contention, watchdog timeout: a circular-wait would
  hang and trip `recv_timeout`.
- 5 in-process unit tests in `cosmon-filestore` (acquire/release, holder
  hint, closure error propagation, sibling-of-fleet-lock, serialisation).

## Piece 2 — API-surface event-fold (I3 ADDITIVE-COUNTERS)

**Code:** `crates/cosmon-rpp-adapter/{build.rs, data/surface_events.txt,
src/surface_events.rs, src/lib.rs}`, tests `api_surface_freeze.rs`,
`dist_serving.rs`.

- `frozen_api_surface()` returns `surface_events::SURFACE_ROUTES`, a
  compile-time fold (`build.rs`) over the append-only
  `data/surface_events.txt`. **29 routes**, byte-identical to the prior
  hand-edited list (8 molecule + 3 artifact + 5 auth-claude + auth-me +
  events + logs + quota + noyaux + workers + 2 avatar-canal + 5
  avatar-lifecycle).
- The hand-edited `assert_eq!(surface.len(), 29)` in both
  `api_surface_freeze.rs` and `dist_serving.rs` is **gone**. Adding a
  route is now one append; the count derives from `SURFACE_EVENTS.len()`.
- Tests swapped: the two hand-edited pins
  (`surface_is_twenty_nine_routes…`, `surface_pins_all_routes…`)
  replaced by the 3134 event-fold props
  (`surface_length_matches_event_log`,
  `surface_routes_project_from_event_log`,
  `event_log_has_no_duplicate_routes`,
  `every_event_carries_a_method_path_and_molecule_id`). The bijection
  test, operator-only-verb guard, and avatar exemptions are preserved.

## Piece 3 — ADR-110 alignment / erratum resolution

- Erratum marked **RESOLVED** with a resolution table + each
  recommendation addressed.
- ADR-110: recovery banner; Phase-1 commit block corrected (lost +
  re-derived); §Decision "already shipped" softened; **§I3 amended** to
  separate the compile-time API-surface fold (shipped) from the runtime
  `freeze_event.json` counter (breakage #3, never built); Phase-3 section
  records all three escalations executed (74c6 typestate, 6166 proptest,
  98e2 TLA+) with the deadlock fix baked here.

## Gates

- **`cargo check --workspace`** — ✅ clean.
- **`cargo fmt --all --check`** — ✅ clean.
- **`cargo clippy --workspace -- -D warnings`** — ✅ clean (filestore,
  rpp-adapter, cli). *Note:* `cargo clippy --all-targets` surfaces 3
  **pre-existing** test-only lints (`cas.rs:249` casts, the legacy-JSON
  migration test's `items_after_statements`) that exist on `main` and are
  not touched by this change; the gate (`--workspace`, no `--all-targets`)
  is clean.
- **`cargo test --workspace`** — ✅ green **except one pre-existing flaky
  timing test**: `cosmon-cli::cli::test_cs_wait_returns_immediately_on_terminal_molecule`
  asserts `wall < 2s` on a `cs wait` subprocess. It **passes in isolation**
  and fails only under full-workspace parallel subprocess contention. It
  exercises `nucleate → complete → wait` — **none** of the modified code
  paths (trunk lock, event-fold, `cs stitch`, `cs tackle` guard). Not a
  regression from this molecule; flagged cosmon-ward as a separate
  flaky-timing-bound issue.
- New tests added by this molecule (all green):
  - `cosmon-filestore`: 5 trunk-lock unit tests + 3 cross-process
    integration tests (`two_concurrent_holders_serialise_via_trunk_lock`,
    `nonblocking_env_fast_fails_with_holder_hint`,
    `lock_order_done_and_stitch_terminate_without_deadlock`).
  - `cosmon-rpp-adapter`: 4 event-fold props in `api_surface_freeze.rs`
    (+ bijection/operator-verb guards preserved); `dist_serving.rs` count
    literal removed.
  - `cosmon-cli`: `api_cli_coverage::every_cli_verb_has_a_registry_row`
    now green (added the missing `cs stitch` doc row — a gap left by the
    a364 recovery).

## Merge discipline (IMPORTANT — read before merging)

The anti-wipe guard (6166) is on `main`, but the **current `cs` binary may
be the old one**; do **not** rely on `cs done` to land this. The branch is
**committed locally (not pushed — local integration via molecules)**;
**the operator must merge it explicitly**:

```bash
cd /srv/cosmon/cosmon
git checkout main
git merge --no-ff feat/task-20260525-0b25-phase1-recovery
# then re-run the TLA+ non-regression in smithy:
#   cd /srv/cosmon/smithy/docs/formal && tools/tlc.sh -config MCStitch.cfg MCStitch.tla
```

**Branch to merge:** `feat/task-20260525-0b25-phase1-recovery` — its HEAD
is the commit carrying this report (`git -C /srv/cosmon/cosmon log -1
feat/task-20260525-0b25-phase1-recovery`).

## ⚠️ Cosmon-ward — worktree-isolation breach observed during this work

While this molecule was running its gates, the **cosmon resident runtime**
(`cs run --resident --poll-interval 5`, PID 67549) ran a
`cs evolve vg-20260525-5cec` *gated-step* commit **inside this feature
worktree**, sweeping the entire in-progress working tree into a commit on
the recovery branch under a foreign molecule's message. The content was
this molecule's own work, so it was recoverable (soft-reset → single
correctly-attributed commit), but a background writer committing into
another worker's isolated worktree is **exactly the I1/I2 pathology this
ADR-110 recovery hardens** — surfaced here, not silently worked around
(cosmon-ward discipline). Worth a follow-up: the resident runtime's
`cs evolve` should refuse to `git add -A && commit` a checkout whose HEAD
branch does not belong to the molecule being evolved.
