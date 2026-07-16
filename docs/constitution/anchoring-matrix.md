# Anchoring Matrix — Axis × Mechanism

> Companion to [`noogram-core-v0.md`](./noogram-core-v0.md). The noogram core
> fixes four invariant axes (existence-in-time, content-integrity, authorship,
> governance-continuity). Each must be certified by at least one mechanism
> grounded in a reference frame external to the noogram (core §5, I5). This
> document enumerates candidate mechanisms, assigns each to at most one axis,
> and selects a minimal covering set.
>
> **Selection rule (einstein).** A mechanism that covers zero axes is useless.
> A mechanism that claims to cover more than one axis *bundles* invariants
> that must stay orthogonal — that is the defect of a generic blockchain. We
> therefore admit only mechanisms that cover exactly one axis. Redundancy is
> acceptable and sometimes required (multiple mechanisms per axis); coupling
> is not.
>
> **Scope.** Costs are indicative public-market ranges, not internal figures.
> Independence is graded relative to the noogram operator's unilateral
> control: *high* = the operator cannot unilaterally revoke or rewrite the
> reference frame; *medium* = revocable with effort or time; *low* = under
> operator control in practice.

## 1. Axes (rows)

1. **A1. Existence-in-time** — answers "did this bit-string exist at time t?"
   Corresponds to noogram invariant I1.
2. **A2. Content-integrity** — answers "is this the same bit-string it was
   when claimed?" Corresponds to I2.
3. **A3. Authorship** — answers "who claims responsibility for this
   bit-string?" Corresponds to I3.
4. **A4. Governance-continuity** — answers "who holds the authority to amend
   the core, and under what succession?" Corresponds to the meta-rule that
   I5 seals remain anchored across custodian changes. This is the one axis
   no cryptographic primitive alone can cover.

## 2. Mechanisms (columns)

- **M1. OpenTimestamps (Bitcoin-anchored).**
- **M2. Transparency log** (Certificate-Transparency-shaped public log).
- **M3. Ed25519 signature, hardware-bound key** (e.g. HW token).
- **M4. 2-of-N external-attestation protocol** (*described here only as a
  mechanism class; the procedure itself lives outside the noogram core — see
  I5*).
- **M5. Notaire physique** (civil-law notary act).
- **M6. INPI Enveloppe Soleau** (French IP-authorship deposit).
- **M7. Swiss notaire deposit** (reserved: cross-jurisdiction fallback).
- **M8. Academic partnership with named MoU** (named human + institution +
  written succession clause).
- **M9. Legal entity with succession clause** (association loi 1901,
  foundation, or equivalent — entity with standing, not just a custodian).

## 3. Matrix

Legend — **Y** = this mechanism certifies this axis directly; **—** = it does
not; **P** = partial (would require composition or repeated use). Cost is
order-of-magnitude EUR, either recurring (`/yr`) or one-shot. Reversibility
indicates whether the anchor can be withdrawn by the operator once laid.
Independence grades the reference frame's autonomy.

| Axis ↓ \ Mech → | **M1 OTS** | **M2 Trans. log** | **M3 Ed25519 HW** | **M4 2-of-N attest.** | **M5 Notaire** | **M6 INPI Soleau** | **M7 Swiss notaire** | **M8 Academic MoU** | **M9 Legal entity** |
|---|---|---|---|---|---|---|---|---|---|
| **A1 Existence-in-time** | **Y** | P | — | — | Y | Y | Y | — | — |
| **A2 Content-integrity** | — | **Y** | — | — | P | P | P | — | — |
| **A3 Authorship** | — | — | **Y** | — | Y | Y | Y | — | — |
| **A4 Governance-continuity** | — | — | — | — | — | — | — | P | **Y** |

Each column's **Y** marks the axis where the mechanism is retained under the
one-axis selection rule; other **Y**/**P** entries indicate capabilities the
mechanism *could* serve but where it would bundle invariants better held
apart.

### Per-cell parameters (one-axis selection)

| Mechanism (retained axis) | Cost | Reversibility | Independence |
|---|---|---|---|
| **M1 OTS → A1** | ≈0 recurring (fee on Bitcoin tx when calendar batches) | Irreversible once confirmed | **High** (SHA-256 + Bitcoin liveness; two orthogonal external assumptions) |
| **M2 Transparency log → A2** | 0–low recurring (hosting + operator SLA; public CT-style logs exist at minimal fee) | Append-only; inclusion proof cannot be withdrawn | **High** (public log's witness set is independent of operator) |
| **M3 Ed25519 HW-bound → A3** | ~50–100 EUR one-shot per token | Key is revocable via published revocation; signatures already issued remain valid | **Medium→High** (private key in hardware; revocation requires the custodian's cooperation) |
| **M9 Legal entity w/ succession → A4** | ~0–300 EUR one-shot (association 1901) to several k EUR one-shot (foundation); recurring admin ~low | Dissolution is possible but leaves a trace; succession clause survives custodian change | **Medium→High** (jurisdiction holds the registry; operator cannot rewrite it unilaterally) |

### Rejected / deferred cells

- **M4 (2-of-N attestation)** — deliberately assigned no axis. Per noogram
  core I5, the verification procedure lives *outside* the core. M4 is a
  meta-mechanism layered on top of M3: it aggregates multiple independent
  Ed25519 signatures to certify a meta-predicate *about* the noogram (e.g.
  "this hash corresponds to the text the operator intends"). It is not an
  axis-covering anchor; it is the mechanism by which external ratification
  under I5 becomes *trustworthy*. Specifying it here would re-enter the
  noogram and violate the meta/object disjunction (godel).
- **M5 Notaire physique** — covers A1 and A3 simultaneously and at
  per-deposit cost ≈100–300 EUR. Rejected as primary under the one-axis rule
  (would bundle existence-in-time with authorship into a single mechanism
  whose failure would cross-contaminate both). Admitted as *redundant* backup
  for A1 or A3 if high-robustness is later required.
- **M6 INPI Enveloppe Soleau** — covers A1 and A3 at ≈15 EUR one-shot,
  duration 5 yr renewable once. Rejected as primary under the one-axis rule.
  Retained as *event-level* authorship/existence redundancy for the initial
  core-text deposit — a low-cost insurance against later dispute of the
  authorship of v0 itself. Specifically: this is the mechanism used *once*
  for the founding deposit, not the mechanism certifying ongoing A1 or A3.
- **M7 Swiss notaire deposit** — held in reserve. A cross-jurisdiction
  deposit strengthens A4 (governance continuity across jurisdictions) and
  A1 (time certification by a second legal system). Cost: several hundred
  EUR one-shot, recurring minimal. Activate only if A4's primary mechanism
  (M9) is weakened by a specific jurisdictional risk that materialises
  later.
- **M8 Academic MoU** — partial on A4 only. An MoU without a named human
  and a written succession clause is fiction (hawking); with both, it
  approximates governance continuity but lacks standing as a legal entity.
  Admitted as *complementary* to M9, not as a substitute.

## 4. Minimal covering selection

Applying the one-axis rule and the coverage requirement (each of A1–A4
covered by at least one retained mechanism):

| Axis | Primary mechanism | Indicative cost | Why this, not others |
|---|---|---|---|
| **A1 Existence-in-time** | **M1 OTS** | ≈0 recurring | Highest independence (two orthogonal external assumptions: SHA-256, Bitcoin liveness), irreversible, no semantic payload, no governance coupling. |
| **A2 Content-integrity** | **M2 Transparency log** | 0–low recurring | Append-only witness set independent of operator; inclusion proofs verifiable by any third party; no bundling with A1 or A3. |
| **A3 Authorship** | **M3 Ed25519 HW-bound** | ~50–100 EUR one-shot | Signatory binding is hardware-local and revocable; does not leak time or integrity semantics. |
| **A4 Governance-continuity** | **M9 Legal entity w/ succession** | ≈0–300 EUR one-shot (1901) | Only mechanism with standing to carry authority across custodian changes; neither cryptographic nor cognitive — and that is the point. |

Total first-year outlay (indicative): **≈50–400 EUR one-shot + ≈0 recurring
operating cost for anchoring**, excluding the legal-entity administrative
baseline.

### One-shot founding insurance (event-level)

- **M6 INPI Enveloppe Soleau** deposit of the v0 core-text, `digest(κ)` of
  each durable artifact, and the identity of the founding custodian —
  **≈15 EUR one-shot**. This is the only cross-axis mechanism admitted in
  the minimal selection, and only because it is used *exactly once*, for the
  single event where A1 and A3 of v0 itself are being bootstrapped against
  the risk of later authorship dispute. It is not the ongoing anchor for
  either axis.

## 5. Redundancy policy (optional, per-axis)

Where robustness is later deemed necessary, mechanisms may be stacked on the
same axis *provided* each retained mechanism still covers exactly one axis on
its own account:

- **A1 robustness** — stack **M1** with a second time authority (periodic
  notarised-time receipt, M5 used only for A1). Failure of either alone does
  not break A1.
- **A3 robustness** — stack **M3** with **M5 used only for authorship** (a
  notarised signature of the public key fingerprint). Failure of hardware
  custody does not erase authorship.
- **A4 robustness** — stack **M9** with **M8** (MoU, complementary) and
  **M7** (cross-jurisdiction deposit). Each adds an independent succession
  path.
- **A2 robustness** — stack **M2** with an independent mirror holding the
  same log; inclusion discrepancy is publicly detectable.

Redundancy is cheap on A1 and A2, moderate on A3, and expensive on A4.
Spend accordingly.

## 6. What this matrix does not decide

- **The digest algorithm.** Core §6 defers it; "Bitcoin-anchored" in M1
  implies SHA-256, which is the currently admissible pre-image-resistant
  function. A future core revision may substitute.
- **The cardinality and identities** of the attestation set for M4. Living
  outside the noogram, they are specified in a separate procedure document
  under I5.
- **The specific legal entity form** for M9 — association, foundation, or
  equivalent. That is a jurisdictional and operational decision deferred to
  a legal-entity ADR.
- **The cadence** of OTS commits and transparency-log appends. A cadence
  policy is a projection (T2) over Σ, not a core property.

These deferrals are deliberate: the matrix commits only to the minimal
axis→mechanism binding required by the core. Everything contingent remains
contingent.
