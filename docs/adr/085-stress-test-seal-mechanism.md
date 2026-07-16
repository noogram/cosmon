# ADR-085 — Stress-test seal mechanism (third-party seal for `deep-think` disconfirming-observation molecules)

**Status:** Accepted
**Date:** 2026-05-04
**Origin:** mailroom-galaxy molecule `idea-20260504-9f11` — cosmon-ward escalation per [ADR-049](049-cosmon-ward-feedback-flow.md)
**Triggering precedent:** `delib-20260503-5a74/dispatch-decision.md` — first audit-trail of a `STEP2_GATE_BLOCKED` bypass
**Related:**
[ADR-027](027-gate-molecules.md) (gate molecules — typology),
[ADR-032p](032-p-external-witness-axiom.md) (external-witness axiom),
[ADR-034](034-witness-charter-v0-protocol.md) (witness-charter v0),
[ADR-049](049-cosmon-ward-feedback-flow.md) (cosmon-ward feedback flow — canonical channel),
[ADR-052](052-one-ledger-one-writer-one-witness.md) (one-writer / one-witness pattern),
[ADR-082](082-architecture-baseline.md) (INV-ADR-OPTIONS-CONSIDERED rule).

## Context

Cosmon's `deep-think` formula (multi-perspective panel deliberation)
sometimes serves as a **stress-test** of a hypothesis or a meta-frame:
the operator pre-commits to *"the position(s) I would most hate to see
overturned"* in a sealed `prior.md`, and the panel verdicts are then
binding evidence against (or for) those pre-commitments. The
disconfirming-observation predicate Janis specified in
`delib-20260503-af60/responses/janis.md` §3 reads:

> at least one of those three pre-committed positions is genuinely
> retracted or sharply qualified in the synthesis, with the retraction
> reproduced verbatim in operator-facing chronicle.

That predicate is the **falsifiability machinery** of stress-test
deliberations. Without a sealed prior, the synthesizer has post-hoc
latitude to declare any axiom "qualified" against whichever wording
happens to coincide with the panel's softest bruise. The seal is the
chess-envelope.

### The triggering incident

`delib-20260503-5a74` ran a stress-test of the founding meta-frame
(A1–A4). The seal was **bypassed**:

1. Step 2 of the formula refused dispatch and emitted
   `STEP2_GATE_BLOCKED.md` (the gate notice).
2. The operator answered *"continue"* in the same conversation window.
3. The worker authored `dispatch-decision.md` recording the methodological
   consequence (the predicate cannot be evaluated by Step 3) and proceeded
   to dispatch the panel.
4. The synthesis carried the caveat ; Janis's protocol-verdict was
   `retracted`.

This is the **closed-loop-without-oracle** failure mode named in
mailroom's `THESIS.md` kill-switch list, transposed from execution
to deliberation. The molecule's worker is the synthesizer is the actor
that would have authored the operator's prior — there is no actor in
the loop *whose interests are not served by continuation*. The override
happened in one breath, in one line, in one file, in one worktree.

The cosmon-ward feedback flow (ADR-049) mandates : when an
application-site galaxy (here mailroom) discovers a cosmon-level
pathology — a broken invariant or a missing primitive — surface it back
to cosmon as a typed molecule rather than silently patch the
application. The pathology *is* in cosmon : the runtime offers no
enforcement of pre-commitment seal for stress-test class molecules.

### What this ADR ratifies

The seal mechanism is **not a constraint imposed on every `deep-think`
deliberation**. Tactical exploratory deliberations (the typical case)
do not require it. The seal applies *only* to the explicitly-declared
**stress-test class**, which the operator must mark at nucleation. The
ADR defines :

1. The class-declaration syntax at `cs nucleate`.
2. The two-layer seal (runtime precondition + witness-quorum) that
   gates dispatch for stress-test molecules.
3. The cross-galaxy escalation reserved for re-runs of previously-bypassed
   molecules.
4. The typed audit artefact replacing the ad-hoc `dispatch-decision.md`.

## Options Considered

Per [ADR-082 INV-ADR-OPTIONS-CONSIDERED](082-architecture-baseline.md),
this section is the load-bearing test of the proposal.

### Option (a) — Cosmon-runtime precondition only (rejected as standalone)

Single runtime guard in `cosmon-runtime/src/guard.rs`. `cs nucleate
deep-think` and `cs tackle` refuse dispatch if `frame.md` contains the
canonical keywords (`disconfirming-observation`, `stress-test`) and no
`prior.b3` exists.

- **Pros :** Cheapest (~80–150 LoC). Slots into `guard.rs` perimeter
  precedent (ADR-048). No new infrastructure.
- **Cons :** Single-observer — the operator owns the override knob
  (runtime config edit) ; no adversarial multi-actor record. The
  failure mode at the heart of the triggering incident — *the operator
  answers "continue" and the gate bends* — is reduced but not closed,
  because the runtime config repo and the worktree are owned by the
  same operator.
- **Why rejected as standalone :** Symptomatic, not curative. Reduces
  bypass cost but does not change its character.

### Option (b) — Witness-quorum seal only (rejected as standalone)

`prior.b3` is committed by the operator ; dispatch then requires a
counter-signature from a separate cosmon `witness` agent that attests
the seal (timestamp + hash) without reading `prior.md`. Inherits
[ADR-052](052-one-ledger-one-writer-one-witness.md).

- **Pros :** Two-actor minimum. Witness has zero read access ; its
  attestation is structurally independent of the prior's substance.
  Bypass requires forging a witness signature.
- **Cons :** Without (a), the operator must remember to invoke the
  witness — the same memory failure that produced the triggering
  incident in the first place. The gate fires only after the operator
  has already produced the seal ; if the operator skips both gestures
  (no seal, no witness), nothing fires.
- **Why rejected as standalone :** Without (a), there is no
  forcing-function at nucleation — only at the next gesture the
  operator may or may not perform.

### Option (c) — Cross-galaxy second-chance only (rejected as default)

When a stress-test molecule reaches dispatch without `prior.b3`, the
runtime opens a 24-hour window and notifies a peer galaxy with
seat-of-veto. Silence → dispatch proceeds with permanent unsealed-run
record. Veto → operator review.

- **Pros :** Maximum integrity. True multi-observer — the witness
  lives where cosmon does not write. Implements the multi-observer
  envelope per mailroom's oracle-boundary distinction
  (`mailroom/CLAUDE.md` §*Oracle Boundary*).
- **Cons :** 24h is incompatible with operator-driven exploratory
  deliberations (the typical case for `deep-think`). Inter-galaxy
  notification with veto semantics does not yet exist as a primitive
  (ADR-035 names cross-galaxy edges but not vetoes). ~600–1000 LoC of
  new infrastructure.
- **Why rejected as default :** The latency cost is a hard refusal of
  operator cadence. Adopted only as the **escalation tier** for re-runs
  of previously-bypassed molecules.

### Option (d) — Dispatch-time prompt rather than seal (rejected)

Replace the seal with an interactive prompt at dispatch ("type your three
pre-committed positions now"). No filesystem seal ; the worker captures
the operator's answer.

- **Pros :** Zero infrastructure. The operator cannot proceed without
  typing.
- **Cons :** The captured answer lives inside the worker's context
  window — the same actor that synthesizes the verdict. *This is
  exactly the closed-loop-without-oracle pathology* the seal is meant
  to prevent. Worker capture and worker arbitration are the same
  operation.
- **Why rejected :** Rebuilds the failure mode under a different name.

### Option (e) — Gate as a separate cosmon molecule (rejected)

Per ADR-027, gate molecules are first-class DAG nodes. A `seal-gate`
molecule could sit between `frame` and `dispatch` of the deep-think
DAG, refusing dispatch until `prior.b3` exists.

- **Pros :** Reuses ADR-027 typology ; the gate is observable in
  `cs deps` ; failure cascades through `cs collapse`.
- **Cons :** A gate molecule executes as a subprocess and signals
  pass/fail. It cannot capture a witness signature in a separate
  session. It also cannot block at the *runtime* layer (the runtime
  walks past gate molecules just like any other node ; the walker
  would still need a precondition that the gate molecule has been
  signed). This option is **complementary to (a)+(b)** — a deferred
  refinement — but not a replacement.
- **Why rejected as standalone :** Insufficient ; collapses to (a)
  without the witness component.

### Option (f, chosen) — Hybrid (a) + (b), with (c) as escalation tier and (e) as a deferred refinement

The shape this ADR ratifies. Dispatch-time gate composed of two
layers that fire in sequence ; cross-galaxy second-chance reserved for
re-runs ; gate-molecule typology (e) deferred to a follow-up ADR if and
when the seal mechanism graduates to first-class DAG visibility.

**Decision Outcome :** Option (f). Options (a)/(b)/(c)/(d)/(e) are
explicitly rejected as standalone above ; (a) and (b) are adopted as
composing layers of (f). (c) is adopted as the escalation tier triggered
by re-runs. (d) is rejected outright. (e) is deferred.

## Decision

### 1. Class declaration at nucleation (mandatory)

Stress-test molecules are declared explicitly at `cs nucleate`:

```sh
cs nucleate deep-think \
  --class stress-test \
  --var question="..." \
  --var panel="janis,popper,godin,..."
```

The `--class stress-test` flag :

- Sets `MoleculeData.class = "stress-test"` (new optional field).
- Activates the seal precondition (layer 1) at `cs tackle`.
- Activates the witness-quorum requirement (layer 2) before dispatch.
- **Opts the molecule out of autopilot-by-default** for any tag
  (`temp:hot` or otherwise) — the molecule will not be auto-tackled by
  `cs ensemble` or `cs run` until both seal layers have fired.

A keyword-detection fallback runs after `frame.md` is authored : if
`frame.md` contains the canonical phrases
*"disconfirming-observation"* or *"stress-test"* (case-insensitive,
exact match) and `--class` was not declared at nucleation, the runtime
**warns and prompts the operator to either set the class or remove the
keywords**. The fallback is a safety net, not the primary path. Forcing
explicit declaration at nucleation is what ratifies the operator's
intent.

### 2. Layer 1 — runtime precondition (`check_prior_seal`)

A new pure-read predicate in `cosmon-runtime/src/guard.rs`,
following the ADR-048 `check_backlog` template :

```text
A stress-test molecule is dispatchable iff:
  1. MoleculeData.class == "stress-test"
  2. ∃ <molecule_dir>/prior.md
  3. ∃ <molecule_dir>/prior.b3 (BLAKE3 hash of prior.md, sealed_at <
     dispatch_time)
  4. Seal attestation (layer 2) exists and references prior.b3
```

If any condition fails, dispatch is refused with a typed
`GuardError::SealMissing { stage: "runtime-precondition", ... }`,
exit code `13` (chosen to extend the ADR-048 family : 12 = dirty
backlog, 13 = missing seal).

Override path : **none at the runtime layer**. The override path lives
in (3.5) below — and it requires a typed `BypassReceipt`, not a
free-text *"continue"*.

### 3. Layer 2 — witness-quorum seal (`cs witness attest`)

A new CLI subcommand : `cs witness attest <molecule_id>`.

**Constraints on the witness session :**

- The witness session is spawned in a **separate tmux session** from
  the molecule's worker. The CLI refuses if invoked from the same
  tmux session as the molecule's worker (cheap heuristic) ; the
  hardened path runs the witness as a LaunchAgent (separate process
  tree) — deferred to a follow-up implementation molecule.
- The witness reads only `<molecule_dir>/prior.b3` (the BLAKE3 hash)
  and `<molecule_dir>/state.json` (timestamp). **It does not read
  `prior.md`**. This is the structural independence guarantee.
- The witness emits a `SealAttestation` to a separate per-fleet ledger
  `<.cosmon/state/fleets/<fleet>/seals.jsonl>`, and an
  `EventV2::SealAttested { molecule_id, prior_hash, sealed_at,
  attested_at, witness_session_id }` to the event log.

The dispatch precondition (layer 1, condition 4) requires a matching
`SealAttestation` whose `prior_hash` matches the on-disk `prior.b3` and
whose `attested_at >= sealed_at`.

### 3.5. Bypass receipt (typed audit artefact)

When the operator chooses to bypass the seal — a real possibility for
genuine emergencies — the override is structured, not free-text. The
operator runs :

```sh
cs nucleate deep-think \
  --class stress-test \
  --bypass-seal \
  --bypass-reason "<one-line reason>"
```

Effects :

- A `BypassReceipt` is written to `<molecule_dir>/bypass-receipt.json`
  containing : actor, reason, timestamp, BLAKE3 hash of `frame.md`,
  reference to the layer-1 condition that was bypassed.
- An `EventV2::SealBypassed { molecule_id, actor, reason, ... }` is
  emitted.
- The molecule is **flagged for cross-galaxy escalation** on its next
  re-run (see §4).
- The synthesis MUST carry a *"bypassed seal"* caveat at the top of
  `synthesis.md` (worker-enforced ; gate-checked by `delib-prep` lint).
- The molecule's per-fleet record is permanent — it cannot be deleted
  without a new ADR.

This replaces the ad-hoc `dispatch-decision.md` artefact (as produced
by `delib-20260503-5a74`). The receipt is **structured, queryable, and
linked to the event log** ; ad-hoc prose written into the molecule's
worktree is no longer admissible as a bypass record.

### 4. Layer 3 — cross-galaxy second-chance (escalation tier, re-runs only)

Reserved for re-runs of a stress-test molecule whose previous instance
emitted a `BypassReceipt`. When such a molecule is re-nucleated, the
runtime :

1. Detects the prior bypass via lineage check
   (`MoleculeData.typed_links` chain back to a previously-bypassed
   ancestor).
2. Opens a 24-hour window during which the dispatch is held.
3. Notifies a peer galaxy via `cosmon-matrix-tick` (the inter-galaxy
   channel named in [ADR-035](035-cross-galaxy-edges.md)).
4. The peer galaxy may emit a `Veto` event ; silence after 24h
   ratifies dispatch with a permanent unsealed-run record.

The peer-galaxy mapping defaults to :

| Stress-test originating galaxy | Peer witness galaxy |
|---|---|
| `mailroom` | `cosmon` |
| `cosmon` | `demo-squad` |
| `demo-squad` | `cosmon` |
| `(other)` | `cosmon` |

This mapping is a configuration, not hardcoded ; cosmon-managed galaxies
declare their peer-witness in their per-galaxy fleet config.

**Deferred to a follow-up implementation molecule.** This ADR commits
to the design ; the inter-galaxy notification primitive does not yet
exist with veto semantics and is the pacing constraint.

### 5. The trade-off, named

Stress-test molecules are slowed by **seconds-to-minutes** under the
default path (a)+(b) and by **24h** under the escalation path (c).
This is by design and is **incompatible with autopilot-by-default**
for this class.

The operator must declare class explicitly at nucleation. There is no
"stress-test that drains."

This is the load-bearing operator commitment the seal mechanism asks
for ; without it, no seal mechanism is meaningful. It is consistent
with the mailroom autopilot-default memory
because that memory governs *tactical* molecules ; the stress-test
class is explicitly carved out by class declaration.

### 6. Entropy of the override path

The bypass cost rises along three axes :

| Layer fired | Bypass cost | What the audit records |
|---|---|---|
| Layer 1 only (runtime precondition) | seconds — operator types `--bypass-seal --bypass-reason "..."` | `BypassReceipt` + `EventV2::SealBypassed` |
| Layer 1 + Layer 2 | infeasible without forging witness signature | a forged `SealAttestation` would itself be an event-log anomaly |
| Layer 1 + Layer 2 + Layer 3 (re-run) | 24h + peer-galaxy notification + permanent unsealed-run record | cross-galaxy event chain |

The operator may always bypass at Layer 1 with a typed receipt. They
cannot bypass at Layer 2 without forging a witness signature (which
would itself be an event-log anomaly). They cannot bypass Layer 3
without a peer-galaxy veto-collusion that lives in another galaxy's
git history.

This is the **proportional-escalation** discipline : the cost of
bypass scales with the audit-trail of prior misuse.

## Decision Drivers

- **D-CLOSED-LOOP-PREVENTION** — The
  closed-loop-without-oracle failure mode named in mailroom's
  `THESIS.md` kill-switch list must not propagate from execution into
  deliberation. The seal interrupts the loop with a witness whose
  interests are not served by continuation.
- **D-COSMON-WARD-CANONICAL** — This ADR is a verbatim instance of
  the cosmon-ward feedback flow ([ADR-049](049-cosmon-ward-feedback-flow.md))
  : an application-site galaxy (mailroom) discovered a primitive
  missing in cosmon and surfaced it back as a typed contribution.
- **D-PROPORTIONAL-ESCALATION** — Bypass cost scales with audit-trail
  of prior misuse. The first bypass is structured and recorded ; the
  re-run is cross-galaxy.
- **D-STRUCTURAL-INDEPENDENCE** — The witness reads the prior's hash,
  not its content (ADR-052 inheritance). The attestation is therefore
  independent of the prior's substance.
- **D-OPT-IN-CLASS-DECLARATION** — The seal applies only to the
  explicitly-declared stress-test class. Tactical deliberations are
  unaffected. Class declaration is operator-intentional ; keyword
  fallback is a safety net.
- **D-AUTOPILOT-CARVE-OUT** — Stress-test class is explicitly
  excluded from autopilot drain. The two disciplines are
  reconciled by class declaration, not by silent priority.

## Consequences

### Positive

- The disconfirming-observation predicate (Janis §3) becomes
  enforceable. Panel verdicts against a sealed prior are binding
  evidence.
- Bypass becomes adversarial : two-actor minimum, structured receipt,
  cross-galaxy escalation on re-run.
- Audit-trail is structured (`BypassReceipt` JSON + `EventV2`
  variants), not free-text prose in worktrees.
- Operator's autopilot discipline is preserved for tactical
  molecules ; the friction is paid only for the class that needs it.

### Negative / costs

- Stress-test molecules cost seconds-to-minutes (typical) or 24h
  (re-run after bypass) at dispatch.
- Operator must remember to declare `--class stress-test` at
  nucleation. The keyword fallback warns ; it does not auto-promote
  (intentional, see D-OPT-IN-CLASS-DECLARATION).
- New surface area in `cosmon-runtime` (~80–150 LoC),
  `cosmon-cli` (~100 LoC : `cs witness` subcommand + `--class` /
  `--bypass-seal` flags), and `cosmon-core` (~50 LoC : new event
  variants). Total budget : ~300 LoC for layers (a)+(b).

### Risks named (with mitigations)

1. **Keyword evasion.** A future formula author may phrase the framing
   without canonical keywords. *Mitigation :* a `delib-prep` lint that
   detects deliberation framings missing the canonical class
   declaration ; the keyword set is a typed `StressTestKeywords`
   enum maintained alongside this ADR.
2. **Witness session collusion.** Both witness and worker may be
   spawned by the same operator. *Mitigation :* same-tmux refusal
   (cheap) ; LaunchAgent witness (hardened, deferred).
3. **Operator override fatigue.** If the seal fires on too many
   molecules, the operator will learn to bypass reflexively.
   *Mitigation :* opt-in class declaration ; the seal fires only on
   explicitly-declared stress-test class.
4. **Cross-galaxy infrastructure debt.** Layer 3 requires inter-galaxy
   notification with veto semantics that does not yet exist.
   *Mitigation :* (c) is deferred to a follow-up implementation
   molecule ; (a)+(b) ship first.
5. **Seal-bypass normalization.** Once `--bypass-seal` exists, the
   operator may use it routinely. *Mitigation :* every bypass triggers
   cross-galaxy escalation on the next re-run, raising the price of
   reuse.

## Implementation plan (next molecules)

The implementation graph below is meant to be nucleated as **separate
molecules** after this ADR is accepted. None of them is auto-spawned
by this ADR. Effort is sized in molecules (per the mailroom
no-time-estimates memory)
— never in calendar units.

```text
ADR-085 (this)
   │
   ├── M1: cosmon-core schema (MoleculeData.class field +
   │       EventV2::{SealAttested, SealBypassed} variants +
   │       BypassReceipt struct)
   │
   ├── M2: cosmon-runtime guard (check_prior_seal — Layer 1)
   │       └── needs M1
   │
   ├── M3: cs witness attest CLI (Layer 2)
   │       └── needs M1
   │
   ├── M4: cs nucleate --class / --bypass-seal flags
   │       └── needs M1, M2, M3
   │
   ├── M5: delib-prep keyword lint
   │       └── needs M1
   │
   ├── M6 (deferred): LaunchAgent witness hardening
   │       └── needs M3
   │
   └── M7 (deferred): cross-galaxy second-chance (Layer 3 — option c)
           └── needs M4, M5
```

After M1–M5 ship, this ADR's status moves from `Proposed` to
`Accepted` ; the triggering precedent (`delib-20260503-5a74`) becomes
eligible for re-run under the new mechanism. The re-run itself is a
separate operator decision, independent of this ADR.

## Citations

- Originating molecule (capture + feasibility) — internal idea notes (idea-20260504-9f11 seal-mechanism-stress-test + feasibility).
- Triggering precedent :
  - `/srv/cosmon/mailroom/.cosmon/state/archive/2026/05/delib-20260503-5a74/responses/janis.md` §3-§4
  - `/srv/cosmon/mailroom/.cosmon/state/archive/2026/05/delib-20260503-5a74/synthesis.md` §T4 + §C4 + §S4
  - `/srv/cosmon/mailroom/.cosmon/state/fleets/default/molecules/delib-20260503-5a74/dispatch-decision.md`
- Cosmon ADR scaffolding :
  - [ADR-027](027-gate-molecules.md) (gate molecules — typology)
  - [ADR-032p](032-p-external-witness-axiom.md) (external-witness axiom)
  - [ADR-034](034-witness-charter-v0-protocol.md) (witness-charter v0)
  - [ADR-035](035-cross-galaxy-edges.md) (cross-galaxy edges)
  - [ADR-048](048-backlog-sanity-invariant.md) (the `guard.rs` precondition pattern this seal extends)
  - [ADR-049](049-cosmon-ward-feedback-flow.md) (the canonical channel for this ADR)
  - [ADR-052](052-one-ledger-one-writer-one-witness.md) (witness pattern)
  - [ADR-082](082-architecture-baseline.md) (INV-ADR-OPTIONS-CONSIDERED rule honoured above)
- Application-site discipline anchors :
  - `/srv/cosmon/mailroom/THESIS.md` (kill-switch enumeration — closed-loop-without-oracle)
  - `/srv/cosmon/mailroom/CLAUDE.md` §*Oracle Boundary*, §*Kill-Switch*
  - `~/.claude/CLAUDE.md` §*Core Rules* (cosmon-ward feedback flow)
