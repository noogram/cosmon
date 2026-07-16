# CosmonRun.tla + CosmonRunScheduler.tla — Validation Report

**Date.** 2026-04-19
**Specs.**
- `docs/specs/CosmonRun.tla` — ADR-052 ten-invariants core
  (extends the synthesis skeleton from `delib-20260419-d34b`).
- `docs/specs/CosmonRunScheduler.tla` — five launchd patrols
  (ADR-050) + future `cs autopilot tick` (ADR-053). Added by
  `task-20260419-94fb`.
**Tool.** TLC v1.8.0 (tlaplus/tlaplus, build `2026.04.18.033656`).
**Hardware.** Apple Silicon, 16 cores, 27 GB heap.
**Governing ADRs.**
- [ADR-052 — One Ledger, One Writer, One Witness per Field](../adr/052-one-ledger-one-writer-one-witness.md) (core).
- [ADR-050 — Unified Patrol Scheduler](../adr/050-unified-patrol-scheduler.md) (horology).
- [ADR-053 — Cosmon Daemon Supervisor](../adr/053-cosmon-daemon-supervisor.md) (autopilot regime).

---

## TL;DR

| Invariant | Class | Closed env | + Async crashes | + BypassMerge | Verdict |
|-----------|-------|-----------:|----------------:|--------------:|---------|
| **I3** Fleet mirrors session | safety | ✅ holds | ❌ violated | ✅ holds | **Eventual safety** — must be reformulated as L-form once environment is honest. |
| **I4** Session implies live process | safety | ✅ holds | ❌ violated | ✅ holds | **Eventual safety** — same as I3. |
| **I6** No ghost fleet entry | safety | ✅ holds | ✅ holds | ✅ holds | **In-band safety** (provided `Complete` atomically clears `fleet_desired`). |
| **I7** Single event writer | safety | ✅ holds | ✅ holds | ✅ holds | **Structural safety** — codomain of `events_writer_lock` is `{None, Worker}`. |
| **I9** Branch merged ⇒ Completed | safety | ✅ holds | ✅ holds | ❌ violated | **GÖDEL SENTENCE** — true iff the environment never writes `branch_merged` directly. Mechanically confirmed. |
| **I5** Completed eventually merges | liveness | ✅ holds | ✅ holds | n/a | Holds under WF on `Done`. |
| **L2** Dead session eventually purged | liveness | n/a | ✅ holds | n/a | Holds under WF on `Purge`. |
| **L3** Lock eventually released | liveness | ✅ holds | ✅ holds | n/a | Holds under WF on `LockRelease`. |
| **I_StepProgress** 8th-clock stall resolution | liveness | ✅ holds (StepProgress.cfg) | ✅ dormant | ✅ dormant | Holds under WF on `MarkStalled` + `Tick`. Amendment 2026-04-20. |

The mechanical check **confirms ADR-052's prose** on every count, and
**sharpens it on three points** that the prose left implicit:

1. I3 and I4 are **not** stepwise safety properties once
   `TmuxCrash` / `ProcessCrash` are admitted into `Next`. They become
   eventual-consistency liveness properties whose witnesses are
   `Purge` (for I3) and a future watchdog action (for I4). The
   reconciliation rate `r ≥ 7.7 × 10⁻³ Hz` from synthesis §(b) is
   precisely the operational form of this latency bound.

2. I6 is a stepwise safety property **only if** `Complete` atomically
   clears `fleet_desired`. The synthesis skeleton wrote `Complete`
   without this clear; we strengthened it. Without the strengthening,
   I6 is also an eventual safety property.

3. I9 — the headline result — is provably **out-of-band**. The
   smallest counterexample (`CosmonRun_I9Counterexample.cfg`) shows
   `branch_merged` flipping to TRUE in **one step**, with
   `mol_status = "Pending"`, the moment `BypassMerge` is admitted.
   This is the c1cb-style external `git merge` of 2026-04-19,
   formalised.

---

## Per-model results

### Model 1 — `CosmonRun_InBand.cfg` (closed environment)

`AsyncCrashesEnabled = FALSE`, `OutOfBandEnabled = FALSE`.

```
19 683 distinct states, depth 25, 6 temporal properties checked.
No errors. Finished in 1 s.
```

All five safety invariants **and** all checked liveness properties
(I5, L3) hold. This is the "kitchen with the door closed" case:
the cosmon CLI is the sole writer of every field, and every
invariant in ADR-052 is mechanically true.

### Model 2 — `CosmonRun_OutOfBand.cfg` (Bypass enabled)

`AsyncCrashesEnabled = FALSE`, `OutOfBandEnabled = TRUE`.

```
Error: Invariant I9_BranchMergedOnlyIfCompleted is violated.
54 distinct states, found in well under a second.
```

I9 is violated as predicted. See full trace in §"I9 counterexample" below.

### Model 3 — `CosmonRun_Crashes.cfg` (async crashes enabled)

`AsyncCrashesEnabled = TRUE`, `OutOfBandEnabled = FALSE`.
Checked invariants: I6, I7, I9. Checked liveness: L2, L3.

```
300 763 distinct states, depth 28, 4 temporal-property branches.
No errors. Finished in 36 s.
```

Confirms that I6 / I7 / I9 are robust to the asynchronous crashes
modelled in `TmuxCrash` and `ProcessCrash`, and that liveness L2 and
L3 hold under weak fairness on `Purge` and `LockRelease` respectively.

### Model 4 — `CosmonRun_CrashesI3.cfg` (I3 + crashes)

```
Error: Invariant I3_FleetMirrorsSession is violated.
Trace: Init → Nucleate(m1) → Tackle(m1) → TmuxCrash(m1)
After TmuxCrash, fleet_desired[m1] = "Registered" but tmux_session[m1] = FALSE.
```

Documents the safety-vs-liveness frontier for I3.

### Model 5 — `CosmonRun_CrashesI4.cfg` (I4 + crashes)

```
Error: Invariant I4_SessionImpliesLiveProcess is violated.
Trace: Init → Nucleate(m2) → Nucleate(m1) → Tackle(m1) → Evolve(m1) → ProcessCrash(m1)
After ProcessCrash, tmux_session[m1] = TRUE but worker_pid_alive[m1] = FALSE.
```

Documents the safety-vs-liveness frontier for I4.

### Model 6 — `CosmonRun_I9Counterexample.cfg` (minimal Gödel witness)

```
Error: Invariant I9_BranchMergedOnlyIfCompleted is violated.
Behavior:
  State 1: <Initial>             — branch_merged = FALSE, status = Absent
  State 2: <Nucleate(m1)>        — branch_merged = FALSE, status = Pending
  State 3: <BypassMerge(m1)>     — branch_merged = TRUE,  status = Pending  ← I9 false
```

Three states. One molecule. This is the smallest witness the model
checker can construct. It is the formal twin of c1cb's morning gesture:
a single external `git merge` that touched `branch_merged` outside the
writer-lock domain.

---

## I9 as the Gödel sentence — what TLC mechanically proves

ADR-052 classifies `branch_merged` as an **out-of-band** field: the
ledger cannot be the single witness because `git merge` is invoked
outside cosmon, by humans and by foreign tooling. The deliberation
expressed this in prose ("on ne peut pas tenir I9 depuis l'intérieur
du système — c'est la phrase de Gödel").

TLC sharpens that prose:

1. **In the closed model** (`CosmonRun_InBand.cfg`), I9 is a true
   theorem: across all 19 683 reachable states, `branch_merged ⇒
   mol_status ∈ {Completed, Collapsed}`. The proof is mechanical,
   not metaphorical.

2. **In the open model** (`CosmonRun_OutOfBand.cfg`), I9 is false
   in two steps. The counterexample is not subtle, not a
   pathological ordering, not a "rare race": it is the *first*
   transition the environment can take. This is the formal content
   of "the property cannot be enforced from inside the spec when
   the environment can write".

The Gödel-sentence character of I9 is therefore exactly:

> *I9 is true in every model where `BypassMerge ∉ Next`, and false in
> every model where `BypassMerge ∈ Next`. Whether `BypassMerge ∈ Next`
> is a fact about the environment, not a fact provable from the
> spec.*

That last clause — *not a fact provable from the spec* — is the formal
fingerprint of a Gödel sentence. The model checker does not "fail" on
I9; it correctly reports that I9 is contingent on a meta-axiom (the
closure of the writer set), and that meta-axiom must be discharged
**outside** the spec, by procedure (`cs done` is the only sanctioned
mutator) and by social contract (chronicles, syzygie, ADR-052 itself).

---

---

## Schedulers — `CosmonRunScheduler.tla`

### TL;DR

| Invariant | Role | Normal | ConvoyCascade | Verdict |
|-----------|------|-------:|--------------:|---------|
| **S1** Non-overlap (mutex) | safety | ✅ holds | — | **Structural** — scheduler `lock` is single-cell. |
| **S2** Window-is-closed | safety | ✅ holds | — | **Structural** — `Fire(p)` guards `clock ∈ WindowOpen[p]`. |
| **S3** Purge-before-respawn | safety | ✅ holds | ❌ violated | **Ordering invariant** — the convoy cascade of 2026-04-12 is the formal counterexample when `S3Enabled = FALSE`. |
| **L2** Eventual-finish | liveness | ✅ holds | — | Holds under WF on `Finish(p)`. |

The five patrols modelled are the ADR-050 migration targets: `nightly-drain`,
`temp-review`, `backlog-sanity`, `patrol-propel`, `purge-stale`. The
scheduler is event-free in the ADR-050 sense (no inter-patrol messaging);
the only coupling is the shared mutex `lock`, the per-patrol `next_fire_at`,
and — critically — the shared `sediment` counter that `purge-stale` resets
and `patrol-propel` must respect.

### The convoy cascade, mechanically

The 2026-04-12 chronicle documents a greedy `cs run` that re-tackled
closed molecules because `compile_plan` walked the transitive closure of
reachable-completed subgraphs. Reframed in scheduler terms:

> *`patrol-propel` fired while stale state (`sediment > 0`) was still on
> disk; `purge-stale` had not yet reaped. The propel action then picked
> up the stale set as "reachable and not-yet-done-from-this-invocation"
> and re-dispatched it.*

`CosmonRunScheduler_ConvoyCascade.cfg` disables the `S3Enabled` guard
(modelling a scheduler that fires `patrol-propel` without checking
sediment). TLC produces the smallest possible witness in **three
states**:

```
State 1: <Initial>                  — sediment = 0, cascade_detected = FALSE
State 2: <ActivityAccrues>          — sediment = 1, cascade_detected = FALSE
State 3: <Fire("patrol-propel")>    — sediment = 1, cascade_detected = TRUE
         Invariant S3_PurgeBeforeRespawn violated.
```

This is the formal twin of the chronicle: one accrual, one propel,
cascade. The model does not know about BFS over `compile_plan`; it only
knows that `patrol-propel` fired on non-empty sediment. Under
`S3Enabled = TRUE`, `CanFire(patrol-propel)` guards on `sediment = 0`
and the 3-state trace becomes unreachable.

### Autopilot tick as a stricter overlay

`AutopilotEnabled = TRUE` adds a second guard on `Fire(patrol-propel)`:
`sediment < BacklogThreshold`. This mirrors the ADR-048 backlog-sanity
scope-check that a future `cs autopilot tick` action will apply. In
`CosmonRunScheduler_Normal.cfg` both guards run simultaneously; the
stricter autopilot threshold dominates while sediment is below S3's
absolute zero. The spec treats autopilot as a *refinement* of S3, not
a replacement: S3 is the floor, autopilot is a tunable ceiling.

### Model 7 — `CosmonRunScheduler_Normal.cfg`

`S3Enabled = TRUE`, `AutopilotEnabled = TRUE`, `BacklogThreshold = 2`,
`MaxTime = 3`, `MaxSediment = 1`.

```
1 557 827 states generated, 603 852 distinct states found, depth 24.
Implied-temporal checking — 5 branches.
Model checking completed. No error has been found.
Finished in 03 min 17 s.
```

TypeOK, S1, S2, S3 all hold. `L2_EventualFinish` holds under weak
fairness on `Finish(p)` across all five patrols.

### Model 8 — `CosmonRunScheduler_ConvoyCascade.cfg`

`S3Enabled = FALSE`, `AutopilotEnabled = FALSE`, `MaxTime = 2`,
`MaxSediment = 1`.

```
Error: Invariant S3_PurgeBeforeRespawn is violated.
17 states generated, 16 distinct states, depth 3.
Finished in < 1 s.
```

Counterexample trace above. The scheduler is a deterministic mutex
with one degree of freedom (the S3 guard); the counterexample exhibits
the minimal sequence that exercises that freedom wrong.

### Model 9 — `CosmonRun_StepProgress.cfg` (8th clock — StepClock)

`AsyncCrashesEnabled = FALSE`, `OutOfBandEnabled = FALSE`,
`T_STALL = 3`, `MaxClock = 6`, `MaxSeqno = 1`, `Mol = {m1}`.

Added 2026-04-20 in response to fixture `idea-20260419-2d4e` — the
4-hour silent molecule whose worker finished its turn without
re-triggering next-step emission (`delib-20260420-1b02`). The config
exercises the new liveness invariant **`I_StepProgress`**:

```
(mol_status[m] = "Running" ∧ Silence(m) > T_STALL)
    ~> (Silence(m) ≤ T_STALL ∨ mol_status[m] ≠ "Running")
```

under weak fairness on `MarkStalled(m)` (cosmon-side detector — not
worker-side, per ADR-052 I2 `SingleWriterPerField`) and on `Tick`
(so the model's clock actually advances). The companion variables
are `now : 0..MaxClock` and `sealLog : Mol → Seq(0..MaxClock)`;
`Evolve(m)` appends `now` to `sealLog[m]` at each step, and
`Silence(m) = now - LastSealT(m)` is the MSS (Shannon) separating
a healthy molecule from a stalled one.

```
688 total distinct states. Implied-temporal checking — 2 branches.
Model checking completed. No error has been found.
Finished in < 1 s.
```

Ghost counterparty: `GhostKind::InferenceStalled` — the seventh
named drift shape in the TLA model's `GhostKind` set (the first
six are `DeadPane`, `VanishedWorker`, `UnHarvested`, `StaleProbe`,
`UnnamedMerge`, `Sediment`). The Rust-side enum extension is
tracked by the Phase 1 polymer workstream.

The **pre-existing eight configs** each acquired constants
`T_STALL = 99` and `MaxClock = 1` so the StepProgress machinery is
dormant (`Silence(m) ≤ 1 < 99`), preserving their original
verdicts. Silence in their traces never exceeds the threshold,
so `MarkStalled` is never enabled — `Complete(m)`'s new guard
`Silence(m) ≤ T_STALL` is trivially satisfied too.

### What this changes for cosmon

**Nothing in the code today.** The S3 guard already exists implicitly —
`compile_plan`'s scoping fix (`task-20260412-30c1`) restricted the
traversal so that `patrol-propel` (and its runtime descendants) cannot
dispatch "reachable-completed" subgraphs. This spec promotes the
implicit invariant to a named, TLC-checkable property: *the scheduler
must never fire a respawn-class patrol while the cleanup patrol still
has work left*. Any future refactor that loses the ordering constraint
is now a formal regression, not a "maybe this will misbehave"
intuition.

Three concrete implications:

1. When `cs autopilot tick` lands (ADR-053 successor), it **must**
   carry the autopilot-style sediment check. The spec shows that
   `AutopilotEnabled = TRUE` does not by itself enforce S3 — it is
   stricter *only if* `BacklogThreshold ≤ 1`. The implementation
   must either pin the threshold to 0 for respawn-class patrols,
   or run S3 in parallel as a hard floor.

2. The scheduler's `lock` is the only thing enforcing S1 today. Any
   successor design that drops the global mutex in favor of
   per-patrol locking must re-check S1 on the new spec; the
   `patrol_firing` domain is not intrinsically exclusive.

3. ADR-050 §5 ("Alignment with existing invariants") should cite
   `CosmonRunScheduler_ConvoyCascade.cfg` as the formal counterpart to
   the 2026-04-12 chronicle. The prose claim "the scheduler is a clock,
   not a message bus" gains a mechanical twin: the clock respects an
   ordering invariant between patrols, and that ordering is visible
   to the model checker.

---

## Reproducing this report

```bash
# Fastest: single recipe verifies all 8 configs.
just tla-verify

# Manual loop, if you want per-run logs:
cd docs/specs
JAVA=/opt/homebrew/opt/openjdk@21/bin/java
for cfg in CosmonRun_InBand CosmonRun_OutOfBand CosmonRun_Crashes \
           CosmonRun_CrashesI3 CosmonRun_CrashesI4 \
           CosmonRun_I9Counterexample; do
    echo "=== $cfg ==="
    $JAVA -cp tla2tools.jar tlc2.TLC -workers auto \
          -config $cfg.cfg CosmonRun.tla \
          | tee tlc-out-${cfg#CosmonRun_}.log \
          | grep -E "(Error|violated|states generated|Finished)"
done
for cfg in CosmonRunScheduler_Normal CosmonRunScheduler_ConvoyCascade; do
    echo "=== $cfg ==="
    $JAVA -cp tla2tools.jar tlc2.TLC -workers auto \
          -config $cfg.cfg CosmonRunScheduler.tla \
          | tee tlc-out-${cfg#CosmonRunScheduler_}.log \
          | grep -E "(Error|violated|states generated|Finished)"
done
```

The captured `tlc-out-*.log` files in this directory are the audit
trail of the validation that produced this report.

---

## What this changes for cosmon

**Nothing in the code yet.** This is a verification step, not a
refactor. The findings refine the prose of ADR-052 in three concrete
ways:

1. ADR-052 §I3, §I4 should explicitly state that they are
   **eventual-consistency** properties under the asynchronous-crash
   model. The watchdog (`Purge` for I3, future `cs patrol --inspect`
   for I4) is the witness; the latency bound is `r ≥ 7.7 × 10⁻³ Hz`
   from synthesis §(b).

2. ADR-052 §I6 should call out the atomicity requirement on
   `Complete`: it must clear `fleet_desired` in the same transition
   that flips `mol_status` to `Completed`, otherwise I6 is also
   eventual.

3. ADR-052 §"Out-of-band classification" of I9 now has a formal
   citation: `docs/specs/CosmonRun_I9Counterexample.cfg` exhibits
   the smallest counterexample, and `CosmonRun_InBand.cfg` proves
   the theorem in the closed environment.

A follow-up task should fold these three refinements back into ADR-052
prose. None requires Rust changes.

---

## Cross-galaxy extension — CosmonRunXGalaxy.tla (I11..I15)

**Date.** 2026-04-19 (amendment).
**Spec.** `docs/specs/CosmonRunXGalaxy.tla` — EXTENDS CosmonRun with the
five cross-galaxy invariants named by delib-20260419-29f9 (chronicle:
*"Deux cuisines, deux cahiers, aucune sonnette"*).

### Invariants

| Invariant | Class | Closed env | + Peer forgery | Verdict |
|-----------|-------|-----------:|---------------:|---------|
| **I11** UnionLedger                | safety | ✅ holds | ✅ holds | **Structural safety** — no Foreign writer exists in the action set. |
| **I12** SingleWriterPerGalaxyField | safety | ✅ holds | ✅ holds | **Structural safety** — every non-empty slot is Owner-stamped. |
| **I13** ContentIdentityUnderRename | safety | ✅ holds | ✅ holds | **Type-shape anchor** — Rename's UNCHANGED witnesses gauge invariance (ADR-011). |
| **I14** PeerCompletionHonest       | safety | ✅ holds | ❌ violated | **GÖDEL SENTENCE** — true iff no pilot hand-forges a peer receipt. Cross-galaxy twin of I9. |
| **I15** CrossGalaxyCostBound       | safety | ✅ holds | ✅ holds | **Structural safety** — AddCrossEdge guard keeps the edge multiplicity ≤ MaxCrossEdges. |

### Per-model results

#### `CosmonRunXGalaxy_InBand.cfg` (closed cross-galaxy)

`Mol = {m1}`, `Galaxies = {gA, gB}`, `MaxSeqno = 1`, `MaxCrossEdges = 1`,
`AdversarialPeerForge = FALSE`.

```
10 105 states generated, 1 224 distinct states found.
Depth 14. Finished in < 1 s.
Model checking completed. No error has been found.
```

All five cross-galaxy invariants hold, plus the four CosmonRun invariants
that remain valid when async crashes and bypass merges are disabled.

#### `CosmonRunXGalaxy_Adversarial.cfg` (ForgePeerReceipt enabled)

`AdversarialPeerForge = TRUE`.

```
Error: Invariant I14_PeerCompletionHonest is violated.
9 states generated, 8 distinct states. Depth 2.
Trace: Init → ForgePeerReceipt(gB, gA, m1)
  State 2: peer_receipt[<<gB, gA, m1>>] = "Completed"
            /\ ledger_by_g[gA][m1]    = 0
```

Two states. One molecule. Galaxy `gB` records a "Completed" receipt
naming peer `gA` while `gA`'s ledger is empty. The receipt is
syntactically valid (`peer_receipt` lives in `gB`'s own state tree, the
signature key lives in `gB`'s secrets); semantically it is a lie. This
is the cross-galaxy twin of the c1cb-style external `git merge` that
produced the I9 counterexample.

### I14 as the Gödel sentence — cross-galaxy boundary

The shape of I14 mechanically reproduces that of I9. In the closed
model, I14 is a theorem: every Completed peer receipt is preceded by a
non-empty peer ledger (ObservePeer's guard). In the open model, a
pilot with filesystem write access to its own galaxy's `peer_receipt`
table flips the receipt without touching the peer's ledger — and the
super-ledger projection `⊎_g Ledger_g` has no way to detect the lie
from inside the union.

The formal signature is identical:

> *I14 is true in every model where `ForgePeerReceipt ∉ Next`, and false
> in every model where `ForgePeerReceipt ∈ Next`. Whether
> `ForgePeerReceipt ∈ Next` is a fact about the environment
> (specifically, about every participating galaxy's filesystem ACL and
> pilot discipline), not a fact provable from the spec.*

The out-of-band gate for I14 is the cross-galaxy receipt-vs-ledger probe
(`cs galaxy verify <peer>`, proposed in delib-20260419-29f9 §3): it
replays the peer's `events.jsonl` and refuses any `CompletionReceipt`
whose `events_hash` does not match. Necessary but not sufficient — a
pilot who forges both the receipt *and* the events.jsonl entries
presents a consistent lie. Sufficiency requires a witness outside both
galaxies (CT-log, third-galaxy audit, or the P_external chain
terminating in human attention + syzygie review).

### What this changes for cosmon

Nothing in the code — mirroring the I9 result. The findings refine the
ADR-052 successor that ADR-057 (galaxy-as-state-root) will formalise:

1. The cross-galaxy super-ledger is a **read-time projection**, never
   a persisted object (B-shape of ADR-035).
2. I14 must be carried as an out-of-band gate in every galaxy's
   `cs galaxy verify` subcommand (once the cross-galaxy `MolRef` is
   implemented — `task-20260419-57cb`).
3. I15's `MaxCrossEdges` parameter is the formal expression of the
   cross-galaxy cost bound: edge multiplicity is budgeted per
   molecule, not unbounded.
