# ADR-074 — QuotaClock: formal specification of the 9th clock

**Status:** Proposed (2026-04-23)
**Scope:** Promote the formal TLA+ spec produced by `delib-20260422-0101`
to architectural-invariant status. This ADR carries the **three
laws** (`I_QuotaMonotone`, `I_QuotaProgress`, `I_AnyHaltCausalityClassified`),
the minimal TLA+ signature, the rejected alternatives, and the
implementation gate. It is the formal-spec companion to the
decision-and-vocabulary ADR-062.

**Parent task:** `task-20260424-58a2`
**Source deliberation:** `delib-20260422-0101`
— 7-persona panel (knuth, wheeler, hawking, shannon, feynman,
turing, forgemaster). Authoritative spec:
`quotaclock-spec.md`.

**Binds:**
- [ADR-016](016-autonomy-regimes-and-resident-runtime.md) — regime
  vocabulary (Inert / Propelled / Autonomous). `I_QuotaProgress`
  applies in Propelled; predictive rotation defers to a future
  runtime consumer.
- [ADR-052](052-one-ledger-one-writer-one-witness.md) — the ten
  named invariants. `quotaLog` is a **projection** of `events.jsonl`
  under I1 (SingleLedger); per-record-kind writer discipline
  preserves I2 (SingleWriterPerField).
- [ADR-056](056-notary-protocol-v0.md) — seal-chain compatibility:
  `QuotaReplyObserved` events travel the same notary pipeline as
  other lifecycle events. No new seal kind required.
- [ADR-058](058-step-progress-invariant.md) — `StepClock` / the 8th
  clock. This ADR adds the **single-line `Fundable` conjunct** to
  `MarkStalled`, making `Stalled` and `Starved` fire on disjoint
  configurations.
- [ADR-062](062-quotaclock-9th-clock.md) — the vocabulary and
  decision ADR for `QuotaClock`. ADR-074 does not restate the
  decision; it **formalizes the three laws** the decision rests on.

---

## 1 · Context

### 1.1 · The gap

Eight clocks watch cosmon's own filesystem, its processes, and the
worker's α-emission (ADR-058 §D3). None of them watch the **layer
above α — the external authority that grants α the right to emit at
all**. The K3 pathology (worker halted on a *"Claude usage limit
reached"* reply, no cosmon clock fired) proved the layer cake had a
hole.

### 1.2 · Why a formal spec is required

ADR-062 names and decides; it does not **formally prove** that
`Starved` and `Stalled` fire on disjoint configurations, nor that
every halt reaches a classified terminal state under weak fairness.
These properties are the load-bearing reason the 9th clock is worth
adding — without the formal guarantee, the `Fundable` conjunct on
`MarkStalled` is a patch, not an invariant. ADR-074 lifts the patch
to a checked TLA+ property.

### 1.3 · The decidability boundary (turing §2.5)

Cosmon-side `Headroom` is a **one-sided upper bound** on truth:
shared-tenant currencies (`MaxRolling5h`, `MaxWeekly`) are consumed
by third-party agents invisibly to cosmon. Exhaustion-prediction on
shared currencies is **statistically unidentifiable** — not
undecidable, but uncomputable from cosmon's data. The spec must
therefore be:
- **liveness, not safety** on the exhaustion side (a leads-to);
- **oracle-driven**, with the antecedent `QuotaReplyExhausted`
  (observable at the moment a refusal reply appears in the ledger),
  not `Headroom = 0` (cosmon-side accumulator).

Safety is still available on the accounting side — `I_QuotaMonotone`
bounds cosmon's own claim about consumption.

---

## 2 · Decision

ADR-074 adopts **three laws** as the formal content of `QuotaClock`.
Each stands alone; together they close the composition hole ADR-058
left open. TLA+ signature summary follows in §2.4; the authoritative
spec with full actions and proofs lives in `quotaclock-spec.md`.

### 2.1 · Law 1 — Safety: `I_QuotaMonotone` (one-sided accounting)

```tla
I_QuotaMonotone ==
    \A c \in Currency, a \in Account :
        ConsumedSince(c, a, LastRefresh(c, a)) <= Ceiling[c, a] + MaxConsumed
```

**What it says.** Cosmon's accumulator never claims to have consumed
more than the ceiling, plus a one-call overshoot tolerance
(`+ MaxConsumed`) encoding the impossibility of pre-billing.

**What it does NOT say.** It does **not** prove the ceiling will not
be exceeded by joint consumption across unobserved third-party
tenants. The bound is one-sided on purpose (turing §2.3); it protects
the ledger's accounting honesty, not the environment's actual state.

### 2.2 · Law 2 — Liveness: `I_QuotaProgress` (oracle-driven)

```tla
I_QuotaProgress ==
    \A m \in Mol :
        (mol_status[m] = "Running" /\ ~Fundable(m))
            ~> mol_status[m] \in {"Starved", "Frozen", "Collapsed"}
                 \/ Fundable(m)
```

**What it says.** Every `Running` molecule that loses funding
eventually reaches `Starved`, `Frozen`, `Collapsed`, or regains
funding (via `RefreshQuota`). No trace may leave a molecule wedged
in the unfunded `Running` configuration indefinitely.

**Fairness obligation.** `WF_vars(MarkStarved(m))` and
`WF_vars(RefreshQuota(c, a))`. The latter is cosmon's architectural
declaration that its environment (market-agents' refresh observer)
is weakly fair. If the environment crashes for a week, the WF
assumption fails and the model correctly predicts starvation
accumulation — the spec shape is honest about its dependency.

### 2.3 · Law 3 — Composition: `I_AnyHaltCausalityClassified`

```tla
I_AnyHaltCausalityClassified ==
    \A m \in Mol :
        (mol_status[m] = "Running" /\ Silence(m) > T_STALL)
            ~> mol_status[m] \in {"Stalled","Starved","Collapsed",
                                  "Frozen","Completed"}
```

**What it says.** Every halt has a known cause. A trace that leaves
a molecule with `Running ∧ Silence > T_STALL` indefinitely is a spec
failure — **the K3 pathology at the formal layer**. The property
decomposes cleanly:

```
I_AnyHaltCausalityClassified
    ≡ I_StepProgress ∨ I_QuotaProgress ∨ (operator path)
```

Each disjunct is a separate way out. Soundness of the disjunction
requires the **single-line patch** to `MarkStalled` (§2.4): adding
`Fundable(m)` as a guard conjunct forces `Stalled` and `Starved` to
fire on disjoint configurations.

### 2.4 · TLA+ signature summary

```tla
CONSTANTS
    Account, Currency, Authority, AuthorityOf,
    AccountOf, Ceiling, Refresh, MaxConsumed

VARIABLE quotaLog   \* append-only projection of events.jsonl, filtered to
                    \* { QuotaConsumed, QuotaRefreshed, QuotaReplyObserved }

StatusValues == {"Absent","Pending","Running","Stalled","Starved",
                 "Completed","Collapsed","Frozen"}

Headroom(c, a) == Ceiling[c, a] - ConsumedSince(c, a, LastRefresh(c, a))

Fundable(m) ==
    \A c \in Currency :
        Headroom(c, AccountOf[m]) > 0
        /\ ~ QuotaReplyExhausted(AccountOf[m])

\* New actions:
\*   ReportConsumption(m, n)   -- claudion-written (probe role, I2)
\*   RefreshQuota(c, a)        -- environment action (cross-galaxy writer)
\*   ObserveQuotaReply(...)    -- worker-written (lifecycle role, I2)
\*   MarkStarved(m)            -- observer-written (disjunctive antecedent)

\* Modified actions:
\*   Evolve(m), Complete(m)    gain the Fundable(m) guard
\*   MarkStalled(m)            gains the Fundable(m) guard  <-- single-line patch
\*   Collapse(m)               accepts "Starved" as a source
```

Full spec (actions, falsifiability traces, TLC config) in
`quotaclock-spec.md`.

---

## 3 · Rejected alternatives

Three non-trivial alternatives were proposed during the delib and
are refused. Each refusal is a commitment — reopening it requires a
successor ADR.

### 3.1 · `LedgerExtension` — ledger safety instead of liveness (knuth, §4 alternative)

**Proposal.** Replace `I_QuotaProgress` (liveness) with a safety
invariant `active ⇒ remaining > 0`, derived from a richer ledger
state.

**Refused.** Turing §2 proves this shape is *provably false* on the
K3 trace: cosmon's `remaining` read 100 at the moment a 429 was
already returned, because third-party consumers had depleted the
shared pool invisibly. A safety invariant grounded in cosmon-side
data **lies by construction** on shared currencies. The invariant
must be a leads-to with an oracle-observable antecedent, or it is
not an invariant at all.

### 3.2 · Observational-only, no invariant (shannon, §5 alternative)

**Proposal.** Add `GhostKind::QuotaExhausted` and the `CollapseCause`
tag but leave the TLA+ spec unchanged. Lean on the 1-bit MSS
`{B1: refresh.jsonl, B2: exit code}` and trust operator attention for
composition.

**Refused.** Without `Fundable(m)` as a guard on `MarkStalled`, the
two clocks can fire on the same observable configuration — exactly
the K3 misattribution arm (`Silence > T_STALL` while `¬Fundable`
makes `MarkStalled` enabled against an already-starved molecule).
The panel converged (C6) that the single-line guard is the minimum
that makes the disjunction sound at the spec level. A ghost label
without an invariant re-admits the bug the 9th clock exists to close.

### 3.3 · Defer to Phase-3 daemon (hawking, §5 alternative)

**Proposal.** Wait for the Resident Runtime (ADR-016 Phase 3) to
ship; implement QuotaClock as a daemon-side concern with
continuous-time `recovery_rate` projection. Nothing lands in the
transactional core now.

**Refused.** Two grounds. (a) ADR-058 established that *detection
sits in the transactional core and scheduling sits in residence*; a
daemon-only QuotaClock breaks that layering and would duplicate
state. (b) The K3 pathology is a **present-day** reality — operators
already hit rate-limits; the post-mortem fix (ADR-062 items 1–3)
ships now regardless of ADR-074's verification status. Hawking's
continuous-time machinery (C4 `recovery_rate`) is preserved in the
**ADR prose** but never enters the TLA+ spec; refresh is modelled as
discrete `RefreshQuota` actions fired by an external observer.

---

## 4 · Consequences

### 4.1 · Phase 1 — `QuotaState` trait + event reader (cosmon-core, non-invasive)

Implement a zero-I/O trait in `cosmon-core`:

```rust
pub trait QuotaState {
    fn fundable(&self, account: &AccountId) -> bool;
    fn last_reply(&self, account: &AccountId) -> Option<QuotaEvent>;
}

#[non_exhaustive]
pub enum QuotaEvent {
    Consumed { account: AccountId, currency: Currency, n: u32, at: Instant },
    Refreshed { account: AccountId, currency: Currency, at: Instant },
    ReplyObserved { account: AccountId, status: ReplyStatus, at: Instant },
}
```

The impl **reads the existing `claude-reservoir` refresh-worker
event log non-invasively** — no double instrumentation, no new
writer. Trait lives in core; file reader lives in a
feature-gated `cosmon-budget` crate (per ADR-062 §2.7, items 4–5).

**Gate:** items 4–8 of ADR-062 ship only after the measurement
trigger (≥3 regex misses in 12 months, per `retraction-clause.md`).

### 4.2 · Phase 2 — API-key currency (extends the Currency set)

Extend `ReservoirKind` with `ApiKeyOrgMonthly` once a single-tenant
API-key deployment exists. Per turing §1.4, this is the
**decidable** path: first-party accounting alone is tight when the
account has no third-party consumers. The spec change is zero (the
`Currency` constant set in TLC configs takes a new value; no action
or invariant rewrites).

### 4.3 · Phase 3 — cross-currency aggregator (Resident Runtime consumer)

When the Resident Runtime lands (ADR-016), it becomes the consumer
of `QuotaClock` across currencies — choosing account rotation,
cross-currency exchange, and predictive policies subject to
documented unidentifiability caveats (turing §1.4). The
transactional core never owns this logic.

### 4.4 · Verification gate

Mechanical validation of the three laws uses the same TLC pipeline
that validated `I_StepProgress` in ADR-058 §7:
- `CosmonRun_QuotaClock.cfg` — single-currency single-account baseline.
- `CosmonRun_QuotaClock_Multi.cfg` — two-currency extension check.
- `CosmonRun_QuotaClock_HeatDeath.cfg` — all currencies exhausted
  simultaneously (hawking C5 aggregate).

The validation run is recorded in `docs/specs/VALIDATION-REPORT.md`
under a new Model entry. Per architectural-invariants §8b
(**propose mechanisms of verification, do not impose them**), this
ADR declares the spec and the TLC gate; it does not block the
runtime on unverified changes.

---

## 5 · What ADR-074 does **not** do

- **Does not restate ADR-062's vocabulary or delivery plan.** The
  staged delivery, naming, cross-galaxy schema, retraction clause,
  and CLI surface live in ADR-062; ADR-074 is the formal-spec
  companion.
- **Does not predict exhaustion.** Liveness with oracle antecedent
  only (turing §2.5). Predictive rotation is a runtime policy
  (Phase 3), not a spec invariant.
- **Does not add a new CLI verb.** The minimum surface
  (`cs collapse --cause`) and the deferred surface (`cs budget`,
  `cs patrol --token-clock`) are defined in ADR-062 §2.7.
- **Does not introduce continuous-time in TLA+.** Refresh is
  discrete; continuous `recovery_rate` is an ADR-prose construct
  only (hawking §4d).
- **Does not require a daemon in the transactional core.** ADR-016
  L2 is preserved.

---

## 6 · References

- `quotaclock-spec.md`
  — the authoritative TLA+ spec (actions, falsifiability traces, TLC config).
- `implementation-plan.md`
  — staged delivery sequence + `RefreshEvent v1` schema.
- `extension-currencies.md`
  — portability to Max / API-keys / financial / custody currencies.
- `synthesis.md`
  — 7-persona verdicts, convergences C1–C9, tensions T1–T3.
- ADR-016 — regimes; ADR-052 — one-ledger substrate; ADR-056 —
  notary; ADR-058 — `StepClock`; ADR-062 — `QuotaClock` decision
  and vocabulary.
