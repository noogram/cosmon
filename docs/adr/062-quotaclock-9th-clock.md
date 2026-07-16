# ADR-062 — QuotaClock: the 9th clock for subscription-quota exhaustion

**Status:** Proposed (2026-04-22)
**Scope:** `I_QuotaProgress` liveness invariant, the `QuotaClock`
(the 9th clock — wheeler), the `GhostKind::QuotaExhausted` variant
(8th GhostKind), the `MoleculeStatus::Starved` terminal state, the
`CollapseCause::RateLimit` enum, the `ComputeReservoir` Rust trait
(deferred until measurement-trigger), the cross-galaxy `RefreshEvent`
schema contract with market-agents, the staged delivery sequence,
and the **mandatory 12-month retraction clause**.

**Parent deliberation:** `delib-20260422-0101`
— 7-persona panel (knuth, wheeler, hawking, shannon, feynman,
turing, forgemaster). Per-persona responses under
`responses/`.
Companion artifacts: `quotaclock-spec.md` (formal TLA+),
`clock-compose-diagram.md` (the 9 clocks together),
`implementation-plan.md` (staged delivery + measurement),
`extension-currencies.md` (cross-currency generalization),
`anti-goals.md`, `retraction-clause.md`.

**Fixture:** K3 (`delib-20260421-8637`) — the 2026-04-21
*"Claude usage limit reached"* mid-flight halt. Eight clocks, zero
fires. The empirical proof-of-need.

**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — regime
  vocabulary; predictive rotation deferred to Resident Runtime
  (Phase 3).
- [ADR-043](043-provider-abstraction.md) — `Quota` is already a
  vocabulary primitive at the provider layer; this ADR *elevates*
  it to invariant status (wheeler §4.1).
- [ADR-052](052-one-ledger-one-writer-one-witness.md) — the ten
  named invariants. This ADR adds an 11th invariant
  (`I_QuotaProgress`) and one more clock. Single-line patch to
  `MarkStalled` per knuth §4.1.
- [ADR-055](055-cosmon-residence.md) — the deferred predictive
  rotation lives in residence, not in the transactional core.
- [ADR-058](058-step-progress-invariant.md) — `StepClock` and
  `I_StepProgress`. The 9th clock composes with the 8th via the
  `Fundable` conjunct on `MarkStalled`.

---

## 1 · Context

### 1.1 · The K3 fixture

On 2026-04-21, worker K3 in deliberation `delib-20260421-8637` (in
the ivan galaxy) halted mid-flight on
*"Claude usage limit reached"* — the Claude Max 5-hour rolling cap on
the `you` account. The pane was alive, the worker process was
alive, the sealLog had a fresh entry from earlier in the step.
**None of the eight clocks fired.**

The operator eventually diagnosed the halt by reading the pane
manually, waited for the cap to reset, and re-tackled. The molecule
completed at 04:35 UTC the next morning. The cost was **operator
diagnosis time** plus the **risk of the wrong reflex** — the
operator's first instinct on seeing a stalled worker is `cs whisper`
(re-prompt), which against a rate-limited account produces a second
*"usage limit reached"* response, burning more tokens against the
already-throttled account.

### 1.2 · Why the eight pre-existing clocks do not cover it

| Clock | What it watches | Fires on K3? |
|---|---|---|
| 1. DAG-bit | predecessor done / not | No — predecessors completed |
| 2. fs mtime | file changed | No — files unchanged |
| 3. tmux heartbeat | pane alive | No — pane alive |
| 4. events.jsonl + flock | append-ordering | No — no contention |
| 5. git | commit landed | No — no merges pending |
| 6. archive | molecule frozen | No — not frozen |
| 7. RunState (I3/I4) | intent vs witness | No — fleet consistent |
| 8. StepClock (ADR-058) | seal cadence | No — recent seal exists |

The eight clocks watch **cosmon's own filesystem and processes** plus
**worker α-emission**. None of them watches **the layer above α — the
authority that grants α the right to emit at all**.

### 1.3 · The structural insight (wheeler)

> *Cosmon's first 8 clocks listen for **silence**. The 9th clock
> listens for **"no"**. That is the entire generalization.*

The first 8 clocks are mute-pessimistic: assume alive until silence
proves otherwise, then probe. The 9th clock makes the apparatus
**also speech-pessimistic**: when the upstream authority *speaks
the word "no"*, treat it as a first-class signal.

This is the only clock whose **input lives outside cosmon's
filesystem entirely** — it reads
`~/.market-agents/events/refresh.jsonl` through a documented schema,
and it never imports market-agents code.

---

## 2 · Decision

### 2.1 · The vocabulary (wheeler §1, §3, §4)

| Layer | Name |
|---|---|
| The clock | **`QuotaClock`** |
| The invariant (safety) | **`I_QuotaMonotone`** (one-sided accounting) |
| The invariant (liveness) | **`I_QuotaProgress`** (leads-to, oracle-driven) |
| The composition invariant | **`I_AnyHaltCausalityClassified`** |
| The ghost | **`GhostKind::QuotaExhausted`** (8th `GhostKind`) |
| The terminal state | **`MoleculeStatus::Starved`** (peer of `Stalled`) |
| The worker action | **`ReportConsumption`** |
| The environment action | **`RefreshQuota`** |
| The observer action | **`MarkStarved`** |
| The TLA variable | **`quotaLog`** |
| The Rust trait | **`ComputeReservoir`** (forgemaster's name; not "TokenBudgetProvider") |
| The Rust value type | **`ReservoirLevel`** |
| The Rust enum | **`ReservoirKind`** |
| The new crate | **`cosmon-budget`** (feature-gated) |
| The cross-galaxy schema | **`RefreshEvent v1`** |
| The CLI command | **`cs budget`** (read-only) |
| The CLI flag | **`cs collapse --cause rate_limit`** (minimum viable) |
| The failure-event tag | **`CollapseCause::RateLimit { account, kind }`** |
| The Greek-letter layer | **ω** — external authority, upstream of α |

Hard collisions refused: never `LedgerClock`, `FuelClock`, `GasClock`,
`TokenClock`, `WalletClock`, or `CapClock` (rationale in
`anti-goals.md` §4).

### 2.2 · The TLA+ formalism (knuth + turing)

The full formal proposal lives in `quotaclock-spec.md`. The
load-bearing pieces:

**Single new variable** `quotaLog : Seq([account, currency, t, kind,
n, ceiling])` — append-only, projection of `events.jsonl` filtered to
`QuotaConsumed | QuotaRefreshed | QuotaReplyObserved` records. Honors
ADR-052 I1 (SingleLedger).

**Three new actions:**
- `ReportConsumption(m, n)` — claudion-written (not worker-written),
  observation of inference billing. I2 discipline.
- `RefreshQuota(c, a)` — environment action, written by market-agents
  refresh-worker. From cosmon's perspective an environment action like
  `TmuxCrash`.
- `MarkStarved(m)` — cosmon-side observer labelling. Disjunctive
  antecedent: `QuotaReplyExhausted` (oracle, authoritative) OR
  `Headroom = 0` (cosmon-only, defensive).

**Modified actions:**
- `Evolve(m)` and `Complete(m)` gain `Fundable(m)` guard.
- `MarkStalled(m)` gains `Fundable(m)` guard — **the single-line
  patch** (knuth §4.1) that makes `Stalled` and `Starved` fire on
  disjoint configurations.

**Two new invariants:**
- `I_QuotaMonotone` (safety, one-sided): cosmon-side `ConsumedSince`
  bounded by `Ceiling + MaxConsumed` (overshoot tolerance for
  pre-billing impossibility).
- `I_QuotaProgress` (liveness): leads-to with the oracle-observable
  antecedent `~Fundable(m)` — turing §2.5.

**Composition invariant** `I_AnyHaltCausalityClassified`: every halt
has a known cause. Composition theorem:

```
I_AnyHaltCausalityClassified
    ≡ I_StepProgress ∨ I_QuotaProgress ∨ (operator path)
```

**Two new fairness conjuncts:** `WF_vars(MarkStarved(m))` and
`WF_vars(RefreshQuota(c, a))`. The latter encodes cosmon's
declaration that market-agents' refresh observer is weakly fair.

### 2.3 · Cross-galaxy clean separation (hawking C2 + forgemaster §2)

**Two orthogonal phase axes:**
- `account_state` — intrinsic to the account, owned by market-agents.
- `fleet_selection` — extrinsic to the account, owned by cosmon.

The rotator's action modifies only `fleet_selection`, never
`account_state`. ADR-052 I2 (SingleWriterPerField) extends across
galaxies.

**The cross-galaxy contract is a versioned schema** declared in
`cosmon-budget::RefreshEvent v1`. market-agents writes; cosmon reads.
**No Cargo dependency in either direction.** Same pattern as the
syzygie protocol (ADR-047).

### 2.4 · Detection: passive, single-stage (shannon + feynman)

The MSS for budget exhaustion detection is `{B1: refresh.jsonl event,
B2: cs evolve exit code with stderr regex match}` ≈ 1 bit jointly.
**Above the 1-bit binary-decision threshold; sufficient.** No active
probe required.

This is **structurally simpler** than the 8th clock (which needed a
two-stage protocol because passive channel capacity was 0 bits in
the adversarial regime). The simplicity is **derived**, not assumed.

### 2.5 · Decidability boundary (turing)

Exhaustion-prediction from cosmon-side data is **statistically
unidentifiable** for shared currencies (`MaxRolling5h`, `MaxWeekly`,
multi-tenant `ApiKeyOrgMonthly`). Even an omniscient halting oracle
does not help — we lack the data, not the algorithm.

Per-currency epistemological table:

| Currency | Predict class | Detection class |
|---|---|---|
| `MaxRolling5h` | unidentifiable | R (oracle reply) |
| `MaxWeekly` | unidentifiable | R (oracle reply) |
| `ApiKeyOrgMonthly` | R if single-tenant | R |
| `FinancialUSD` | externally identifiable (bank oracle) | R |
| `CustodyScoped` | R | R |

**Architectural consequence:** engineer toward `CustodyScoped` and
single-tenant deployments where prediction is computable. Until then,
predictive rotation is a runtime policy in residence with documented
unidentifiability caveats.

### 2.6 · Hawking's five topology constraints (C1–C5)

| C# | Constraint |
|---|---|
| C1 | `account_state` admits hidden edge `Funded → Throttled` (six phases, two hidden edges); forbidding it would falsify K3 |
| C2 | Two orthogonal phase axes (account intrinsic vs fleet selection) |
| C3 | Tri-valued clock `{Funded, Throttled, Unknown}`; `Unknown` is suspended judgment, not failure |
| C4 | Continuous-time `recovery_rate` projection (5h cap recedes); `Decay` (continuous) vs `WindowJump` (discrete) actions kept separate |
| C5 | Two firing scopes: per-currency (Browning, no ghost, route) vs universal (HeatDeath, exactly one fleet-level ghost, collapse) |

These are **topologically forced**, not policy choices. Any spec that
violates them leaks the K3 pathology back in through a different
channel.

### 2.7 · Implementation: staged delivery (forgemaster + feynman)

The implementation is **separated into a minimum and a deferred
remainder, gated by the measurement plan**:

**Items 1-3 (minimum, ships immediately on ADR acceptance):**
1. `CollapseCause::RateLimit { account, kind }` enum + `cs collapse
   --cause` flag. ~50 LoC.
2. `GhostKind::QuotaExhausted` variant under `#[non_exhaustive]`.
3. `MoleculeStatus::Starved` (peer of `Stalled`).

**Items 4-8 (full clock, ships only on measurement-trigger):**
4. `ComputeReservoir` trait + `ReservoirLevel` + `ReservoirKind` in
   `cosmon-core`.
5. `RefreshEvent v1` schema + `ReservoirFileReader` impl in new
   `cosmon-budget` crate.
6. `cs budget` command (read-only).
7. `cs patrol --token-clock` extension.
8. TLA+ skeleton in `docs/specs/CosmonRun.tla`.

**Deferred (Phase 3 / Resident Runtime):**
- `cs tackle --rotate-on-exhaust` — perimeter violation.
- Predictive rotation policy.

Trigger to ship items 4-8: **≥3 misses** of the regex in 12 months
(retraction-clause.md §2.2 trigger B).

---

## 3 · Composition with the 8 existing clocks

Wheeler's Greek-letter layer assignment locates each clock at the
layer it watches; the 9th occupies **ω** — *outside* α, *upstream* of
inference. It is the only clock that sits beyond the α horizon.

| # | Clock | Layer |
|---|---|---|
| 1 | DAG-bit | δ — control plane |
| 2 | fs mtime | γ — data plane |
| 3 | tmux heartbeat | β — pty process |
| 4 | events.jsonl + flock | event ledger |
| 5 | git | history |
| 6 | archive | retention |
| 7 | RunState | state machine |
| 8 | StepClock (ADR-058) | α — inference emission |
| **9** | **QuotaClock** (this ADR) | **ω — external authority** |

With clock 9 in place, every layer of the cosmon-worker-provider
stack has at least one clock witnessing it. Without it, the layer
cake had a hole the K3 incident fell through.

The composition with `StepClock` via the single-line patch makes the
two clocks fire on **disjoint configurations**:
- `Silence > T_STALL ∧ Fundable` → `MarkStalled` only
- `Silence > T_STALL ∧ ¬Fundable` → `MarkStarved` only

(Full diagram in `clock-compose-diagram.md` §3.)

---

## 4 · DNA stress test

| DNA invariant | Stress | Mitigation |
|---|---|---|
| Zero I/O in core | trait in `cosmon-core` | abstract methods only; no default impls with side effects |
| Stateless CLI | `cs patrol --token-clock` could become a watch loop | poll once per invocation, exit; scheduler calls patrol repeatedly |
| DAG = 1 bit | budget level could leak into DAG edges | budget never enters DAG; `events.jsonl` carries observables, DAG carries ordering |
| Transactional Core first | `cs tackle --rotate-on-exhaust` would make tackle a resident monitor | **deferred to ADR-055 Phase 3** |

**No invariant is violated** by items 1-8 in the staged delivery as
specified (forgemaster §5).

ADR-052 invariants preserved:
- I1 (SingleLedger): `quotaLog` is a projection of `events.jsonl`.
- I2 (SingleWriterPerField): per-record-kind writer discipline; the
  worker writes `QuotaReplyObserved` (its own observation, Lifecycle
  role); claudion writes `QuotaConsumed` (Probe role); market-agents
  writes `QuotaRefreshed` (cross-galaxy environment writer — declared
  in WF assumption).
- I7 (SingleEventWriter): `cs collapse --cause` writes one molecule's
  event under the existing single-writer lock.
- I8 (MeasurementEmission): cosmon-side budget reads emit
  `QuotaProbed` events to a separate `budget-witness.jsonl` file.

---

## 5 · Consequences

### 5.1 · Positive

- K3-shape halts now have a structured cause attribution from day 1.
- Operator-attention exergy spent diagnosing rate-limit halts drops
  from minutes (read pane, infer cause) to seconds (read structured
  ghost label).
- `cs whisper` against a `Starved` molecule is no longer the
  operator's first reflex — the ghost label tells the operator to
  rotate, wait, or collapse.
- Cross-galaxy contract with market-agents is documented and
  versioned. Future galaxies that produce budget signals (custody-
  vault, financial-budget) can reuse the schema.
- The reserve design (items 4-8) is **preserved** — the panel's TLA+
  work is not lost. It ships on evidence (Trigger B), not on faith.

### 5.2 · Negative

- One additional pattern to learn for new contributors. Mitigated by
  the kitchen analogy in `extension-currencies.md` and the
  child-grade explanation in `feynman.md`.
- Cross-galaxy dependency on market-agents' `RefreshEvent` schema.
  Mitigated by `schema_version` field and the `#[serde(other)]`
  fallback discipline.
- A new `MoleculeStatus::Starved` value affects exhaustive `match`
  statements over `MoleculeStatus`. Mitigated by `#[non_exhaustive]`
  on the enum.

### 5.3 · Open (deferred)

- Predictive rotation on shared currencies — structurally
  unidentifiable; deferred to single-tenant deployments or to
  configurable runtime policy.
- Cross-currency exchange-rate decisions — defer to scheduler
  (Resident Runtime).
- Auto-resume of `Starved` molecules — disabled by default;
  configurable in residence with K-bound discipline (mirroring
  ADR-058 §D6 anti-Sisyphus).

---

## 6 · Anti-goals (summary; full list in `anti-goals.md`)

1. Does not predict future exhaustion in the spec.
2. Does not allow the worker to write its own consumption.
3. Does not infer exhaustion from cosmon-side accounting alone.
4. Does not introduce a probabilistic invariant (PTLA).
5. Does not couple to a specific currency in the core spec.
6. Does not import market-agents code.
7. Does not write the budget *state* into `events.jsonl` (the *act of
   refusal* on a specific molecule IS allowed there under Lifecycle).
8. Does not use a daemon (no resident loop in the transactional core).
9. Does not auto-rotate accounts in `cs tackle`.
10. Does not collapse `ProviderError::RateLimited` and
    `CollapseCause::RateLimit` into one type.
11. Does not flood `cs peek` with N molecule-level ghosts at heat
    death (one fleet-level aggregate).
12. Does not auto-resume `Starved` molecules without operator gate.

Hard naming refusals: never `LedgerClock`, `FuelClock`, `GasClock`,
`TokenClock`, `WalletClock`, `CapClock`.

---

## 7 · Cross-references

- `quotaclock-spec.md` — the formal TLA+ spec with mermaid diagrams,
  TLC config, falsifiability traces.
- `clock-compose-diagram.md` — the 9 clocks together; layer cake;
  composition with StepClock.
- `implementation-plan.md` — staged delivery, CLI surface diff, DNA
  stress test, sequence diagram.
- `extension-currencies.md` — how the spec generalizes to API keys
  and financial budgets.
- `anti-goals.md` — the 12 refusals that are part of the contract.
- `retraction-clause.md` — the 12-month measurement protocol and
  retraction triggers.

External:
- ADR-016 — Resident Runtime regime; deferred items wait here.
- ADR-043 — Provider abstraction; `Quota` already a vocabulary
  primitive.
- ADR-047 — Syzygie / event-log protocol v0.
- ADR-052 — The 10 named invariants substrate.
- ADR-055 — cosmon residence.
- ADR-058 — `StepClock` / `I_StepProgress`; this ADR adds the
  `Fundable` conjunct to `MarkStalled`.

---

## 8 · The vision sentence (verbatim from wheeler §7)

> **`QuotaClock` is the only clock that listens to an explicit
> "no" from outside cosmon.**

The kitchen has a pantry, and when the pantry is empty, the chef
cannot cook — even if everything else in the kitchen is fine. The
9th clock is the timer that watches the pantry, not the chef.

---

## 9 · Status of cargo-cult risk

Per feynman §5, the cargo-cult risk on this ADR is **present but
addressable**. The deliberation could have produced *only* the
chronicle entry; instead it produces an ADR + minimum implementation
+ reserve design + measurement plan + retraction clause. Honest
balance:

- The minimum (items 1-3, ~65 LoC) is what K3 demanded. Ship it.
- The reserve design (items 4-8) is what *might* be demanded next.
  Park it.
- The measurement plan commits the project to a verdict procedure
  on a 12-month horizon.
- The retraction clause makes the proposal falsifiable.

Without §10 below, this ADR is not ready to ship.

---

## 10 · Retraction (mandatory; verbatim from `retraction-clause.md` §1)

> **If, 12 months from this ADR's signature, the operator log
> contains zero cases where a `kind: rate_limit` tag was needed to
> disambiguate a halt the StepClock + a single retry could not
> handle on its own, the 9th clock — including any
> `ComputeReservoir` trait machinery built on top — is retracted as
> over-engineering, and the chronicle entry replaces the ADR.**

The two triggers (Trigger A: retract; Trigger B: promote
minimum→full) and the three counters (`N_halts`,
`N_ratelimit_correct`, `N_ratelimit_misdiagnosed`) are documented in
`retraction-clause.md`. The audit script lives at
`tools/cs-budget-audit.sh` (or `crates/cosmon-cli/src/bin/
cs-budget-audit.rs` once items 4-8 ship).

---

## 11 · Coda — the chronicle entry

When this ADR is signed, the operator commits to
an internal chronicle:

> **2026-04-22 — Le 9ème clock écoute le « non »**
>
> Cosmon avait 8 horloges qui écoutent le silence. La 9ème,
> **QuotaClock**, est la première qui écoute un *« non »* — un refus
> explicite prononcé par une autorité extérieure. Délibération-mère :
> `delib-20260422-0101`. ADR : `ADR-062`. Clause de rétractation :
> 12 mois. Si à 12 mois aucun halt n'a eu besoin du tag `kind:
> rate_limit` pour être correctement diagnostiqué, le 9ème clock est
> retiré et cette entrée le remplace.
>
> Le réacteur apprend de ce qu'il brûle.

— end ADR-062 —
