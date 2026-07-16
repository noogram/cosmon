# Phantom workers — diagnostic 2026-04-25

**Reported by:** accord galaxy (mission-20260424-0362, Tenant-Demo v1.2.0 contract flotte)
**Reproduced in:** task-20260425-8e26
**Affected commands:** `cs tackle <child> --force-runtime`, `cs run <terminal-root>`
**Severity:** blocking — silent failure mode that wastes hours of operator time.

## TL;DR

`cs tackle <id> --force-runtime` on a DAG root (a molecule with active
dependents) marks the root molecule as `Running` and writes
`assigned_worker = <runtime-tmux-wid>` **before** the runtime daemon has
had a chance to dispatch it. The runtime daemon then sees the root as
already `Running` with an assigned worker and refuses to dispatch a real
claude session for it: every gate that would normally spawn a worker
(`DagPolicy::next_actions` via `frontier::compute_from_molecules`,
`Executor::drain_native_tail`) explicitly skips molecules that already
hold a `Running` status and an `assigned_worker`. The result is a tmux
session named `runtime-…` that loops forever showing the dashboard but
never spawns a child worker — a *phantom worker*: status `Running`,
zero progress, zero token cost, zero filesystem activity.

The companion symptom — `cs run <root>` exiting "Torn down 1 completed
molecule(s)" while pending descendants remain — has the same root
mechanism on the other side of the option-B collapse boundary: when the
named root is itself terminal (`Collapsed` or `Completed`),
`DagPolicy::absorb_collapsed` keeps the parent out of the skip-set so
`(parent, child)` edges remain gated, the policy emits no actions, and
the runtime drains immediately. The operator who explicitly typed
`cs run <terminal-root>` was asking for "continue past the terminal
root", but the policy interpreted the call as "nothing left to do".

## Code traces

### Trace A — `cs tackle <child> --force-runtime`

1. `crates/cosmon-cli/src/cmd/tackle.rs:192` — `has_active_dependents`
   returns `true` for a Wave-1 redacteur whose `Blocks` edges target
   pending Wave-2 verifiers, so the call routes into
   `tackle_as_runtime`.
2. `crates/cosmon-cli/src/cmd/tackle.rs:1164` — `runtime_session::spawn_runtime_session`
   creates a detached tmux session running `cs run <id>`. The session
   genuinely starts (verified at line 1179) but the process inside it
   is the runtime daemon, not a claude worker.
3. `crates/cosmon-cli/src/cmd/tackle.rs:1193-1200` — the root molecule
   is mutated:
   ```rust
   let mut updated = mol;
   if updated.status == Pending || updated.status == Queued {
       updated.status = Running;          // ← the rogue write
   }
   updated.assigned_worker = Some(wid);   // ← the rogue write
   updated.session_name = Some(session_name);
   updated.updated_at = Utc::now();
   store.save_molecule(&mol_id, &updated)?;
   ```
   The `wid` here is the runtime daemon's tmux session id, not a claude
   worker. The runtime is the *orchestrator* of this molecule, not the
   *worker* that produces its artefacts.
4. The runtime daemon boots inside the tmux session and calls
   `compile_plan` on `<id>`, then enters its event loop.
5. `crates/cosmon-runtime/src/dag_policy.rs:487-494` — the policy
   filters the ready frontier through
   `frontier::compute_from_molecules`, which requires
   `status == Pending && assigned_worker.is_none()`
   (`crates/cosmon-state/src/frontier.rs:163-164`). Because the rogue
   write at step 3 set `Running` and an `assigned_worker`, the root is
   not eligible. The policy emits no `Evolve` action for it.
6. `crates/cosmon-runtime/src/lib.rs:391` —
   `SubprocessExecutor::drain_native_tail` is the only other dispatch
   path that could spawn a worker on a `Running` molecule. It bails out
   on the same `assigned_worker.is_some()` test:
   ```rust
   if !bypasses && mol.assigned_worker.is_some() {
       return Ok(false);
   }
   ```
7. The wave-2 verifiers are gated by `BlockedBy(<wave-1-id>)`. They are
   ineligible for dispatch because their predecessor is `Running`
   (`predecessor_cleared` only releases on `Collapsed`,
   `Completed+merged_at`, or `Frozen+merged_at`). Nothing dispatches.
8. The runtime loops forever in the "actions empty + has_running"
   branch (`crates/cosmon-runtime/src/lib.rs:986-998`): it sleeps
   `poll_interval`, ticks again, observes the same state, sleeps again.
   The dashboard renderer keeps drawing heartbeat frames. `cs ensemble`
   reports the runtime worker as `running healthy` because tmux is
   alive — but no claude was ever spawned.

### Trace B — `cs run <terminal-root>`

1. `crates/cosmon-cli/src/cmd/run.rs:147` — `compile_plan` walks the
   closure (`mission` + 15 children).
2. `crates/cosmon-runtime/src/dag_policy.rs:441-444` — at the first
   tick `absorb_completion` runs on `mission` with
   `CompletionKind::Collapsed`:
   ```rust
   match kind {
       CompletionKind::Completed => self.absorb_completed(...),
       CompletionKind::Collapsed => self.absorb_collapsed(...),
   }
   ```
3. `crates/cosmon-runtime/src/dag_policy.rs:304-334` — `absorb_collapsed`
   intentionally does **not** insert `mission` into `self.completed`.
   The Plan's skip-set therefore does not unblock children whose only
   blocker is the collapsed `mission`. (This is option B from
   `DIAGNOSIS-mission-collapse.md` — "lateral drain without forward
   leak".)
4. `plan.ready()` returns nothing usable: `mission` is not in the
   ready frontier (already terminal), and children remain gated
   because `(mission, child)` edges are still alive in the plan.
5. `next_actions` returns an empty vector. `has_running == false`.
   The runtime exits with `ShutdownReason::PolicyDrained`.
6. `crates/cosmon-cli/src/cmd/run.rs:289-309` — the post-run teardown
   loop calls `cs done` on every terminal molecule in the closure.
   Mission is the only terminal one, so it prints "Torn down 1
   completed molecule(s)" and exits.

The 15 pending children remain on disk, blocked by a collapsed parent,
invisible to any future `cs run` because option B keeps them gated.

## Hypotheses validated

- **Validated**: `cs tackle --force-runtime` writes `Running` +
  `assigned_worker` on the root *before* the runtime can dispatch it.
- **Validated**: `frontier::compute_from_molecules` filters out
  Running-with-worker molecules; this is the gate that prevents the
  runtime daemon from touching its own root.
- **Validated**: `drain_native_tail` is also gated on `assigned_worker`,
  so the secondary dispatch path is closed too.
- **Validated**: `cs run` on a `Collapsed` root drains immediately
  because option-B keeps the collapsed parent out of the skip-set.
- **Invalidated**: the briefing's hypothesis that `--force-runtime`
  *attaches to an existing daemon*. There is no shared daemon — every
  `cs tackle --force-runtime` call spawns its own `cs run` inside a
  fresh tmux session. The phantom comes from the rogue molecule write,
  not from a missing daemon.

## Why `cs ensemble` says "healthy"

`cs ensemble`'s liveness check is binary on tmux session status. As
long as the runtime tmux session is alive (the daemon is ticking), the
worker reports `running`. There is no progress watchdog: a worker that
holds `Running` for hours with `current_step == 0`, zero token cost,
and no filesystem activity is indistinguishable from a healthy
long-running step.

## The bug auto-freeze (jq error) — out of scope but noted

Mission collapsed at step 4 because the `auto-freeze` gate's jq
expression failed with `"Cannot index array with string 'id'"`. This
is a separate bug: the gate's input shape changed (or never matched
the jq expression). It is the *trigger* for the phantom-worker
incident — without the false-collapse the operator would not have
needed `cs tackle --force-runtime` — but it is not the cause of the
phantom-worker bug. Filing as a separate ticket
(`auto-freeze-jq-shape-bug-20260425`).

## Fix design

### Fix 1 — `cs tackle --force-runtime` must not pre-mutate the root

`tackle_as_runtime` should:

- Spawn the runtime tmux session (existing).
- Verify the session started (existing).
- Register the runtime as a `Runtime`-role worker in the fleet
  (existing) so `cs ensemble` lists it. The worker's `current_molecule`
  field still points at the root for surface display.
- **Not** set `mol.status = Running`.
- **Not** set `mol.assigned_worker`.
- **Not** set `mol.session_name` on the root molecule.

The runtime daemon's `DagPolicy` will dispatch the root through the
normal path on the first tick: it will be `Pending`, in the frontier,
and `apply_evolve` will spawn a real claude via `cs tackle <id> --leaf`.

Concretely the fix is a small subtract — delete the molecule mutation
block at `tackle.rs:1193-1200`. The existing
`register_runtime_worker` already records the runtime's relationship
to the root through the worker's `current_molecule` field, which is
the correct level of indirection.

### Fix 2 — `cs run <terminal-root>` is an explicit operator override

When the operator types `cs run <id>` on a molecule that is already
terminal (`Collapsed` / `Completed` / `Frozen`), they are explicitly
asking the runtime to continue past it. The runtime should treat the
named root as if it were `Completed` for skip-set purposes,
*regardless* of its actual terminal status. This unblocks forward
`Blocks` dependents, letting the runtime dispatch the convoy of
children.

This is contained — only the explicitly-named root gets the special
treatment. Children that collapse mid-run still respect option B
(`absorb_collapsed` continues to gate their forward dependents).

The implementation lives in `cs run`'s policy construction
(`crates/cosmon-cli/src/cmd/run.rs`): after `compile_plan`, if the
named root's status is `Collapsed`, force-insert it into
`DagPolicy::completed` via a new helper (`DagPolicy::pre_complete`).
The runtime then sees the root as a satisfied predecessor on the very
first tick.

### Fix 3 — `cs ensemble` flags phantom workers

A phantom worker is observable: status `Running`, `current_step == 0`,
no token cost, age > 5 min. `cs ensemble` should surface this with a
`👻 phantom?` annotation so the operator does not have to guess from
the cost column. Implementation deferred to a follow-up molecule
(noted in the briefing as priority moyenne) — the structural fixes
above remove the phantom-creation pathway, while this fix would catch
phantom-creation pathways we haven't found yet.

## Acceptance scope for this molecule

- Fix 1 implemented + unit test (mandatory).
- Fix 2 implemented + unit test (mandatory).
- Fix 3 deferred to a follow-up molecule (priority moyenne; the bug is
  a defense-in-depth observability layer, not a structural correctness
  property).
- Integration regression test reproduces the symptom and asserts the
  fix.
- This document committed as the artefact-of-record.
- Chronicle entry filed in an internal chronicle.
