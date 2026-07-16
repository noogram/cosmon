# ADR-138 — Autonomous Runtime: Two-Loop Client-of-Core

**Status:** Proposed — design + spec only. This ADR ratifies the *shape* of
cosmon's autonomous runtime. It does **not** authorise the build; each build
unit is a follow-up molecule (see §12 phased plan). No `just install`.

**Date:** 2026-06-26
**Decider:** Noogram (operator synthesis, 2026-06-26)

**Source deliberations (Phase 0 → Phase 1 lineage):**
- `delib-20260626-ceae`
  — Phase 0: market survey + SOTA mapping + Rust/cosmon-fit verdicts.
  One-sentence finding: *"The autonomous runtime cosmon needs is ~90% already
  on disk."*
- `delib-20260626-d222`
  — Phase 1: design + TLA+ spec panel (von-neumann · torvalds · karpathy ·
  architect · adversary). One-sentence finding: *"Everything is a lift, a
  rename, or a new ~40-line type — except the four RR-5 forensic events,
  which are a hard prerequisite."*

**Builds on (prior art — realize, do not reinvent):**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — three regimes
  (Inert/Propelled/Autonomous), *no daemon in the transactional core*, the
  worker/human-callable perimeter. Inherited vocabulary unchanged.
- [ADR-095](095-resident-runtime-ifbdd-path.md) — RR-1..RR-5 obligations;
  un-retires ADR-016 Phase 3+ under the IFBDD sequencing discipline;
  `resident.rs` / `ResidentScheduler` as the clean ADR-095 path.
- [ADR-137](137-molecule-health-deacon-witness-patrol.md) — `PatrolReport` /
  `PatrolAction` types already in `cosmon-core`; `run_patrol()` and the heal
  verb are specified there but not built. This ADR extends that spec with the
  `--heal` apply-loop and the liveness guard.
- ADR-116 — liveness lease
  pattern (20-min TTL); governs the presence-row stale-latch fix (§9 Divergence C).
- [ADR-038](038-whisper-perturbation-port.md) — `cs whisper`, the 6th channel
  (human-pilot → live-worker advisory text). The heal loop must never override
  a whisper-piloted molecule.
- [`crates/cosmon-runtime/src/resident.rs`](../../crates/cosmon-runtime/src/resident.rs)
  — the clean ADR-095 module containing `ResidentScheduler` (the pure policy
  trait, zero-I/O). This ADR renames it `LoopPolicy` and lifts it to
  `cosmon-core`.
- [`docs/architectural-invariants.md`](../architectural-invariants.md) §8a–§8k.

**Supersedes (partial):** [ADR-016](016-autonomy-regimes-and-resident-runtime.md)
Phase 3+ design sections — replaced by the concrete two-loop specification
below. ADR-016's vocabulary (Inert/Propelled/Autonomous) and the three-regime
table are preserved.

---

## 1. Context

ADR-095 re-opened the Resident Runtime build path after ADR-054 retired it,
under five named invariants (RR-1..RR-5) and an IFBDD construction order.
Two deliberation rounds (Phase 0 + Phase 1) surveyed the 2026 market, mapped
every SOTA pattern onto cosmon's hexagonal Rust primitives, and designed the
concrete runtime. This ADR records those findings as actionable specifications.

The key market finding (Phase 0, unanimous): cosmon sits in an empty fourth
quadrant — stateless CLI over JSON-on-disk, closer to Temporal's
durable-execution model than to LangGraph/CrewAI checkpoint frameworks, with
no server to run. The autonomous runtime must stay in that quadrant.

The key design finding (Phase 1, von-neumann + adversary): **everything is a
lift or a rename except the four RR-5 forensic events**, which are unbuilt
and are a HARD PREREQUISITE. All other items are realizations of specced
primitives.

---

## 2. Decision — Two-Loop Client-of-Core Architecture

The autonomous runtime is two idempotent, one-shot loops driven by an external
OS timer. No daemon, no broker, no resident process.

### 2.1 Outer loop — DAG drain (`cs run`)

`cs run` walks `ready_frontier`, dispatches via `cs tackle`, harvests via
`cs done` (merge-before-dispatch). On each tick it consults a `LoopPolicy`
trait implementation and emits `LoopAction` decisions. The `--supervise` flag
selects a `SuperviseMode` that gates which `ActionClass`es auto-proceed.

**`--supervise` is mutually exclusive with `--resident`.**
These are orthogonal axes: `--supervise` selects how present the pilot is
(which actions need human confirmation); `--resident` selects the loop-engine
variant. Combining them produces an error at CLI parse time.

### 2.2 Health loop — liveness watchdog (`cs patrol --heal`)

`cs patrol --heal` is a new, one-shot, idempotent verb. It calls the pure
`scan()` function (from ADR-137, `cosmon_core::health`), receives a
`HealthReport`, and applies each `PatrolAction` through existing verbs
(`cs stuck`, `cs freeze`, `cs collapse`, etc.). Backoff is derived from
`events.jsonl` — no new state file.

### 2.3 Clock — external OS timer, not a daemon

An OS timer (launchd plist / systemd timer / cron) fires:
- `cs patrol --heal` every τ (liveness tick)
- `cs run <dag-root>` on demand for DAG drain

Retraction = delete the timer file (one file, one operation). Cosmon stays a
one-shot tool; the scheduler is external.

### 2.4 Conductor — port type now, adapter deferred

`ConductorSignal` and `Residual` are new zero-I/O types in `cosmon-core::conductor`
(~40 lines). They define the typed steering channel for the no-human case:
`RefineGoal | Abort | RaiseGuardrail` in, `Residual` out. `Residual` is the
type of entries appended to the existing `runtime-trace.jsonl` — no new file.
`Abort` is advisory: it resolves to `cs collapse` / `cs stuck` / `temp:frozen`,
never `cs done`. The FS-watcher adapter (for the AWAY case) is deferred until
the first unattended Autonomous run exists.

---

## 3. Key Types (crate/module placement)

All new types live in `cosmon-core` (zero-I/O). Shell I/O lives only in
`cosmon-cli` and `cosmon-runtime`. See §11 for the full placement table.

### 3.1 `LoopPolicy` trait (lifted from `ResidentScheduler`)

```rust
// cosmon-core::policy
pub trait LoopPolicy: Send + Sync {
    fn decide(&self, snapshot: &EnsembleSnapshot) -> Vec<LoopAction>;
}

pub enum LoopAction {
    Tackle(MoleculeId),
    Done(MoleculeId),
    Collapse { id: MoleculeId, reason: String },
    Escalate { id: MoleculeId, target: EscalationTarget },
}
```

`ResidentScheduler` in `resident.rs` IS this trait, mislocated. The action:
rename, move to `cosmon-core::policy`, update `resident.rs` to `impl LoopPolicy`.

### 3.2 `SuperviseMode` and `ActionClass`

```rust
// cosmon-core::supervise
pub enum ActionClass { Dispatch, Merge, Terminal }

pub enum SuperviseMode { Grip, Touch, Watch, Away }

impl SuperviseMode {
    pub fn allows(&self, class: ActionClass) -> bool {
        match (self, class) {
            (Self::Touch | Self::Watch | Self::Away, ActionClass::Dispatch) => true,
            (Self::Watch | Self::Away, ActionClass::Merge) => true,
            (Self::Away, ActionClass::Terminal) => true,
            _ => false,
        }
    }
}
```

| Detent | Auto-proceed | Surfaces to pilot |
|--------|-------------|-------------------|
| Grip | nothing | Dispatch + Merge + Terminal |
| Touch | Dispatch | Merge + Terminal |
| Watch | Dispatch + Merge | Terminal |
| Away | Dispatch + Merge + Terminal | nothing (policy guardrails) |

### 3.3 `StepVerdict` and `FailCompact`

```rust
// cosmon-core::verdict
pub enum StepVerdict {
    Pass,
    Fail(FailCompact),
    Inconclusive,
}

pub struct FailCompact {
    pub code: u32,
    pub log_ref: String, // pointer to log range, not raw log
}
```

`StepVerdict` flows through the data plane: it lands on the completed-step
record and as a `StepVerdictRecorded` event in `events.jsonl`, surfaced via
`cs observe --json`. No new `verdict.json` file (would violate RR-2).

### 3.4 `Phi` and `FuelBudget` — the liveness invariant

```rust
// cosmon-core::progress
pub struct Phi(pub u64);        // Σ (steps_remaining + 1) over {Pending, Queued, Running}
pub struct LiveWork(pub u64);   // count of alive-but-not-terminal molecules (vitality)
pub struct FuelBudget(pub u64);

impl FuelBudget {
    pub fn debit(&mut self) -> bool { /* false = budget exhausted */ todo!() }
}
```

**Mandatory correctness constraint (Phi domain):** `Phi` sums ONLY over
molecules whose status is *forward-progress-capable* — `{Pending, Queued,
Running}`. `Queued` (assigned-to-a-worker-but-not-yet-executing) belongs in the
domain even though it is not advancing *this* tick: it is the transient state on
the normal dispatch path `Pending → Queued → Running`, and **excluding it would
break the variant**. If `Queued ∉ domain` while `Running ∈ domain`, the
`Queued → Running` transition *increases* `Phi` — a well-founded variant must
never increase on a forward step. Keeping `Queued` in the domain makes the whole
dispatch path `Phi`-monotone (membership is preserved at every arc; `Phi` only
drops when a molecule leaves through `Completed`/`Collapsed`).

`Frozen` and `Starved` molecules are progress-*incapable* (alive but not
advancing, and not about to); including them keeps `Phi > 0` perpetually on a
blocked backlog, which would make the variant never converge and fire a
guaranteed spurious "stall" on every tick. The `progress_measure` function MUST
filter to `{Pending, Queued, Running}` before summing. `Frozen`, `Starved`, and
`temp:frozen`-tagged molecules contribute 0 to `Phi` and to `FuelBudget` debits.

The pair `(Phi, FuelBudget)` forms a lexicographic well-founded variant on
`ℕ×ℕ`: `Phi` decreases on every forward step; `FuelBudget` decreases on every
retry that does not advance `Phi`. Together they guarantee no spin-without-progress.
(The variant's monotonicity is conditional on a *closed epoch* — no external
injection of work. New `cs nucleate` and `Starved → Running` recovery both
re-raise `Phi` from outside the system, exactly as a classical variant resets
when fresh work arrives; termination is argued per quiescent epoch.)

**Vitality (`L`) is a SEPARATE measure from `Phi` — do not conflate them
(delib-20260626-9825 D4).** `Phi` answers *"is there forward-progress-capable
work the runtime can advance right now?"* It is correctly 0 for a fleet whose
only alive molecules are `Starved` or `Frozen`. But `Phi = 0` does **not** mean
*"the fleet is healthy/quiescent"* — a fleet pinned at `Starved` is alive and
blocked, not idle and done. Reading the green/amber/red vitality light off `Phi`
alone yields a **false GREEN on a starved fleet**: `Phi = 0` ⇒ "nothing to
advance" ⇒ GREEN, when the honest reading is AMBER (alive-but-blocked, the
operator should look).

The fix is a second domain, the **live-work measure `L`** = the count of
*alive-but-not-terminal* molecules, which drives the vitality light:

| Status      | in `Phi` (forward-progress variant) | in `L` (vitality) | vitality contribution | rationale |
|-------------|:-----------------------------------:|:-----------------:|-----------------------|-----------|
| `Pending`   | ✅ | ✅ | GREEN (advanceable) | assignable, will progress |
| `Queued`    | ✅ | ✅ | GREEN→AMBER if stuck | transient; preserves `Phi` monotonicity |
| `Running`   | ✅ | ✅ | GREEN (advancing) | actively progressing |
| `Starved`   | ❌ | ✅ | **AMBER** | externally-imposed block (ADR-062 quota refused) — operator may not know, MUST surface |
| `Frozen`    | ❌ | ❌ (carve-out) | neutral (GREEN) | operator-*intended* hold (`cs freeze`) — a deliberate human pause; surfacing AMBER would be noise |
| `Completed` | ❌ | ❌ | terminal | done |
| `Collapsed` | ❌ | ❌ | terminal | done |

The load-bearing distinction is **externally-imposed stall vs operator-intended
hold**: `Starved` is imposed from outside (a quota provider refused) and the
operator may be unaware, so it drives **AMBER**; `Frozen` is a conscious `cs
freeze` and is therefore silent (it is the documented "contributes 0 to the
vitality signal" case the D4 finding asked us to justify, not include). The
vitality rule, stated once:

> `L = 0` → **GREEN** (quiescent — nothing alive, legitimately idle/done).
> `L > 0 ∧ Phi > 0` → **GREEN** (alive *and* advancing).
> `L > 0 ∧ Phi = 0` → **AMBER** (alive but nothing is forward-progress-capable —
> the starved/queued-stuck fleet that used to read false-GREEN).
> dead/zombie `Running` (ADR-116 lease-expired) → **RED** (health-witness scope).

The single invariant that kills the bug: **`Phi = 0 ∧ L > 0 ⟹ vitality ≠
GREEN`.** The old single-measure reading had no `L`, so `Phi = 0` collapsed
unconditionally to GREEN.

### 3.5 `Authority` phantom and `DoneToken<A>`

```rust
// cosmon-core::authority
pub trait Authority: sealed::Sealed {}
pub struct Human;
pub struct Runtime;
impl Authority for Human {}
impl Authority for Runtime {}

pub struct DoneToken<A: Authority>(PhantomData<A>);
```

**The `Authority` phantom is an in-process documentation aid and correctness
guard, NOT the sole runtime gate for out-of-process calls.**

A rogue process can call `cs done` in a shell script without holding any
`DoneToken`. The actual runtime gate has two layers:
1. **`cs done` perimeter check** — walk-up discovery detects if the caller
   is inside a worktree; if so, refuse (partially built, see RR-1).
2. **RR-5 `RuntimeMergeDispatched` event** — any `cs done` that occurs without
   a preceding `RuntimeMergeDispatched` event in `events.jsonl` is a detected
   ghost-merge. This layer is UNBUILT and is a hard prerequisite (see §4).

Both layers are necessary; neither alone is sufficient. The ADR-016 regime
boundary is encoded by the phantom making "runtime holds a `DoneToken<Human>`"
not compile; the RR-5 events make out-of-process violations observable.

---

## 4. Hard Prerequisite — RR-5 Forensic Events

**The four `Runtime*` event variants MUST land in `EventV2` before any PR
that wires an autonomous dispatch or merge path.**

This is the IFBDD pact (instrument before behaviour) from ADR-095 §RR-5.
The adversary verified from source (`crates/cosmon-core/src/event.rs`) that
these variants are currently ABSENT from `EventV2`.

Required new variants:
- `RuntimeShelledOut { mol_id, verb, timestamp }` — runtime called a `cs` verb
- `RuntimeMergeDispatched { mol_id, authority, timestamp }` — runtime triggered a merge
- `RuntimeReadDecideWrite { mol_id, snapshot_hash, action, timestamp }` — read–decide–write audit
- `RuntimeWorktreeClaimed { mol_id, worktree_path, timestamp }` — runtime claimed a worktree

These events enable:
- Ghost-merge detection (any `cs done` without `RuntimeMergeDispatched`)
- Watchdog heartbeat (`WatchdogHeartbeatMissed` is derivable from last
  `RuntimeShelledOut` + patrol interval)
- Full audit trail for AWAY mode forensics

**No autonomous dispatch or merge PR ships without these events. Period.**

---

## 5. Convergences (Phase 1 panel findings, all decision-grade)

**C1 — `ResidentScheduler` IS `LoopPolicy` mislocated; this is a lift, not a
new design.** (karpathy + torvalds + architect) The pure-policy trait already
exists in `cosmon-runtime/resident.rs`. It takes a snapshot, returns decisions,
does zero I/O. Action: rename + move to `cosmon-core::policy`.

**C2 — Two projections, two tables; `FleetSnapshot` ≠ `EnsembleSnapshot`.**
(torvalds + architect) `EnsembleSnapshot` is the dispatcher's table
(`{id, status, blocked_by}`). The health witness needs `FleetSnapshot`
(`[MoleculeHealthRow]`) with timestamps and liveness booleans. Importing one
for the other inverts the crate dependency edge — kills RR-3.

**C3 — `SuperviseMode` is three consequence classes, not four capability
levels.** (karpathy) `ActionClass { Dispatch, Merge, Terminal }` is what the
dial is a pure step function of. `SuperviseMode::allows(ActionClass) -> bool`
is a 3-line match.

**C4 — `StepVerdict` flows through the data plane; no new `verdict.json`.**
(von-neumann + karpathy) Verdict lands on the completed-step record +
`StepVerdictRecorded` event. No new file, no RR-2 violation.

**C5 — Build the Conductor port now; defer the FS-watcher adapter.**
(architect + torvalds) Ship the ~40-line zero-I/O type; the adapter has nothing
to watch until an actual unattended Autonomous run exists.

**C6 — `(Φ,B)` is sound only if `Phi` is pinned to forward-progress-capable
molecules; vitality is a SEPARATE measure `L`.** (von-neumann + adversary —
MANDATORY correctness fix; amended by delib-20260626-9825 D4) `Frozen` and
`Starved` are alive-and-stalled statuses; including them in `Phi` fires a
guaranteed spurious stall signal on every frozen-backlog tick. Fix: `Phi` sums
only the *forward-progress-capable* domain `{Pending, Queued, Running}`.
**`Queued` belongs in `Phi`** (it was the unstated gap): it is the transient
assigned-but-not-executing step on the dispatch path, and excluding it would let
`Queued → Running` *increase* `Phi`, breaking the well-founded variant.

The D4 amendment closes the dual gap: `Phi = 0` must **not** be read as "fleet
healthy/quiescent". A fleet whose only alive molecules are `Starved` (or stuck
`Queued`) has `Phi = 0` yet is alive-but-blocked — reading the vitality light off
`Phi` alone yields a **false GREEN**. Vitality is driven by a second measure,
the live-work count `L` = alive-but-not-terminal molecules, with
`Phi = 0 ∧ L > 0 ⟹ AMBER` (never GREEN). `Starved` (externally-imposed block)
counts in `L` → AMBER; `Frozen` (operator-intended `cs freeze` hold) is the
documented contributes-0-to-`L` carve-out — a deliberate pause is signal-silent,
not a stall. See §3.4 for the full status-classification table and vitality rule.

**C7 — RR-5 events are a hard prerequisite; build them before any autonomous
dispatch/merge path.** (adversary ground-truth + ADR-095 §RR-5) The four
`Runtime*` variants are unbuilt. Every mitigation for the Authority-phantom
exploit and the watchdog-miss exploit depends on them.

---

## 6. Divergences (resolved)

**Divergence A — Is the `Authority` phantom a compile-time fence or a runtime
fence?** (von-neumann vs adversary) Resolution: both layers needed. The phantom
is an in-process guard (makes accidental violations not compile). The real
out-of-process gate is (a) `cs done` perimeter + (b) RR-5 `RuntimeMergeDispatched`.
The ADR names both; neither alone is sufficient.

**Divergence B — Legacy `DagPolicy` cluster violates RR-1 at crate level.**
(architect finding, uncontested) `dag_policy.rs`, `guard.rs`, `lib.rs`,
`witness.rs` in `cosmon-runtime` import `cosmon_state::StateStore` directly.
This is pre-existing ADR-022 structural debt, not introduced by this design.
Resolution: feature-gate the legacy path behind a `legacy-dag` feature flag;
split into `cosmon-runtime-legacy` (ADR-022 path) and `cosmon-runtime`
(ADR-095 path, RR-1-clean). **Filed as a `temp:warm` bead** — see §10.

**Divergence C — Presence guard needs a TTL.** (adversary finding) A pilot who
disconnects without un-registering leaves a stale presence row. Molecules are
treated as piloted and skipped by `--heal` forever. Resolution: presence rows
need a liveness lease — 20-minute TTL, aligned with ADR-116's liveness-lease
pattern. The `cs patrol --heal` pass must treat rows older than 20 min with
no refresh as absent. This is a concrete constraint for ADR-137's `scan()`
realization, cross-referenced here for completeness.

---

## 7. `--supervise` vs `--resident` — Orthogonal Axes

These flags are **mutually exclusive** and serve different purposes:

| Flag | Axis | What it controls |
|------|------|-----------------|
| `--supervise grip\|touch\|watch\|away` | Presence dial | Which ActionClass decisions auto-proceed vs. surface to pilot |
| `--resident` | Loop engine | Whether `cs run` uses the `RuntimeLoop` (async) or the default sync shell |

Combining `--supervise` and `--resident` in the same invocation is a CLI error.
Rationale: mode confusion (the aviation analogy from Phase 0, jobs persona) is
the #1 UX failure mode — printing the current detent in `cs peek`'s header
is required.

---

## 8. TLA+ Specification

Two skeleton files under `specs/tla/`:

- `CosmonRuntime.tla` — shared VARIABLES + safety properties (Idempotent-Replay,
  Crash-Recovery, Regime-Boundary, Merge-Before-Dispatch)
- `BoundedProgress.tla` — imports `CosmonRuntime`; adds `Phi`, `FuelBudget`,
  `LexLess`, WF fairness, Bounded-Progress liveness property

The Phi domain (`{Pending, Queued, Running}` only — forward-progress-capable)
MUST appear as an explicit invariant in `BoundedProgress.tla`, alongside the
separate live-work measure `L` and the vitality invariant
`Phi = 0 ∧ L > 0 ⟹ ¬GREEN` (§3.4, delib-20260626-9825 D4). See `specs/tla/`
for the skeleton files.

The TLA+ spec and the Rust types are the same theorem in two languages (Phase 1
surprising insight, von-neumann): the `Authority` sealed trait ≙ Regime-Boundary's
inexpressible Runtime-done action; the `(Φ,B)` lexicographic variant ≙
`VariantDecreases` under WF fairness.

---

## 9. Crate/Module Placement Map

| New item | Crate | Module | Zero-I/O? | RR invariant |
|----------|-------|--------|-----------|--------------|
| `LoopPolicy` trait | `cosmon-core` | `policy` (new) | ✅ | RR-1 |
| `LoopAction` enum | `cosmon-core` | `policy` | ✅ | RR-1 |
| `ActionClass` enum | `cosmon-core` | `supervise` (new) | ✅ | — |
| `SuperviseMode` enum | `cosmon-core` | `supervise` | ✅ | — |
| `StepVerdict` + `FailCompact` | `cosmon-core` | `verdict` (new) | ✅ | gates typestate |
| `Phi` + `FuelBudget` | `cosmon-core` | `progress` (new) | ✅ | liveness guard |
| `Authority` trait + `DoneToken<A>` | `cosmon-core` | `authority` (new) | ✅ | RR-1 phantom |
| `FleetSnapshot` + `MoleculeHealthRow` | `cosmon-core` | `health` (new) | ✅ | RR-2 |
| `EventTail<'a>` | `cosmon-core` | `health` | ✅ | — |
| `HealthFinding` + `AnomalyClass` | `cosmon-core` | `health` | ✅ | — |
| `HealthReport` | `cosmon-core` | `health` | ✅ | — |
| `scan()` fn | `cosmon-core` | `health` | ✅ | RR-2 |
| `ConductorSignal` + `Residual` | `cosmon-core` | `conductor` (new) | ✅ | — |
| `ContextSources` + `AssembledContext` | `cosmon-core` | `context` (new) | ✅ | — |
| `ContextAssembler` trait | `cosmon-core` | `context` | ✅ | — |
| RR-5 event variants (4) | `cosmon-core` | `event_v2` (existing) | ✅ | RR-5 |
| `--supervise` flag on `cs run` | `cosmon-cli` | `cmd/run.rs` | I/O shell | — |
| `StepVerdictRecorded` event write | `cosmon-cli` | `cmd/evolve.rs` | I/O shell | — |
| `cs patrol --heal` apply-loop | `cosmon-cli` | `cmd/patrol.rs` | I/O shell | RR-1 |
| `RuntimeLoop` (renamed from `ResidentScheduler`) | `cosmon-runtime` | `resident.rs` | I/O shell | RR-1..RR-4 |
| Conductor FS watcher | `cosmon-runtime` | `resident.rs` | deferred | deferred |
| External OS timer | none | `docs/guides/` | external | RR-2, RR-4 |

**Crate dependency graph (allowed edges only):**
```
                cosmon-core
               (zero-I/O; no deps on sibling crates)
              /             \
    cosmon-runtime       cosmon-cli
    (I/O shell;           (I/O shell;
     no cosmon-state      calls cs verbs;
     in resident.rs;      imports core + state +
     legacy DagPolicy     filestore + runtime)
     is feature-gated)
```

---

## 10. Legacy `DagPolicy` RR-1 Debt — `temp:warm` Bead

**Filed as `temp:warm` follow-up, not blocking this ADR.**

Architect finding (Phase 1): `cosmon-runtime` crate contains two clusters:
- **ADR-022 legacy cluster** (`dag_policy.rs`, `guard.rs`, `lib.rs`,
  `witness.rs`) — imports `cosmon_state::StateStore` directly, violating RR-1.
- **ADR-095 clean cluster** (`resident.rs`) — uses `cs ensemble --json` stdout,
  RR-1 clean.

One-PR structural fix (independent of this ADR's design work):
1. Feature-gate the legacy cluster behind `features = ["legacy-dag"]`.
2. Split into `cosmon-runtime-legacy` (can import state; deprecated) and
   `cosmon-runtime` (RR-1-clean ADR-095 path only).

The `cs run` RR-1 CI test (ADR-095 §2: `cargo tree -p cosmon-runtime
--no-default-features` must not show `cosmon-state` edges) fails today due
to this debt. The legacy-dag feature gate makes the CI test pass without
deleting old code.

---

## 11. `WatchdogHeartbeatMissed` and Independent Evaluator Requirement

For AWAY mode (no pilot at terminal), the stall-witness must have an
independent evaluator and an off-box alarm surface. `WatchdogHeartbeatMissed`
is RR-2-compatible (derivable from last `RuntimeShelledOut` event + patrol
interval in `patrols.toml`), but a watchdog that shares its single point of
failure with the clock it watches provides no isolation. The AWAY design must
include an independent evaluator path — flagged as a constraint on the
AWAY-mode realization molecule, not a blocker for the spec here.

---

## 12. Phased Build Plan

The IFBDD sequencing: instrument before behaviour.

| Phase | Work | Molecule kind |
|-------|------|--------------|
| **P0 (prerequisite)** | RR-5 forensic events (4 `EventV2` variants + serde tests) | task |
| **P1a** | `cosmon-core::health` — `FleetSnapshot`, `scan()`, `Phi`, `FuelBudget` (ADR-137 realization) | task |
| **P1b** | `cosmon-core::policy`, `cosmon-core::supervise`, `cosmon-core::verdict` — lift `LoopPolicy`, add `SuperviseMode`, `StepVerdict` | task |
| **P1c** | `cosmon-core::authority` — `Authority` phantom, `DoneToken<A>` | task |
| **P1d** | `cosmon-core::conductor` — `ConductorSignal`, `Residual` port types | task |
| **P2a** | `cs patrol --heal` apply-loop in `cosmon-cli` | task |
| **P2b** | `--supervise` flag wired on `cs run` in `cosmon-cli` | task |
| **P2c** | `StepVerdictRecorded` event write in `cs evolve` | task |
| **P3** | `RuntimeLoop` rename + integration tests in `cosmon-runtime` | task |
| **P4** | External OS timer docs + launchd template | task |
| **debt** | Legacy `DagPolicy` feature-gate split (Divergence B) | task, temp:warm |

No P2+ work ships before P0 completes.

---

## 13. Coherence Checklist

1. **Stateless?** ✅ — `cs run` and `cs patrol --heal` are both one-shot.
2. **Idempotent?** ✅ — applying the same `HealthReport` twice = once.
3. **Regime-aware?** ✅ — `SuperviseMode` gates transition to Autonomous.
4. **Single perimeter?** ✅ — `cs run` owns DAG drain; `cs patrol --heal` owns liveness.
5. **Symmetric undo?** ✅ — delete the OS timer to retract the clock.
6. **Runtime-compatible?** ✅ — the two-loop shape IS the resident runtime.
7. **Worker/human boundary respected?** ✅ — workers use `cs evolve`/`cs complete` only; `cs done` perimeter blocks worker self-destroy.
8. **Write-read asymmetry preserved?** ✅ — `scan()` is pure read; apply-loop is pure write; no combined RW command.
9. **Merge-before-dispatch respected?** ✅ — `cs done` (merge) called before dispatching dependents.
10. **CLI-first for workers?** ✅ — workers use walk-up `cs` verbs, not MCP.

---

## 14. Relationship to ADR-016 and ADR-095

This ADR is the concrete design realization that ADR-095 §"Successor ADR" called
for. It keeps ADR-016's vocabulary (three regimes, command perimeters, the
two-layer model) and fills in the Phase 3+ design that ADR-054 had retired.
ADR-016 Phase 3+ is now un-retired and specified concretely; ADR-054's
load-bearing inheritances (Markov property, one-layer-of-truth, `cs harvest`
cure) are preserved.
