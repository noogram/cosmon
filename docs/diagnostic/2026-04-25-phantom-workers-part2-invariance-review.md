# Phantom workers part 2 — invariance review (2026-04-25)

**Reported by:** accord galaxy (mission-20260425-6e96, Tenant-Demo v1.2.0
fleet-native delivery), 2026-04-25 ~14h.
**Reproduced in:** task-20260425-911f.
**Companion to:** `docs/diagnostic/2026-04-25-phantom-workers.md` (first
diagnostic, fixed in task-20260425-8e26 commit `d8dbb3528`).
**Affected commands:** `cs tackle <id>` (without `--force-runtime`) on a
molecule with active `Blocks` dependents — the *implicit* DAG-root path.
**Severity:** structural — multiple safeguards individually sound, none
catches the defect when composed.

## TL;DR — what the operator saw

```
cd /srv/cosmon/accord
cs tackle task-20260425-21c3        # NO --force-runtime
# → "mode: runtime / session: runtime-redacteur-contrat-nda-mutuel-21c3"
sleep 5
cs ensemble
# → "running diverged" / Live "-" / Cost $0
tmux -L accord-6976 capture-pane -t runtime-redacteur-contrat-nda-mutuel-21c3
# → "can't find pane"
cs purge
# → "Reclassified 3 worker(s) to Stale (tmux session missing)"
```

The operator's workaround — `cs tackle <id> --leaf` — bypasses the
auto-detect and spawns a single claude session that does work. That
escape hatch is documented in the runtime banner emitted by
`tackle_as_runtime` (`hint: pass --leaf to dispatch <root> as a single
worker`).

The first diagnostic (8e26) *closed the path that explicitly typed
`--force-runtime`*. This second incident reproduces the same class of
bug through the *implicit* path that `has_active_dependents` selects
automatically — the runtime tmux session genuinely starts (so
`verify_runtime_session_started` passes), but it dies shortly after,
before any worker progress is visible. No safeguard in cosmon catches
the death.

## The structural question

> *"On a mis tellement de choses en place dans cosmon pour lutter contre
> les agents/flottes qui meurent (propulsion, validation runtime en
> TLA+, validation flottes en TLA+, stall detection, patrol --propel,
> etc.). Pourquoi aucun de ces safeguards n'a catchié ce cas ?"*
> — opérateur, 2026-04-25.

Cosmon has a defensive layer for almost every failure mode. The
question is *why their union is not the union of their failure
detection.*

## Inventory of safeguards — what we have today

| # | Safeguard | Source of truth | Trigger condition | Action |
|---|-----------|-----------------|-------------------|--------|
| S1 | `verify_runtime_session_started` | `cs tackle` (in-process) | Pre-spawn tmux liveness probe (5 attempts × 200ms) | Refuse mutation if session never came up |
| S2 | Startup `orphan_scan` | `cs run` runtime daemon | Once at startup, for every Running molecule | `eprintln!` warning to stderr, **no state mutation** |
| S3 | `frontier::compute_from_molecules` filter | every consumer | Pending + assigned_worker.is_none() | Filters Running-with-worker out of dispatch |
| S4 | `apply_evolve` rollback | `cs run` runtime daemon | dispatch returns Err | Reset Running → Pending, clear assigned_worker |
| S5 | `cs patrol` (default scan) | external scheduler | desired=Running + tmux=Dead → "Diverged" | Add to `stalled_workers` list in report |
| S6 | `cs patrol --respawn` | external scheduler | classified Diverged | Re-spawn tmux session |
| S7 | `cs patrol --propel` | external scheduler | Running + updated_at older than `--stale-after` | Send transport nudge to worker |
| S8 | `cs patrol --nudge` (Phase1 stall detection) | external scheduler | Running + last_progress_at older than step.timeout | Send briefing nudge, increment `nudge_count` |
| S9 | `cs purge` (sweep) | manual / scheduler | desired=Running + tmux=Dead | Reclassify worker → Stale, remove fleet entry |
| S10 | TLA+ runtime spec | review-time | Models molecule lifecycle transitions | Catches dispatch/complete/collapse violations at design time |
| S11 | Pane-died hook (`install_harvest_hook`) | tmux session | When a leaf-worker pane dies | Exec `cs harvest` from main repo |

## Case study — why each safeguard did NOT catch this

The bug pathway, after Fix-1 (8e26):

```
cs tackle <id>                    # has_active_dependents = true
  ↓
tackle_as_runtime
  ↓ spawn_runtime_session (tmux new-session "cs run <id>")
  ↓ verify_runtime_session_started → passes (tmux session is alive at t=0)
  ↓ register_runtime_worker (Runtime role, desired=Running)
  ↓ return Ok
  ↓
[runtime tmux session is now executing `cs run <id>`]
  ↓
[`cs run` either: (a) fails to dispatch the leaf, (b) dispatches OK
 then the runtime exits PolicyDrained when the leaf completes/dies,
 (c) panics on a worker error, or (d) drains for any other reason]
  ↓
cs run exits → tmux pane closes → tmux session dies → fleet has
"runtime-foo" worker with desired=Running but tmux=Dead.
```

The fleet entry plus the dead tmux is the **phantom**.

Per safeguard:

| # | Why it missed |
|---|---------------|
| **S1** | Probes once *before* registering — the session WAS alive at that instant. The probe budget is ~1s, far shorter than the ~5s observed lifetime of the runtime daemon. By the time the daemon dies, S1 is long gone. |
| **S2** | Runs **once at startup** (`Runtime::run` line ~854). After that, no further check. If the worker dies on tick 30, the orphan scan does not re-fire. Worse: even when triggered, S2 only `eprintln!`s — it never mutates state. The runtime daemon literally cannot rescue itself. |
| **S3** | The frontier filter is doing its job *correctly*: a Running molecule with assigned_worker is invisible. But the assigned_worker points at a tmux session that no longer exists. The filter has no eye for tmux liveness; it only sees the JSON. So the dead-but-marked-Running molecule wedges the DAG indefinitely (the runtime loops in `actions empty + has_running` until killed). |
| **S4** | Rollback fires only when `dispatch()` itself returns Err synchronously. If `cs tackle --leaf` *succeeds* (returns 0) and the worker dies a moment later, S4 sees no error to roll back. |
| **S5** | The patrol scan correctly classifies the dead runtime worker as Diverged, but **patrol does not run on its own** — it is an external command. Until somebody types `cs patrol`, the Diverged state sits on disk untouched. The runtime daemon does not patrol itself. |
| **S6** | Same as S5 — needs explicit invocation. Furthermore, respawning the *runtime* session is structurally different from respawning a worker: the runtime is itself a dispatcher, not a worker. `cs patrol --respawn` was designed for worker reconciliation. Whether it correctly handles a Runtime-role worker is at best ambiguous. |
| **S7** | Filters on `last_progress_at` and the staleness window. A worker that dies in <5s never emits a progress event, so `last_progress_at` is None. Patrol skips silently (`continue`). |
| **S8** | Same pre-condition as S7: `last_progress_at` must be set. Phase1 stall detection is a *post-progress* nudge, not a *no-progress-yet* detector. The nudge skip on dead tmux (`if !be.is_alive(...) { continue; }`) further filters out the case. |
| **S9** | Catches the bug *after the operator runs purge manually*. That is exactly what happened — the operator typed `cs purge` and saw `Reclassified 3 worker(s) to Stale`. Useful as forensics, useless as automatic defense. |
| **S10** | The TLA+ runtime spec models molecule transitions (Pending → Running → Completed/Collapsed). It does **not** model the (worker-side) state of `tmux pane alive` vs `tmux pane dead`. From TLA+'s perspective, `assigned_worker` always points at a live process. The state space is missing the variable that this bug lives in. |
| **S11** | The pane-died hook is installed on **leaf workers** by `cs tackle` (line ~379). It is **not** installed on the runtime tmux session itself — the runtime is registered via `register_runtime_worker`, which writes a fleet entry but does not arm the pane-died → cs harvest hook. Even if it did, the hook calls `cs harvest`, which is designed to merge a worker's branch on completion — not to revert a Running runtime molecule. |

## The composite gap — the principle the bug illuminates

> *"Plusieurs safeguards qui ne se croisent pas laissent un trou —
> l'addition de mécanismes de défense ne donne pas la défense de leur
> addition."*

Each safeguard is sound for its own scenario. The phantom-runtime
session is a scenario that lives **between** the safeguards:

- It is not a **pre-spawn** failure (S1 misses by timing).
- It is not a **late-progress** failure (S7/S8 miss for absence of the
  progress signal they consume).
- It is not a **single-event** failure (S2 misses by being one-shot).
- It is not a **synchronous-dispatch** failure (S4 misses for absence
  of the Err to roll back).
- It is not a **manually-noticed** failure until the operator types
  `cs purge` or `cs patrol` — there is no autonomous closing.

The bug is the gap *between* the layers, not a flaw in any single
layer. To close it we need a continuous detector, in the runtime loop,
that reads the same observable the patrol scan reads (`tmux
has-session`) but does so on every tick, not only at startup, and that
mutates state when it observes death — not just emits a warning.

## TLA+ — the missing variable

The current runtime spec captures the molecule-lifecycle transitions
but not the *worker-process* lifecycle. The state space implicitly
assumes:

  ∀ m : m.assigned_worker ≠ None ⇒ tmux_alive(m.assigned_worker)

That assumption holds *initially* (the orphan scan checks it once)
but is not preserved as an invariant by any transition the spec
models. To capture this bug class in TLA+ we need:

1. **A new variable** — `WorkerProcess`, with three values: `Alive`,
   `Dead`, `Unknown`. One per assigned worker.
2. **A new transition** — `WorkerDies(w)` — non-deterministic, can
   fire at any time on any Alive worker. This models the real world:
   processes die for reasons cosmon does not control.
3. **A new safety property** — *eventually, every Dead worker leads
   to the molecule being either re-dispatched or terminally collapsed*
   (no permanent pending state with a dead worker).
4. **A new liveness property** — `EventualPaneRecovery`: in any
   execution where a worker dies on a Running molecule, there exists
   a future tick at which the molecule's status is no longer Running
   with that dead worker assigned.

Without these, TLA+ validates a machine that can wedge forever on a
dead worker and call it correct. The spec checks consistency between
its own transitions; it does not check consistency with the
*environment* (the kernel that owns processes).

## The structural fix

The minimum-cost closure is a **periodic in-loop liveness check**.
Concretely: extend the existing `Runtime::run` loop to call
`orphan_scan` every N ticks (not only at startup), and on a definitive
death verdict, transition the molecule from `Running` (with a dead
worker) back to `Pending` (clearing `assigned_worker` and
`session_name`). The frontier reducer then re-surfaces the molecule on
the next tick, and the policy dispatches it again via
`SubprocessExecutor::dispatch`.

This single hook closes the gap because:

- It runs **continuously** (not once at startup) — covers S2's blind
  spot.
- It **mutates state** (not just warns) — covers S2's other blind spot.
- It uses the **same observable** as the existing patrol scan
  (`tmux has-session`) — no new probe, no new failure mode.
- It is **idempotent**: the second time around, if the worker came
  back via some other path, the orphan_scan returns nothing.
- It composes with the existing `LivenessCheck` trait — production
  uses `TmuxLivenessCheck`, tests inject a stub.
- It does not change `frontier::compute_from_molecules`, so every
  other consumer of the frontier remains correct.

This fix corresponds to **Fix A** in the briefing.

### Fix A — code surface

In `crates/cosmon-runtime/src/lib.rs`, inside `Runtime::run`, before
the policy is asked for actions, run an in-loop liveness sweep every
`liveness_recheck_every` ticks:

```rust
if config.liveness_recheck_every.is_some_and(|n| ticks % n == 0) {
    for orphan in orphan_scan(&snapshot, self.liveness.as_ref()) {
        if let Ok(mut mol) = self.store.load_molecule(&orphan.id) {
            if mol.status == MoleculeStatus::Running {
                eprintln!(
                    "ORPHAN RESET: molecule {} session {} died — \
                     resetting to Pending so frontier can re-dispatch",
                    orphan.id, orphan.session_name
                );
                mol.status = MoleculeStatus::Pending;
                mol.assigned_worker = None;
                mol.session_name = None;
                mol.updated_at = Utc::now();
                let _ = self.store.save_molecule(&mol.id.clone(), &mol);
                stamped_any = true;  // force snapshot reload
            }
        }
    }
}
```

A single new field on `RuntimeConfig` (`liveness_recheck_every:
Option<u64>`, default `Some(10)` — every 10 ticks, ≈10s at the default
1s poll) governs how aggressive the recheck is. Tests pass `None` to
disable.

### What about Fix B — invert the auto-detect default?

Fix B in the briefing proposes inverting the auto-detect default:
`cs tackle <id>` would dispatch a leaf by default and require an
explicit `--force-runtime` to take the runtime path. That is appealing
in principle (the workaround `--leaf` already proves leaf-spawning is
correct for this scenario) but it is a **policy** change, not a
mechanism fix:

- It assumes leaf-spawning a DAG root is always preferable to a
  resident runtime. That holds for many one-shot DAGs but breaks for
  multi-wave fleets where the runtime is the *whole point*.
- It would be a breaking UX change for any pilot that relies on the
  current default (cs run autodetect from cs tackle).
- The phantom bug would still exist for anyone who *does* opt into
  `--force-runtime`. Without Fix A, the bug is just relocated, not
  closed.

The decision proposed by this diagnostic: **keep the auto-detect
default; close the gap with Fix A**. The structural mechanism is the
periodic liveness check, not the dispatch-mode default. Fix A makes
the runtime mode safe; the auto-detect can then remain.

If the operator nonetheless wants the explicit-opt-in policy, Fix A is
still required as a defense-in-depth layer for `--force-runtime`. The
order of operations is: Fix A first; revisit Fix B as a separate
deliberation.

### Fix C — TLA+ spec extension

Add to the runtime spec a `WorkerProcess` variable indexed by molecule
id, a non-deterministic `WorkerDies` transition, and the
`EventualPaneRecovery` liveness property. Defer to a follow-up
molecule once the model file is located (current TLA+ assets to be
catalogued in a separate review).

### Fix D — `cs ensemble` 👻 phantom annotation

Already covered structurally by Fix A. Defense-in-depth annotation
deferred to a follow-up molecule (priority moyenne, same as the
parent diagnostic 8e26).

## Acceptance — what this diagnostic delivers

- Inventory of S1–S11 ✅
- Per-safeguard explanation of failure on this case ✅
- Composite-gap principle named ✅
- Missing TLA+ variable identified ✅
- Fix design + minimum surface ✅
- Fix B / Fix C / Fix D positioned as follow-ups, not blockers ✅

The companion implementation of Fix A and the regression test
accompany this diagnostic in the same molecule (911f).
