# ADR-135 — The living-audit-subject primitive: a verdict over a moving subject auto-falsifies

**Status:** proposed
**Date:** 2026-06-25
**Decider:** Noogram
**Authoring task:** `task-20260625-782d`
**Source finding:** `dave` galaxy — pattern identified by the *architect*
persona (S2) in `delib-20260508-a12d`, flagged by the *dirac* persona in
`delib-20260508-1eef` as **speculative-generality cosmon-ward** (a general
substrate concern, **not** a dave-ward mechanism). Migrated cosmon-ward per
the explicit directive of dave `task-20260508-b2c8` — *"Filer cosmon-ward via
mailroom ou nucléation directe à cosmon galaxy."*

**Binds:**
`docs/architectural-invariants.md` §8b (**briefing seals** — the molecule-layer
*instance* of this primitive that cosmon already ships; this ADR is its
generalization),
ADR-097 (the L1–L5 fleet
validator — the audit *machinery* that produces verdicts; this ADR constrains
the *form* of every verdict it emits),
[ADR-130](130-notary-quarantine-consensus-layer.md) (keep the **signature/seal**
primitive, quarantine the consensus layer — the seal-as-trace BLAKE3 mechanism
reused here),
[ADR-047](047-event-log-protocol-v0.md) (the append-only event log — the
immutable-snapshot substrate a content-addressed verdict pins against),
[ADR-082](082-architecture-baseline.md) (substrate tier — *why* cosmon, not
dave, owns this).

**Architectural invariants:** Composability Principle (CLAUDE.md — molecules +
formulas are the only extension points; this ADR adds **no command, no daemon,
no store**); §8b (*propose mechanisms of verification, do not impose them* — the
seal is a trace, not a lock).

---

## Context

An audit assumes its subject holds still. You read the subject, you record a
finding, the finding outlives the reading: *"line 42 hard-codes a Geant4
constant"*, *"this claim has no source"*, *"the covariance estimate is
singular"*. The verdict is treated as a durable fact about the subject.

That assumption silently breaks the moment the subject is **alive** — still
being edited while the audit is in flight. Then the verdict measures a moving
target, and **auto-falsifies**: by the time the report is read, the revision it
judged no longer exists. Line 42 has moved, the unsourced claim now has a
citation, the singular matrix was reconditioned by a parallel fix worker. The
finding is not *wrong* — it was true of a revision that is gone. It is **stale**,
and nothing in a bare verdict distinguishes stale from wrong.

This came cosmon-ward from the `dave` galaxy, which audits a received CUDA
gamma-imaging kernel against its academic source-of-record. The *architect*
persona named the pattern in delib a12d: the audited subject auto-falsifies
while the audit runs. The *dirac* persona, in delib 1eef, made the load-bearing
call: **do not fold this into the dave ledger.** It is not a dave mechanism
— it is *speculative generality imposed before observation* if dave carves out
a bespoke handler for it, and *corroborated generality* if cosmon names it once
as substrate. Every galaxy that audits a mutable subject meets it:

- **dave** — auditing a live codebase whose author and fix-workers keep
  editing.
- **sandbox** — auditing statistical estimates that shift as the
  data window or the fit re-runs.
- **mailroom** — auditing a comms thread that grows new messages mid-triage.
- **lumen** — auditing rendered surfaces that re-render under it.

Folding the handler dave-ward would mint **N divergent copies** of the same
discipline, none of them shared — the exact anti-pattern that made `visual-qa`
(ADR-120) and `latex-convergence` (ADR-129) cosmon primitives rather than
per-galaxy checklists.

### Two flavors of the pathology

1. **Passive drift.** The subject is edited *concurrently* by some other actor
   (the author, a parallel worker, a re-run). The audit and the mutation race;
   the verdict references a revision the live subject has left behind.
2. **Reflexive auto-falsification** (the sharper, more dangerous case). Acting
   on the audit's *own* findings mutates the subject — and if the audit ledger
   is itself a node of the audited system, each corrective action re-opens the
   audit. The audit eats its own tail; a single pass never terminates, yet the
   ledger reads as if it did.

This is cosmon's physics vocabulary earning its keep: it is the **observer
effect** at the audit layer — you cannot read a live subject and assume the
reading outlives the act of reading. It is the same two-clock hazard §8d calls
Hawking's chronology protection, one layer up: the verdict-clock and the
subject-clock will disagree, and a bare verdict has no principled way to say
which moment it belongs to.

## Decision

**Name the primitive; reuse the seal cosmon already owns; build nothing new.**

Per dirac's flag, the trap here is *generalization-into-mechanism* — inventing a
`cs audit-freeze` verb, an audit daemon, or a per-galaxy ledger schema *before*
the need is corroborated. cosmon already ships the correct primitive at the
molecule layer: **briefing seals** (§8b) — a BLAKE3 hash that pins the *exact
bytes* judged, plus `cs verify`, which recomputes the hash against the live file
and reports PASS / **FAIL** (divergence) / **SKIP** (no seal on record). The
seal does not freeze the subject; it makes the subject's drift **visible**, the
way `git status` makes a dirty tree visible. That is exactly the antidote a
living-audit-subject needs.

So this ADR does not add a command, a daemon, or a store. It **inscribes a
doctrine** and points every galaxy at the existing seal-as-trace BLAKE3
primitive (§8b / ADR-130). When a galaxy needs enforcement beyond a trace, it
composes a **formula** over molecules (Composability Principle) — never a new
core verb.

### The doctrine — four rules

1. **Snapshot-before-judge.** An audit freezes its subject to a
   content-addressed revision (a git rev, a BLAKE3 seal, an immutable copy)
   **before** recording any finding. You judge a snapshot, never a live handle.

2. **The verdict carries its snapshot.** Every finding is the pair
   `(claim, subject-hash)`, emitted as `verdict @ snapshot` — never `verdict`
   unqualified. A finding with no subject-hash is **inadmissible**: it cannot be
   retrospectively checked, which is precisely why `cs verify` exits `2` (SKIP,
   inconclusive) rather than `0` (PASS) on a sealless molecule. A bare verdict
   over a living subject *is* a shadow contract (§8b) waiting to happen.

3. **Divergence means stale, not wrong — re-audit, don't trust.** When the live
   subject's hash ≠ the sealed hash, the verdict is **stale**: re-run it against
   the new snapshot. This is the FAIL-vs-SKIP distinction generalized — a
   live/sealed mismatch is a *known divergence* (act on it), not a *missing
   record* (inconclusive) and not a *defect in the subject* (don't blame the
   author for a finding about a revision they already fixed).

4. **No reflexive ledger.** The audit ledger must not be a node of the subject
   it audits. If it unavoidably is (flavor 2), declare it: the audit is a
   **loop-until-dry**, not a single pass — budget for convergence and make the
   non-termination *explicit*, rather than emitting a one-shot verdict that the
   next corrective action silently invalidates.

### What this is NOT (the dirac guardrail, restated)

- **Not a new `cs` verb.** No `cs audit-freeze`, no `cs audit-verify`. The
  freeze is content-addressing the subject (git rev / BLAKE3 seal) the galaxy
  already has; the verify is `cs verify` or its formula-level analogue.
- **Not a dave (or any single galaxy) mechanism.** It is substrate. A galaxy
  that re-derives it locally is shipping a divergent copy — file a bead, point
  at this ADR.
- **Not a lock.** Same as §8b: the seal is a trace, not a `chmod`. It catches
  the honest drift and the lazy shadow contract; it does not stop a motivated
  actor, and it must never block the hot path.
- **Not premature infrastructure.** This ADR is the *naming*. Mechanism follows
  corroboration: when a galaxy demonstrably needs enforcement, it composes a
  formula and we amend — we do not pre-build the formula here.

## Consequences

- The federation gains a **shared name** for a failure mode every audit-bearing
  galaxy will hit, and a single place (§8b + this ADR) to read the discipline —
  instead of N galaxies each rediscovering "our audit report went stale before
  anyone read it."
- Audit verdicts across the fleet acquire a **shape contract**: `verdict @
  snapshot`. A snapshotless finding is now nameably inadmissible, which the
  L1–L5 fleet validator (ADR-097) can enforce as a coherence check without any
  new primitive — it already walks the event log and seals.
- `dave` (and sandbox, mailroom, lumen) reuse the existing
  BLAKE3 seal-as-trace primitive rather than each inventing a freeze mechanism.
  dave's ledger stays a dave ledger; the living-audit-subject handling lives
  in substrate where dirac put it.
- cosmon adds **zero** surface: no command, no daemon, no store, no migration.
  The molecule-layer instance (§8b seals + `cs verify`) already exists and is
  untouched; this ADR is the doctrine that says "that pattern is general — reuse
  it, do not re-derive it."
- The reflexive-ledger rule (#4) gives audit-formula authors a tested escape
  hatch — declare the loop, budget convergence (the same loop-until-dry shape as
  `latex-convergence` ADR-129) — instead of a one-shot verdict that the first
  corrective action falsifies.

## References

- `docs/architectural-invariants.md` §8b — briefing seals, the molecule-layer
  instance of this primitive (the seal mechanism, `cs verify`, FAIL/SKIP/PASS).
- ADR-097 — the fleet
  validator whose verdicts this ADR shapes.
- [ADR-130](130-notary-quarantine-consensus-layer.md) — the signature/seal
  primitive reused as the snapshot mechanism.
- [ADR-047](047-event-log-protocol-v0.md) — the append-only log, the
  immutable-snapshot substrate.
- `dave delib-20260508-a12d` (architect S2, identification) /
  `delib-20260508-1eef` (dirac, the *not-dave-ward* flag) /
  `task-20260508-b2c8` (the migration directive).
- Internal chronicles — 2026-06-25 entry, *"L'audit qui mesure une cible
  en mouvement."*
