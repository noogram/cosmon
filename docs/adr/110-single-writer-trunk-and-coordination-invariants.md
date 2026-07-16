# ADR-110 — Single-writer-trunk and coordination invariants

**Status:** Accepted — 2026-05-24.
**Date:** 2026-05-24.
**Authoring task (cosmon-side):** `task-20260524-f1ce`
(formula `task-work`, branch `feat/task-20260524-f1ce-adr-single-writer-trunk`).
**Parent deliberation:** `delib-20260523-a682`
— *« Should cosmon adopt a MISSION + TLA+ + polymerisation framework, or
keep the linear nucleate→tackle→wait→done relay? »* Panel of 5 personas:
wheeler · von-neumann · godel · jobs · torvalds. Verdict: **Option D'** —
*data-structure-first, invariants-named, framework-deferred*.
**Cross-galaxy reference:** [smithy ADR-0019 —
*Orchestration cosmon : single-writer-trunk, invariants nommés,
framework "MISSION" différé*](https://github.com/noogram/smithy/blob/main/docs/adr/0019-orchestration-single-writer-trunk-mission-deferred.md).
smithy *planned* the pattern (vocabulary, sentier, doctrinal
inscription) ; this ADR inscribes the cosmon-side runtime invariants
that the Phase 1 commits already enforce mechanically.

**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — three regimes
  (Inert / Propelled / Autonomous). Single-writer-trunk applies in every
  regime ; it is not regime-specific.
- [ADR-022](022-native-dag-scheduler.md) — native DAG scheduler. **I4
  PROGRESS** is a liveness property of the DAG, not of `cs stitch` alone.
- [ADR-099](099-dispatch-site-stability.md) — dispatch-site stability.
  This ADR does not change the dispatch site ; it constrains *who may
  write* through it.

> **⚠️ Recovery note (2026-05-26).** The three Phase 1 commits below were
> **lost in a branch-wipe** (an internal audit erratum, 2026-05-25):
> on 2026-05-25 *none* of the original SHAs were reachable from `main`, so
> this ADR's central claim ("already enforce mechanically") was **false**.
> The foundation was re-derived from the dangling objects and re-landed by
> `task-20260525-0b25` (Phase 1 recovery): `cs stitch` first
> (commit `3856c00ce`, a364 recovery), then the trunk lock + worktree guard
> + event-fold, **plus** the TLA+ deadlock-free lock order (option 1, see
> below). The original SHAs are kept as historical anchors; the live
> enforcement is the recovery commits referenced in the ops report
> [`docs/ops/2026-05-26-phase1-recovery.md`](../ops/2026-05-26-phase1-recovery.md).
> With the recovery merged, the claim below is **true again**.

**Phase 1 implementation commits:**
- `8655dad5a` (lost; re-derived by `task-20260525-0b25`) —
  `feat(filestore, cli): trunk write-discipline lock + worktree guard`
  (Phase 1 / Commit 1, I1+I2). The re-do adds the **deadlock-free lock
  order** the TLA+ model (`smithy/docs/formal/MCStitch.tla`) proved
  necessary: trunk ⊃ fleet, trunk dropped before the fleet-purge,
  `cs stitch` trunk-only.
- [`1c1270b70`](https://github.com/noogram/cosmon/commit/1c1270b70) —
  `feat(cli): add 'cs stitch <root-id>' — sequential merge under trunk lock`
  (Phase 1 / Commit 2). Recovered on `main` via merge `3856c00ce`;
  switched from the fleet lock to the **trunk lock** by the recovery
  (TLA+ deadlock-free option 1).
- `950986727` (lost; re-derived by `task-20260525-0b25`) —
  `feat(rpp-adapter): surface_freeze() as event-fold (task-20260523-3134)`
  (Phase 1 / Commit 3, I3). Re-derived for the current **29-route** surface
  (the original folded 22): `frozen_api_surface()` projects from the
  append-only `data/surface_events.txt`, no hand-edited count.

---

## Context

Five empirical breakages were observed on the cosmon-server v1.4 drain
(see `delib-20260523-a682/frame.md` §1) :

1. **Cross-worker contamination** of the cosmon `main` checkout — two
   workers writing the same working tree, one stepping on the other's
   staged files.
2. **Naïve human stitcher** (~30 min of bash glue per drain) walking
   completed molecules and merging them by hand, in an order the
   operator had to reconstruct each time.
3. **`surface_freeze` counter non-additive** — concurrent
   read-modify-write on a JSON counter, with last-writer-wins erasing
   intermediate increments.
4. **Spawn timeouts under load** (170+ molecules in flight) on a
   shared checkout — workers blocking on a git index they did not own.
5. **No formal model** for the merge order, even though
   `MoleculeLink::Blocks` already encoded the DAG.

The panel found, unanimously (wheeler / von-neumann / godel / jobs /
torvalds), that the five breakages were **one breakage** — the
absence of a named invariant on **who may write the cosmon main
branch, and how that right is transferred**. wheeler named it
*commit-bus* : the branch is a single-writer channel ; the entire
fleet protocol reduces to *who holds the write token, and how does it
move*. Once named, the five breakages stop being independent
operational annoyances and become five symptoms of one missing
mechanical guard.

The panel also rejected (unanimously) the alternative of adopting a
TLA+-validated *MISSION framework* before fixing the five breakages.
The reasoning (consolidated in smithy ADR-0019 §6) :
- TLA+ on the whole coordination engine costs 30–60 days of
  engineering for cosmon's current maturity (1–2 engineers, weekly
  deliverables) — opportunity cost too high.
- The five breakages have a *mechanical* fix (file locks, worktree
  discipline, event-fold) that ships in ~5 days and yields immediate
  return.
- A formal model can be added *later*, *ciblé* on the scheduler
  (`cs stitch`), if Phase 1+2 prove insufficient. **The mechanical
  fix does not preclude the formal model — it precedes it.**

This ADR inscribes the five invariants (named by wheeler in the
synthesis) that Phase 1 already enforces in code, so future agents
and humans can read the rationale without re-running the panel.

## Options Considered

The panel `delib-20260523-a682` enumerated and weighed four options.
The full per-persona reasoning is in
`.cosmon/state/fleets/default/molecules/delib-20260523-a682/responses/`
and the consolidated trade-off table is in
`delib-20260523-a682/synthesis.md` §3. Summary :

### Option A — Pure operational fix, *no* doctrinal inscription

Ship the three mechanical commits (lock, `cs stitch`, event-fold)
and stop there. No ADR, no named invariants, no shared vocabulary.
**Rejected** (wheeler, godel, von-neumann) : the five breakages
recur because they share an unnamed cause ; without naming the
invariant, the next operator hits the same wall in three weeks.

### Option B — Full TLA+ MISSION framework

Adopt a TLA+-validated *MISSION + équipage + protocole* framework
covering the whole coordination engine, before fixing the five
breakages. **Rejected unanimously (5/5)** : 30–60 engineer-days of
opportunity cost for cosmon's current maturity (1–2 engineers,
weekly deliverables), with no guarantee that the formal model would
catch *these* breakages, which are mechanical rather than
protocol-level.

### Option C — Parallel opt-in *mission* runtime mode

Maintain the existing linear `nucleate → tackle → wait → done` *and*
a new `mission`-mode runtime as a parallel path, operator-selectable
per fleet. **Rejected** (jobs, torvalds) : two execution models
double the bug surface for a minority of cases. The word `mission`
can enter as a doc concept + optional CLI alias without a separate
runtime.

### Option D' — Data-structure-first, invariants-named, framework-deferred *(chosen)*

torvalds' Option D (ship the three commits) enriched with :
- wheeler's invariant-first framing (name I1–I5 *before* the first
  line of code),
- von-neumann's depth stratification (3–4 layers with rising cost
  and explicit conditionality on Phase 3),
- godel's frontier guardrail (operator stays at the boundary — I5,
  non-negotiable by Gödel-2),
- jobs' subtraction discipline (refuse any vocabulary that has no
  named mechanism beneath it — *polymerisation* is cut).

This is the minimum doctrinal load that makes Phase 1 *durable*
without paying for a framework.

## Decision

Cosmon adopts **single-writer-trunk** as the load-bearing invariant
of the fleet, expressed as **five named invariants I1–I5** that bind
the runtime, the CLI, and the operator's relationship to both.

Phase 1 enforces I1–I3 mechanically (re-landed on `main` by the
`task-20260525-0b25` recovery after the branch-wipe — see the Recovery
note above and the erratum). I4–I5 remain load-bearing at the
boundary : the operator stays on the frontier (I5, godel Gödel-2),
the DAG scheduler guarantees liveness (I4, ADR-022). No new framework,
no new daemon, no new primitive — the invariants are inscribed onto
the primitives that already exist (`with_fleet_lock`, the new
`with_trunk_lock`, `MoleculeLink::Blocks`, `DagPolicy::compile_plan`,
worktree creation in `tackle`).

## Invariants

### I1 — WRITER-UNIQUE

At any instant, **at most one worker holds the right to write to the
cosmon `main` branch** (or any equivalent trunk branch of a fleet).
The branch is a *commit-bus* — a single-writer channel ; the entire
fleet protocol reduces to *who holds the write token, and how does it
transfer*.

**Mechanical enforcement** (commit `8655dad5a`) :
- `FileStore::acquire_trunk_lock(cmd_hint) -> TrunkLockGuard` —
  advisory `flock` on `<state_dir>/trunk.lock`, sibling of
  `fleet.lock`.
- `with_trunk_lock(cmd_hint, f)` closure wrapper — RAII guard,
  releases on every early-return path of the caller.
- Non-blocking probe first ; on contention, reads holder hint
  (pid + cmd) and either blocks (default) or fast-fails when
  `COSMON_TRUNK_LOCK_NONBLOCKING=1`.
- Operations that mutate the cosmon main checkout
  (`cs land` / `cs stitch` / `cs done` merge path) wrap their git
  invocations in `with_trunk_lock`.

**What this is not :** I1 does not forbid concurrent reads, concurrent
writes to *worker branches*, or concurrent writes to *worktrees*. It
forbids two writers on the *same* main-branch checkout at the same
instant.

### I2 — ISOLATION

Each worker writes in a **disjoint worktree** (`.worktrees/<mol>/`,
created at `tackle` time). The merge into the trunk is a *distinct
operation* from the work itself.

**Mechanical enforcement** (commit `8655dad5a`, alongside I1) :
- `cs tackle <mol>` creates `.worktrees/<mol>/` on branch
  `feat/<mol-id>` from `main` (this already existed — see
  `crates/cosmon-cli/src/cmd/tackle.rs:418`).
- `cs evolve` and `cs done` write **on the molecule branch**, never on
  `main`. The merge to main is the *stitcher's* responsibility (I1
  + `cs stitch`).
- Worktree guard refuses writes from a checkout pointed at `main` if
  the molecule has its own branch — surfaces the violation early
  instead of letting it land as silent corruption.

**Mapping to existing primitives :** the worktree is *created* today
but was not *enforced* as the only legitimate write surface before
this ADR. Phase 1 turns a convention into a mechanical guard.

### I3 — ADDITIVE-COUNTERS

Shared counters (`surface_freeze`, fleet-wide telemetry, drain-level
aggregates) are **strictly additive** — either CRDTs (G-Counter,
[@shapiro2011crdt, §3.1.1]) or routed through a single sequencer.
**Never** read-modify-write a JSON field from a worker process.

> **Amended 2026-05-26 (erratum resolution, recommendation #2).** The name
> `surface_freeze` collided two *unrelated* mechanisms. They are separated
> here:
>
> 1. **Compile-time §8p API-surface fold (ADR-080).** *This is what commit
>    `950986727` actually does, re-derived onto `main` by the recovery.*
>    The route list is folded out of the append-only
>    `crates/cosmon-rpp-adapter/data/surface_events.txt`; the surface size
>    is `SURFACE_EVENTS.len()`, never a hand-edited integer. **Shipped and
>    enforced** — see below.
> 2. **Runtime per-molecule `freeze_event.json` counter (breakage #3).** A
>    concurrent runtime counter under `fsync`. **Never implemented**; it is
>    a future shape, not a current claim. Its additive contract (G-Counter /
>    grow-only set) is pinned executably by
>    `crates/cosmon-rpp-adapter/tests/proptest_i3_additive_counters.rs`
>    (task-20260525-6166) so whatever lands next has a target.

**Mechanical enforcement** (compile-time API-surface fold, commit
`950986727`, re-derived by `task-20260525-0b25`) :
- `frozen_api_surface()` projects from `SURFACE_ROUTES`, a compile-time
  fold (`build.rs`) over the append-only `data/surface_events.txt` —
  adding a route is **one append**, never a counter bump.
- The aggregate count is *derived* by `SURFACE_EVENTS.len()` — never
  stored as a mutable scalar nor hand-edited in a test.
- Hand-edited count assertions are forbidden ; tests assert the derived
  count (`surface_length_matches_event_log`), not a hand-written number.

**Why it matters :** before Phase 1, a worker computing
`count = read_json(counters); counters[name] = count + 1; write(counters)`
under concurrent load lost increments because two workers read the
same `count` before either wrote. The event-fold form is
*commutative* and *idempotent* at the storage layer — losing or
re-applying an event does not corrupt the aggregate, only delays its
convergence.

### I4 — PROGRESS

**Every accepted mission progresses or fails explicitly.** No silent
stall. Liveness is guaranteed by the DAG scheduler under weak
fairness on the `tackle` action.

**Mechanical enforcement** (already present in ADR-022,
`DagPolicy::compile_plan` + `next_actions`) :
- `cs run --resident` polls the DAG and dispatches the next runnable
  action ; an action that completes unblocks its successors via
  `MoleculeLink::Blocks`/`BlockedBy`.
- `cs stitch` (commit `1c1270b70`) walks the DAG closure rooted at
  `<root-id>` via `DagPolicy::compile_plan` + Kahn toposort, then
  merges in leaves→root order — guaranteeing progress *toward* the
  root mission as each leaf lands.
- On unrecoverable conflict, `cs stitch` aborts cleanly
  (`git merge --abort`) and surfaces the offending edge ; the failure
  is *explicit*, not a stall.

**What this is not :** I4 does not promise that every action
*succeeds*. It promises that the DAG cannot deadlock silently — it
either advances, or surfaces a named failure point that the operator
(or a higher mission) can act on.

### I5 — OBSERVATION-NEUTRE

**Observation does not mutate state.** `cs observe`, `cs ensemble`,
`cs peek --snapshot` are pure read paths ; they never lock the trunk,
never touch the molecule cache, never advance a state machine.

This is *Gödel-2* applied to the runtime : a consistent system
**cannot prove its own consistency from within**. The operator stays
at the frontier — observing without mutating, judging without being
absorbed by the mechanism. Removing the operator from the loop would
require the runtime to prove its own correctness, which the second
incompleteness theorem rules out for any non-trivial state machine
[@godel1931].

**Corollary :** `cs done` *modifies* state (it merges). Therefore it
**is not observation** — its name lies. The synthesis recommends
renaming it `cs land` (or `cs merge-and-seal`) to expose the activity
of the operation, making it visibly lockable. This rename is
*recommended* by I5 but is **not** a precondition for accepting this
ADR — the lock (`with_trunk_lock`) is already enforced under the
current name.

**What this is not :** I5 does not forbid the operator from acting.
It forbids the *runtime* from claiming to observe while mutating.
The operator may always *choose* to mutate (run `cs land`, edit a
file, abort a merge) — that is a frontier action, not an observation.

## Consequences

### Cost (Phase 1, already paid)

- ~3.5 engineer-days for the three commits and their tests.
- ~25 lines of operator-facing CLI surface (lock hint, `--push`,
  `--cargo-check`, `--dry-run`, `cs stitch` argument).
- One environment variable (`COSMON_TRUNK_LOCK_NONBLOCKING`) for the
  fast-fail mode used in CI / scripts that prefer to surface
  contention rather than block on it.
- No new daemon, no new dispatch site, no new persistent state file
  beyond the lock file (which is recreated each session).

### Benefit (measured against the five breakages)

1. **Cross-worker contamination** — eliminated mechanically by I1+I2.
   A worker whose lock acquisition fails reports it ; it does not
   stamp on the live checkout.
2. **Naïve human stitcher** — replaced by `cs stitch <root-id>`,
   which walks the DAG and merges in topological order under one
   trunk lock. Operator time: ~30 min → ~30 s of supervision.
3. **`surface_freeze` non-additive** — eliminated by I3 (event-fold).
   The counter cannot lose increments because it does not exist as a
   mutable scalar.
4. **Spawn timeouts under load** — reduced (not eliminated) because
   workers no longer wait on the main checkout's git index ; they
   write their own worktree. Residual contention on `state.json` is
   bounded by `with_fleet_lock` and is independent of fleet size.
5. **No formal model** — replaced by **five named invariants**.
   Phase 1 does not give a *machine-checked* model, but it gives a
   *readable* one, and the named invariants are precise enough that
   a TLA+ spec (Phase 3) would have a clear target rather than a
   moving one.

**Rollback:** every Phase 1 commit reverts cleanly. The lock file is
the only new persistent artefact and is safe to delete with the
runtime stopped.

## Phase 3 — conditional

Phase 3 is **conditional on continued breakage** after Phase 1+2 land
and run for ≥2 weeks. The order of escalation, from cheap to costly :

1. **Typestate Rust on worker roles** (~3–5 days) — encode the
   `WorkerState ∈ {Idle, Holds, Releasing}` transition at the type
   level so a misuse of `with_trunk_lock` becomes a compile error.
   Triggered if: a runtime panic on lock state inconsistency is
   observed.

2. **`proptest` on the event bus** (~2–3 days) — fuzz the
   `surface_freeze` event-fold and the `cs stitch` topo-merge with
   random DAGs and random worker schedules ; assert I3 + I4 hold on
   all reachable traces. Triggered if: a regression on additive
   counters or DAG progress is observed in production.

3. **TLA+ ciblé on `cs stitch`** (~2 weeks, amortizable) — formal
   spec of the scheduler protocol only (not the workers), proving
   I1 (mutual exclusion) and I4 (progress under weak fairness)
   under the existing CRDT [@shapiro2011crdt] / Lamport-clock
   [@lamport1978time] assumptions. Triggered if: a coordination bug
   surfaces that the previous two layers do not catch — i.e. a bug
   in the *protocol*, not in the *implementation*.

Each step is independently valuable, independently reversible, and
**none is undertaken speculatively**. The default is to ship Phase 1
+ Phase 2 (this ADR + ADR-NEXT *Mission = Molecule + DecayProduct +
Blocks*) and observe.

> **Phase 3 — executed (2026-05-26).** All three escalations were
> triggered and carried out:
> 1. **Typestate worker roles** — `task-20260525-74c6` (compile-time
>    I1 WRITER-UNIQUE).
> 2. **proptest on the event bus + I3** — `task-20260525-6166`
>    (`proptest_event_bus.rs`, `proptest_i3_additive_counters.rs`).
> 3. **TLA+ ciblé on `cs stitch`** — `task-20260525-98e2`
>    (`smithy/docs/formal/MCStitch.tla`). It found, *preventively*, a
>    3-step **circular-wait deadlock** at the trunk/fleet integration
>    boundary (`cs done` trunk ⊃ fleet vs. a naïve `cs stitch`
>    fleet ⊃ trunk). The fix — `cs stitch` holds the trunk lock **alone**
>    (option 1), `cs done` drops the trunk lock **before** its fleet-purge —
>    is **baked into the Phase 1 recovery** (`task-20260525-0b25`). The
>    model is the non-regression test: `MCStitch.cfg` must stay green,
>    `MCStitchDeadlock.cfg` documents the fault to never commit.

## What this ADR is *not* deciding

- **It does not adopt a TLA+ framework** for the whole coordination
  engine. Refused unanimously by the panel for cosmon's current
  maturity. Phase 3 may revisit, *ciblé* only.
- **It does not introduce a `mission` runtime mode** parallel to the
  linear `nucleate → tackle → wait → done`. Refused by jobs (D1
  in synthesis) : maintaining two execution models doubles the
  surface of bugs to serve a minority of cases. The word `mission`
  enters via doc + optional CLI alias (`cs mission <name> tackle` =
  `cs run <root> --resident`), not via a separate runtime.
- **It does not enshrine the word *polymerisation***. Refused by
  wheeler + jobs : the monomer / catalysis / homopolymer metaphor
  does not apply to a cosmon DAG (heterogeneous, ramified, role-
  differentiated). Zero-loss replacement : *auto-chaining* or *DAG-
  driven enchainment*.

## References

### Internal (relative to `/srv/cosmon/cosmon/`)

- `crates/cosmon-filestore/src/lib.rs` — `with_fleet_lock`,
  `with_trunk_lock` (commit `8655dad5a`).
- `crates/cosmon-cli/src/cmd/stitch.rs` — `cs stitch` implementation
  (commit `1c1270b70`).
- `crates/cosmon-cli/src/cmd/tackle.rs:418` — worktree creation per
  molecule (pre-existing).
- `crates/cosmon-runtime/src/dag_policy.rs` — `DagPolicy::compile_plan`,
  `next_actions` (pre-existing, used by I4).
- `crates/cosmon-core/src/interaction.rs:212-280` —
  `MoleculeLink::Blocks` / `BlockedBy` / `DecayProduct` (pre-existing,
  the DAG edges that I4 walks).
- `.cosmon/state/fleets/default/molecules/delib-20260523-a682/synthesis.md`
  — panel synthesis (5 convergences C1–C7, divergences D1–D4,
  surprising insights S1–S5).
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — autonomy
  regimes (binds I1 to every regime).
- [ADR-022](022-native-dag-scheduler.md) — native DAG scheduler
  (binds I4).
- [ADR-099](099-dispatch-site-stability.md) — dispatch-site stability
  (binds the *write* side of I1).

### External (citekey conventions)

- `[@lamport1978time]` — Lamport, *Time, Clocks, and the Ordering of
  Events in a Distributed System*, CACM 21(7), 1978. Foundation for
  ordering on a single-writer channel.
- `[@shapiro2011crdt, §3.1.1]` — Shapiro et al., *A Comprehensive
  Study of Convergent and Commutative Replicated Data Types*, INRIA
  RR-7506, 2011. G-Counter is the reference shape for I3.
- `[@godel1931]` — Gödel, *Über formal unentscheidbare Sätze der
  Principia Mathematica und verwandter Systeme I*, Monatshefte für
  Mathematik 38, 1931. Second incompleteness theorem — load-bearing
  for I5.
- `[@armstrong2003erlang]` — Armstrong, *Making Reliable Distributed
  Systems in the Presence of Software Errors*, KTH thesis 2003. OTP
  supervision trees as the established family this ADR sits inside.
- `[@lamport2002]` — Lamport, *Specifying Systems*, Addison-Wesley
  2002. Reference for Phase 3 TLA+ work if triggered.

### Cross-galaxy

- [smithy ADR-0019](https://github.com/noogram/smithy/blob/main/docs/adr/0019-orchestration-single-writer-trunk-mission-deferred.md)
  — *Orchestration cosmon : single-writer-trunk, invariants nommés,
  framework "MISSION" différé*. Plans the pattern ; this ADR
  inscribes the runtime invariants.
