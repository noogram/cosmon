# ADR-063 — Vocabulary: Orbitale / Nucléon / Noyau + Phase

**Status:** proposed
**Date:** 2026-04-23
**Parent deliberation:** `delib-20260422-f52c` (Wheeler §a/§b/§c, synthesis §D4/§S5).
**Authoring molecule:** `idea-20260422-c983`.
**Inherits:** [ADR-061](061-pilot-session-and-causal-closure.md) (Nucléon, `nucleon_id`, `pilot-session`).
**Blocks (cited by):** sibling `idea-20260422-8ec9` — future ADR on the
pilot-relativity principle and four game-theoretic invariants (Einstein §8e-extended
+ Von-Neumann §8g–§8i, plus tackle-exclusivity now carried as ratified prose;
§8f is the ratified two-plane rules). That ADR will cite this one for the names it uses.

---

## Context

ADR-061 named the cognition that causes a molecule to exist — the **Nucléon**
— and introduced `nucleon_id` as its reference identity. That closed one hole:
*who* nuclées.

Phase 2 of the substrate (post-"cosmon sort du nid", 2026-04-22) opens three
more holes the same metaphor already knows how to fill:

1. A Nucléon manifests on **devices** (Tailnet mesh: MacBook, iPad-Blink,
   iPhone-Blink, AWS node). Those devices are not the pilot; they are where
   the pilot can appear.
2. Nucléons bound by operational trust form a **community**. A fleet is owned
   by this community, not by one pilot.
3. Nucléons will, eventually, run on **different cognitive substrates**
   (biological today, LLM-frontier soon, world-model / Noogram-self later).

If we do not ratify the three names now, each downstream ADR, chronicle and
CLI will invent its own synonym (*mesh*, *cercle*, *tribu*, *tenant*,
*backend*, *kind*) and the system will speak three dialects within a month.
Synthesis §D4 already flagged the *cercle ⇄ Noyau* drift in the panel itself —
Godin used *cercle* five times, Wheeler proposed *Noyau*, no one resolved.

The panel converged on the atomic metaphor Cosmon already uses:
**Orbitale ⊂ Nucléon ⊂ Noyau ; Nucléon a une Phase.** This ADR ratifies
those four names — three system terms and one explicit placeholder — without
writing Rust.

---

## Decision

### (1) Orbitale — the family of trusted devices

The **Orbitale** of a Nucléon is the set of devices on which the pilot may
manifest: MacBook, iPad-Blink, iPhone-Blink, AWS node, any future Tailnet
peer.

Wheeler §a, verbatim:

> *En physique atomique, l'orbitale est l'enveloppe de positions accessibles
> à un électron lié au noyau. Elle n'est pas l'électron ; elle est où il peut
> se manifester. Exactement le statut d'un iPad Blink : ce n'est pas le
> pilote, c'est le lieu où le pilote peut apparaître. Le mot dit :
> appartenance à un Nucléon, quantique (dedans ou pas), géométriquement
> distribuée. Il ne dit pas `device`, `mesh`, `Tailnet` — implémentations.*

An Orbitale is a property *of* a Nucléon. It is not an independent type.
In code, it is expressible as a membership relation: the Tailnet ACL whose
members map to the current Nucléon's `nucleon_id`. The name does not
prescribe an implementation; future ADRs may realise it via Tailscale,
Wireguard, SSH keys, or any other trust substrate.

### (2) Noyau — the family of trusted humans

A **Noyau** is a finite set of Nucléons in an operational trust relation —
the right to emit sparks in the same fleet.

Wheeler §b, verbatim:

> *Plusieurs Nucléons en relation de confiance forment un Noyau. Le mot est
> déjà dans la métaphore : un noyau est un ensemble lié de nucléons. La force
> de liaison physique devient, ici, la relation opérationnelle de confiance —
> le droit d'émettre des étincelles dans le même fleet. Un fleet appartient
> à un Noyau ; le Noyau est l'ensemble fini des nucleon_id admis.*

A fleet *belongs to* a Noyau. Admission to the Noyau is the human-readable
name for the act of adding a `nucleon_id` to the fleet's trust roster. The
Noyau is a **list**, not a **hierarchy**: it has members, not rôles. RBAC is
explicitly out of scope. A Noyau of one (solo pilot) is valid and is the
default shape of a galaxy today.

### (3) Phase — placeholder field on Nucléon

A Nucléon carries a **Phase**: a label for the cognitive substrate in which
its pilot-cognition is currently running. Biological, LLM-frontier, mixed
(augmented-human), and future substrates (world-model, Noogram-self) are
**phases** the same way solid/liquid/gas/plasma are phases of matter.

Wheeler §c, verbatim:

> *Je refuse de nommer définitivement aujourd'hui. Fixer un mot pour
> biologique / LLM-frontier / world-model / Noogram-self, c'est pré-déclarer
> la taxonomie avant que les trois dernières existent — la trahison exacte
> du propose-don't-impose. Placeholder : Phase. Chaque substrat admis est
> une phase dans laquelle un Nucléon peut incarner sa cognition (comme la
> matière a des phases : solide, liquide, gaz, plasma). Observable
> (`nucleon_phase: biological`), pas gravée dans le type. Un humain augmenté
> d'un world-model n'est pas deux Nucléons — un Nucléon en phase mixte.
> Quand le quatrième substrat apparaîtra, il recevra un nom de phase sans
> changer l'ontologie.*

**Phase characterises; it does not imbricate.** A Nucléon *has* a Phase the
way an electron has a spin. A Nucléon does not live *inside* a Phase, nor is
Phase a parent type. This is the single most important rule of this ADR —
it is what keeps the vocabulary open to substrates we cannot yet imagine.

### (4) Nesting

The three terms nest through the same root (the nucleus):

```
Orbitale ⊂ Nucléon ⊂ Noyau ;   Nucléon has Phase.
```

— a device belongs to a Nucléon; a Nucléon belongs to a Noyau; a Nucléon
*has* a Phase. The nesting is lexically obvious: *"admit an Orbitale to the
Noyau"* reads without a glossary because the terms share the atomic root.

### (5) Rendered in code — minimal admissible form

This ADR ships zero code. When the implementation sibling lands, the
type-level realisation **must** stay within the minimum admissible shape:

```rust
// On MoleculeData (or a dedicated NucleonData sidecar — implementation
// choice deferred to the sibling ADR that lands the code).
pub struct NucleonIdentity {
    pub id: NucleonId,             // from ADR-061
    pub phase: NucleonPhase,       // from this ADR
    // orbitale / noyau are RELATIONS, not fields — resolved via Tailnet
    // membership and the fleet's trust roster respectively.
}

pub enum NucleonPhase {
    Biological,
    LlmFrontier,
    // Mixed(...), WorldModel, NoogramSelf — NOT declared today.
    // Adding variants before the substrate exists is pre-commitment
    // and violates propose-don't-impose (§8b invariants).
}
```

Two variants only. The enum must not pre-declare substrates that have no
existing implementation — adding `WorldModel` before a world-model Nucléon
can run cognition would pre-shape the taxonomy and defeat the placeholder.
`Mixed(...)` is deferred until a concrete use case surfaces (augmented human
with a world-model co-thinker) and can be specified without guesswork.

---

## Rejected alternatives

For the **device** family (part 1):

| Rejected | Reason |
|----------|--------|
| `mesh` | Names the implementation (Tailscale), not the conceptual role. |
| `device` | Too generic; does not say "belonging to a Nucléon." |
| `Tailnet` | Brand-coupled; fails if the substrate changes. |

For the **trust community** family (part 2) — Wheeler §b:

| Rejected | Reason |
|----------|--------|
| `cercle` | Sociological, carries no force-of-binding. Allowed as prose synonym (see §Discipline below), not as system term. |
| `tribu` | Same as *cercle*, plus anthropological baggage. |
| `communauté-nucleon` | Tautological hyphenation; breaks lexical cohesion. |
| `organisation` | Imports RBAC ontology we do not have. |
| `tenant` | SaaS term; drags in multi-tenancy expectations that cosmon deliberately rejects. |

For the **substrate** family (part 3) — Wheeler §c:

| Rejected | Reason |
|----------|--------|
| `Substrate` as a type | Pre-declares taxonomy before substrates exist. Same reason `Subject` is not a thing. |
| Four-variant enum today (`Biological`, `LlmFrontier`, `WorldModel`, `NoogramSelf`) | Three of the four have no referent on disk; declaring them is pre-commitment. |
| Substrate-as-parent-type (Nucléon inherits from Substrate) | Violates the characterising-not-imbricating rule. Two Nucléons in the same Phase are still two Nucléons, not one. |

---

## Discipline for surface prose

**Noyau is the system term. Cercle is allowed as prose synonym.**

- Code, CLI flags, Rust type names, ADR bodies, event payloads, and all
  strict-vocabulary contexts use **Noyau** (e.g. `cs fleet admit --noyau`,
  `NoyauId` if a type ever lands, *"the Noyau of this fleet"*).
- Chronicles, narrative docs, handbook side-paragraphs, and speech may use
  **cercle** when the prose reads better with it (e.g. Godin's *"le cercle
  est informé par le fait"* in the first-veillée ceremony, preserved as
  written).
- Translator table: `cercle` ≡ `Noyau` colloquially; never `Nucléon`, never
  `Orbitale`. No other synonyms are authorised.

The discipline matches physics itself: *"le noyau"* is the term of art;
*"l'atome"* is the colloquial-but-precise-enough synonym. Mixing registers
is fine as long as the register stays legible.

This resolves synthesis §D4.

---

## Coherence checklist (invariants §5)

Passes by construction — vocabulary is regime-invariant, command-less,
idempotent-null. Scope-bounded (three disjoint levels: device / cognition
/ community), self-similar (composes at solo, multi-device, multi-human),
alphabet-closure respected (any future code introducing `NucleonPhase`
lands with the TLA+ spec edit in the same commit, same rule as ADR-061).
Nothing in this ADR contradicts an existing invariant.

---

## Consequences

### Positive

- **Vocabulary settled before divergence.** Three terms ratified before
  any of the downstream ADRs (sibling `idea-20260422-8ec9`, future
  `cs spark`, future `matrix-echo-tick`, future `cs pilot welcome`) has
  to choose for themselves.
- **Propose-don't-impose on substrate taxonomy.** The Phase placeholder
  lets Nucléon admit a fourth cognitive substrate (world-model,
  Noogram-self, something not yet imagined) by adding an enum variant —
  not by reshaping the type hierarchy.
- **Syzygie anchors.** Mailroom and showroom inherit citable terms
  when they adopt the same metaphor.

### Neutral / accepted costs

- **One new enum variant expected** when LLM-frontier Nucléons start
  nucleating in production. `NucleonPhase::LlmFrontier` is declared today
  as an anticipatory lexical slot; the behaviour that backs it ships
  with the sibling ADR.
- **The discipline *cercle ≡ Noyau* is a soft contract.** Enforcement is
  by review, not by type. A slip in a chronicle is a chronicle bug, not
  a system bug.

### Negative (risks)

- **Premature Phase variant temptation.** A future contributor may want
  to add `WorldModel` when a paper or demo appears. The rule is: do not
  declare a Phase variant until a Nucléon in that Phase can run
  cognition against `.cosmon/` and leave durable artifacts there. The
  enum is closed today with `Biological` and `LlmFrontier`; any PR
  adding a third variant must cite a running instance.
- **Noyau-as-list is finite.** If cosmon ever needs rôle-hierarchy
  (cercles-within-cercles, admin-vs-member), the Noyau term would
  become load-bearing for a feature it was explicitly named *not* to
  carry. The remediation is to write a successor ADR; the Noyau stays
  flat until then.

---

## Scope and non-scope

**In scope.** Ratifying three system terms (Orbitale, Noyau, Phase) and
preserving Nucléon (ADR-061) unchanged. Declaring the nesting. Listing
rejected alternatives. Establishing the prose discipline *Noyau* (system)
vs. *cercle* (allowed synonym). Vocabulary pass across `CLAUDE.md`,
`docs/vocabulary.md`, `docs/handbook.md` in the same commit.

**Out of scope.** Rust type introductions (`NoyauId`, `OrbitaleId`,
`NucleonPhase` enum) — those land with sibling `idea-20260422-8ec9` or a
later implementation ADR. Renaming existing code, CLI flags, or fields.
Phase variants beyond `Biological` and `LlmFrontier`. Re-opening ADR-061.
UI/UX for multi-Noyau galaxies.

---

## Acceptance

This ADR is **proposed**. Operator ratification is the explicit next step.
Until ratified:

- The briefing seals in the sibling ADR-062 (when it lands) treat the four
  names as *provisional-but-citable*.
- No code adoption is required — ADR-061's existing `nucleon_id` usage
  continues unchanged.

Wheeler's closing line:

> *"Behind it all is surely an idea so simple, so beautiful..." — ici,
> l'idée simple est que cosmon tient déjà, depuis le premier jour, une
> métaphore atomique cohérente. Je ne l'étends pas ; je la déplie.*

The ADR does exactly that: it unfolds a metaphor already present and
refuses to name what the system cannot yet observe.
