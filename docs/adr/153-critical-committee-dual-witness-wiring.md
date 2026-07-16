# ADR-153 — Auto-wiring the critical cross-provider committee on a dual, separate, conjunctive witness

**Status:** Accepted (2026-07-12)
**Decision owner:** Noogram
**Origin:** C4 of `delib-20260711-c6c8`
**Depends on:** ADR-142 (Incarnation launch-time decision), ADR-147 (provider-family diversity witness), ADR-150 (directional routing policy / carrier parity, C1), ADR-151 (monotone criticality declaration, C2), ADR-152 (budget-aware SOR + authoritative receipt, C3), ADR-143 (cmb-diagnosis-verify gate)
**Blocks:** C5 (per-provider empirical calibration probe)

## Context

C1–C3 assembled the machinery a critical task needs: a directional routing
policy that *produces* a partial `Incarnation` (ADR-150), a monotone criticality
fold that says *how much assurance* a subject demands (ADR-151), and a pure
budget-aware router that *chooses a seat* and records why (ADR-152). ADR-147
added the missing axis for a *reading committee*: **provider-family
error-independence** — two seats may not resolve to the same
`(provider, base_url, model-family)` endpoint tuple, because a Claude auditing a
Claude is channel-independent yet error-*correlated*.

What was still missing is the **automatic wiring**: when the effective
criticality crosses the committee threshold, *convene the committee* — reusing
the existing `cross-provider-committee` formula, `cmb-verify`, and the
`Refutes`/`RefutedBy` edges — with no operator memory in the loop.

And convening exposed a structural hole in ADR-147's single witness:

- **Provider distinctness is necessary but not sufficient.** Two *different*
  providers, both handed the generator's confident prose as "the mechanism," both
  told to *confirm*, are still an echo of the same framing. Channel independence
  without **posture independence** is a costume. A committee that is
  provider-diverse but persona-uniform can rubber-stamp a wrong fix with a
  clean-looking diversity table.

- **The "generator family = galaxy default" fallback silently collapses witness
  (1).** If the generator's family is guessed rather than read from its registered
  Incarnation, a refuter tackled under the *actual* generator family reads as
  "distinct" when it is not.

## Decision

Roster admission for a critical committee requires **two separate witnesses,
joined conjunctively** — a seat sits only if BOTH pass, and the SOR (C3) may
bargain neither. The pure kernel is `cosmon_core::committee` (zero-I/O, like
`criticality` and `sor`):

1. **Witness (1) — provider-family** (`FamilyWitness`). The seat's resolved
   `EndpointTuple` (`provider_diversity::resolve_endpoint_tuple`, unchanged) is
   distinct from the generator's AND from every other admitted seat's. Carries the
   **§8b ceiling on the record** (`FamilyWitness::proxy_costume_ceiling`): the
   family label is config-derived, not attested, so a proxy-costume survives tier
   (a); binding family to an attested token is tier (b), the ADR-147 follow-on.

2. **Witness (2) — persona/role** (`PersonaWitness`). The seat plays a **distinct
   `role_id`** from the generator and every other seat, carries a **versioned
   adversarial briefing contract that was really injected**
   (`AdversarialBriefing::injected`, versioned by
   `ADVERSARIAL_BRIEFING_VERSION`), and ships a **falsification-attempt artefact**
   (proof it tried to break the fix, not merely read it). A declared-but-not-
   injected contract, or a missing falsification artefact, fails this witness even
   when the provider family is impeccably distinct.

`plan_committee(generator, refuters, requirement)` admits each refuter under both
witnesses, order-stably (a candidate is compared against the generator and the
seats admitted before it; the first of two colliding seats wins), and returns a
`RosterPlan` with the admitted roster, the typed rejects (each tagged with the
witness axis it failed — `FamilyCollision`, `PersonaCollision`,
`BriefingNotInjected`, `FalsificationArtifactMissing`), and whether the
distinct-family floor is met (`floor_met`; `false` is a **missing-seat**
finding).

### The SOR may not bargain a witness

C3's integer score ranks by quality, headroom, availability, and cost. That score
must **never** seat a witness-failed candidate. So admission runs *upstream* of
and *independent* from the router: only `RosterPlan::admissible_seat_ids` are ever
offered to `sor::select`. A witness-rejected seat is not a low score to
outweigh — it is a seat **not on the ballot** (`sor_may_not_resurrect`). A
budget-blocked *admissible* seat is a typed SOR refusal
(`NoAdmissibleCandidate`), never a silent fall-back to a rejected witness.

### No "generator family = galaxy default" fallback

The generator's family is read from `{{target}}`'s **registered Incarnation**. If
none exists, convene closes with a missing-input finding rather than guessing the
galaxy default.

### The conjunctive verdict-door, unchanged in spirit

`committee_verdict(outcomes)` folds seat outcomes: **refuted** if ANY seat
returned `Refuted` OR any falsifier went red; **confirmed** ONLY if there is at
least one seat and EVERY seat confirmed with no red falsifier; **inconclusive**
otherwise (including the empty jury). A door, not a vote — one lone refutation
among a hundred confirmations still refutes.

### Requirement derivation

`committee_requirement(level, bias)` maps the stake to a distinct-family floor
(`root` → 2, `security`/`max` → 3) raised — never lowered — by the config
`min_distinct_provider_endpoints` (max-join on the add-only lattice).

## Consequences

- **Positive.** The committee is auto-proposable from the C2 fold; the second
  witness closes the persona-uniform echo; the SOR cannot resurrect a rejected
  witness by any score; the no-fallback rule removes a silent witness-(1)
  collapse. All logic is pure and unit-tested (missing seat, budget-blocked seat,
  same-family alias, same-persona refuter, one refutation amid confirmations).

- **Negative / residual (named, not closed).** Tier (a) family is still
  config-derived — a proxy-costume survives (tier (b) `SameFamilyRefusal` is the
  follow-on). The `role_id` and the `injected` flag are *declared* facts the
  formula asks the convener to honour, not cryptographically attested; a motivated
  convener can lie about injection just as it can about family (same §8b ceiling).
  And the elasticity conserved by C2 — **stake self-classification** — is
  unclosed here too; only C5's empirical calibration probe polices it. This ADR
  makes the jury *loud and structured*, not incorruptible.

## Alternatives considered

- **A new `cs committee` command.** Rejected — a committee is a recipe over
  molecules, so it stays a formula (composability principle). C4 adds a pure core
  kernel the formula cites, not a command or daemon.
- **Fold the persona witness into the SOR `diversity_ok` flag.** Rejected — that
  would let the router's score interact with the witness. The witness must be a
  hard pre-filter upstream of the ballot, which is exactly `plan_committee` →
  `admissible_seat_ids` → `sor::select`.
- **Majority vote across seats.** Rejected — a vote lets a mono-posture crowd
  drown one true refuter, the failure the whole invariant exists to prevent. The
  door stays conjunctive.

## References

- `crates/cosmon-core/src/committee.rs` — the pure kernel + tests.
- `crates/cosmon-core/src/provider_diversity.rs` — witness (1) resolution (ADR-147).
- `crates/cosmon-core/src/criticality.rs` — the C2 fold (ADR-151) that triggers the wiring.
- `crates/cosmon-core/src/sor.rs` — the C3 router (ADR-152) the witness gates.
- `.cosmon/formulas/cross-provider-committee.formula.toml` — v2, the dual-witness recipe.
- `../showroom/docs/feedback/cosmon-directional-routing-sor.md` — the syzygie return-citation.
