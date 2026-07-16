# ADR-152 — Budget-aware SOR is a pure policy that emits an authoritative receipt

**Status:** Accepted (2026-07-12)
**Decision owner:** Noogram
**Origin:** C3 of `delib-20260711-c6c8`
**Depends on:** ADR-142 (Incarnation launch-time decision), ADR-147 (provider-family diversity witness), ADR-150 (directional routing policy / carrier parity, C1), ADR-151 (monotone criticality declaration, C2)
**Blocks:** C4 (critical cross-provider committee wiring)

## Context

ADR-142 places Smart Order Routing as a **policy** above the launch-time
`Incarnation` selector, not a new core primitive: whatever the router decides
must be exactly one partial `Incarnation` re-entering the single `cs tackle`
resolution fold. C1 (ADR-150) shipped the named directional policy that
*produces* a partial `Incarnation`; C2 (ADR-151) shipped the monotone
criticality fold that says *how much assurance* a subject demands. What was
missing was the ranking that **chooses the seat** under budget pressure, and a
durable record of *why*.

Two concrete gaps motivated C3:

1. **The selection surface was ex-ante and mutable.** `AdapterSelected` /
   `ModelSelected` are minted *before* the availability probe, and
   `model-selection.json` is a mutable sidecar. Neither is an authoritative
   *ex-post* record: a retrospective audit cannot replay which venues were
   considered, what was observed, how they scored, and why one won.

2. **`unreadable → empty` conflated with `absent → zero`.** The strong-model
   ceiling folds `events.jsonl` for the in-window strong-dispatch count. When the
   log was *unreadable* the fold returned an empty vector — a count of **zero** —
   which *opens* the budget gate exactly when the evidence is missing. A budget
   ceiling must **fail closed**.

3. **Calibration carried no freshness identity.** `CalibrationSnapshot` scored a
   corpus revision but did not record *which model version* it scored or *when*,
   so a router could not tell a fresh reading from a stale one across a model
   bump.

## Decision

SOR is a **pure, total, deterministic** function
(`cosmon_core::sor::select`) with a four-stage shape:

1. **Hard filter (admissibility).** Each candidate is checked against the
   request's hard constraints: spawnability + carrier parity (ADR-150), an
   honoured literal pin, capacity, diversity (ADR-147), and — **only when the
   subject is critical** (ADR-151) — *fresh* calibration and *known* local budget
   history. A rejected candidate is recorded with a typed `RejectReason`; it is
   never silently dropped. The router **only ever chooses among admissible
   seats** and never marchande a witness.

2. **Versioned integer score.** Survivors are scored over quality, headroom,
   availability, cost, and a staleness penalty, with a recorded `SCORE_VERSION`.
   Integer arithmetic keeps the order total and free of float-NaN hazards. Two
   scores are comparable only when their versions agree.

3. **Total, stable tie-break.** Survivors are ordered by
   `(score desc, adapter asc, model asc)` — a *total* order, so the winner is
   deterministic even on ties.

4. **Receipt or typed refusal.** No admissible candidate ⇒
   `SorRefusal::NoAdmissibleCandidate` carrying every reject — the router
   **never** falls back to a global provider default. Otherwise a `SorDecision`
   with the chosen partial `Incarnation` and a sealed `RoutingReceipt`.

### Local consumption vs external observation

The module keeps two provenance classes strictly apart, because they fail
differently:

- **Local-attributed consumption** (`LocalConsumption`) is a fold over *our own*
  `events.jsonl`. Its only honest states are `Available(n)` and `Unavailable` —
  and `Unavailable` is never `Available(0)`. A budget gate fails closed on
  `Unavailable`.
- **External observations** (`Observation<T>`) — quota, price, provider load,
  calibration freshness — are *reports about the world* that decay. Each carries
  a **value, source, observed_at, TTL, derived status, and content hash**, so the
  receipt records exactly what was believed and how fresh it was.

### The authoritative receipt

`RoutingReceipt` is the append-once, ex-post record — the payload of the
`RoutingDecisionRecorded` / `IncarnationDecided` event. It carries policy digest,
criticality + provenance, candidates, typed rejects, per-term scores + tie-break
order, the chosen `Incarnation`, attempt/supersession, and a **content hash**
sealing every other field (via the shared canonical `cosmon_hash` plumbing).
Because `select` is deterministic, two runs over byte-identical inputs produce
byte-identical receipts and hashes — which is exactly what makes **replay on
restart** safe: the consumer re-emits the recorded receipt rather than silently
recomputing. Its emission must precede SOR dispatch; a failure to record forbids
the dispatch. `AdapterSelected` / `ModelSelected` remain as compatibility
projections.

### Fail-closed history fold

`cosmon_core::model_budget` gains `LocalHistory { Counted(u32) | Unavailable }`
and `strong_gate_with_history`. The `cs tackle` seam
(`load_strong_dispatch_records`) now distinguishes a genuinely-absent log
(`NotFound` → `Counted(0)`, a trustworthy zero) from an *unreadable* one
(`Unavailable`). With a ceiling configured, `Unavailable` fails closed —
downgrade (`DowngradeReason::HistoryUnavailable`) or abort
(`StrongGate::AbortHistoryUnavailable`) per policy — never treating unknown
history as zero. With no ceiling configured the history is irrelevant and a
positive-act strong pin is honoured, byte-identical to today.

### Calibration freshness

`CalibrationSnapshot` gains optional `model_version` and `measured_at`
(serde-default, so legacy snapshots keep loading). A calibration reading is only
comparable across sweeps sharing `(corpus_rev, model_version)`; the SOR uses
`measured_at` + a TTL to classify a reading fresh/stale/missing.

## Consequences

SOR stays stateless, pure, and cheap to remove; its only output is one partial
`Incarnation` entering the existing fold. C4 may add committee requirements that
tighten admissibility, but the SOR chooses only among admissible seats and
cannot weaken any witness. The receipt is the durable audit substrate C4 and
`showroom` consume via `../showroom/docs/feedback/cosmon-directional-routing-sor.md`.

### Zero I/O

Like `model_budget` and `calibration`, `sor` is pure — `now` is always
caller-supplied. The seam that folds the event log, probes availability, and
appends the receipt is the `cs tackle` shell.
