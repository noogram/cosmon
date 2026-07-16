# ADR-139 — `spore`: the shareable, parameterizable template of a whole polymer

**Status:** Accepted (2026-06-26).
**Date:** 2026-06-26.
**Decider:** Noogram (operator canonisation).
**Source deliberation:**
`delib-20260626-951f`
— *« Quel terme doit nommer et définir la nouvelle primitive cosmon : un
template de polymère réutilisable, partageable, validé TLA+ — un DAG dynamique
de molécules paramétrable — qui livre une mission raffinable ? »* Panel of five
personas (wheeler · torvalds · jobs · knuth · shannon). Three of five
converged independently on `spore` from three unrelated lenses (product-feel,
literate-readability, information-density); the Q5 verdict (name the structure,
demote proof and portability to properties) was unanimous among the personas
who addressed it.

**Scope discipline.** This ADR fixes the **ontology and the file format**, not
the code. `cs spore run` and the pure `expand(spore, params)` function are
specified in §Implémentation (future) and are explicitly **not built** here.
No code lands in this commit; this is a doc-only canonisation.

**Builds on (realize, do not reinvent):**
- ADR-004 — `formula` as the
  immutable template of ONE molecule; the unit-of-work distinction this ADR
  scales up by one level.
- [ADR-026](026-dynamic-fleet-orchestration.md) — dynamic fleet / DAG
  orchestration; the live runtime a germinated spore replays into.
- [ADR-038](038-fleet-composability-v0.md) /
  [ADR-039](039-fleet-composability.md) — fleet composition and content-
  addressed `FleetId`; the crew half of a spore and the sharing substrate.
- [ADR-111](111-mission-convention-existing-primitives.md) — a **mission** is
  the live instance (convention over `Molecule` + `Blocks` + `DecayProduct`,
  *not* a runtime). A spore is the *recipe* of a mission; germinating a spore
  yields a mission. This ADR is ADR-111's verdict applied one scale up.

---

## Context

cosmon's template/instance table has a hole. `formula` is the immutable
template of **one** molecule (ADR-004); nucleating a formula yields one
`molecule`. A `polymer` is a growing DAG of molecules, and its live, named
form is a `mission` (ADR-111). But there is **no named template of a whole
polymer** — no reusable, parameterizable, shareable, TLA+-validated recipe
that germinates into a mission and stays refinable by whoever receives it.

|                | template (immutable) | instance (lives / dies)   |
|----------------|----------------------|---------------------------|
| **one unit**   | `formula`            | `molecule`                |
| **whole DAG**  | **`spore`** ← *new*  | `polymer` / `mission`     |

The empty cell is the *template of a polymer* — the slot the operator named.
It sits between `formula` (template of ONE molecule) and `fleet` (the live
crew): a fleet carries the workers, a formula carries one node's recipe, and
nothing carries the **whole wired body** as a portable, dormant package.

The empirical origin is the grace Claude-Code SKILL-zip: the operator wanted
its cosmon analogue — not to teach one agent a skill, but to **package a
validated agent-polymer** that a recipient could download, parameterize, and
grow into a running mission. wheeler's framing names *why* a word is owed even
though no runtime is: a mission stays a nameless convention because it never
leaves its fleet; this object exists *in order to leave*, and **the moment an
object becomes a unit of exchange between two parties, it acquires an
identity — and an identity demands a name.**

The convergence on the term `spore` was unusually clean because the panel split
the labour exactly as the frame intended: torvalds owned *"is it even a new
thing?"* (the data-structure verdict, Q1), and jobs/knuth/shannon owned *"what
is it called and defined as"* (Q2/Q3/Q5). There was no muddle between the two
questions.

---

## Decision

Name the empty cell **`spore`**.

### The definition (knuth's sentence, adopted verbatim)

> **A *spore* is a shareable, parameterizable template of an entire polymer —
> bundling one or more molecule formulas, a fleet configuration, a mission
> template, a manifest, and an embedded TLA+ specification — that, when
> instantiated with a recipient's parameters, germinates into a polymer of
> molecules which provably terminates, whose gates fail closed, and whose
> parametrization is deterministic.**

The definition names three invariants *as invariants*, not as part of the noun:

1. **Termination** — the germinated polymer has a bounded variant; no
   unbounded foaming.
2. **Fail-closed gates** — gates deny on ambiguity rather than pass.
3. **Deterministic parametrization** — instantiation is a *function*: same
   spore + same params ⇒ same polymer.

The verb for the bottom row is **`germinate`** (`spore ──germinate──▶
polymer`), parallel to `nucleate` (`formula ──nucleate──▶ molecule`).
`germinate` is the spore's own native verb (a dormant packet grows into a
living organism); the runner-up `amorcer` (an *initiator* metaphor) clashes
register with `spore` and is recorded but not adopted.

### D1 — A projection at the runtime layer, a new noun at the ontology layer

This is **not** a new domain type, lifecycle, or runtime (torvalds: PROJECTION;
ADR-111's verdict one scale up). The germinated thing is an ordinary polymer of
ordinary molecules going through `nucleate → tackle → wait → done`.

But it earns a **name**, because it crosses a fleet boundary and is exchanged
between parties — *transfer confers identity* (wheeler). The honest accounting
of what is genuinely new: of the six parts of the bundle (crew, per-node
recipes, param schema, manifest, proof, **wiring**), five already have homes —

| bundle part        | existing home                                  |
|--------------------|------------------------------------------------|
| crew               | `Fleet` (ADR-038/039)                          |
| per-node recipes   | `[Formula]` (ADR-004)                          |
| param schema       | formula variables                              |
| manifest / header  | header bytes                                   |
| proof              | an optional `.tla` file (a check, not a type)  |
| **wiring**         | **— nothing —**                                |

Only the **wiring** — the static inter-formula edge set (`from → to`, typed
`Blocks` / `DecayProduct`) — has no home. `Formula` structurally *cannot* hold
an edge to another `Formula` (it is the template of ONE molecule); `Fleet`
composes crew, not work-topology. A graph, however, is **data, not a
primitive** — this is the `Fleet`/`compose` move one scale up. The structural
reduction:

```
Spore = Fleet (crew)
      + [Formula] (per-node recipes)
      + ParamSchema
      + DAG-of-typed-edges        ← the only new bytes
      + optional .tla seal
```

materialised by a pure function

```
expand(spore, params) -> [ cs nucleate … --blocked-by … ]
```

— the moral twin of `cs fleet resolve`. **"It's a Makefile that replays to
`cs nucleate`"**: a declarative front-end over existing verbs, not a new kind
of molecule and not a scheduler.

### D2 — Proof and portability are properties, stated in the definition, not baked into the noun

The unanimous Q5 verdict (wheeler · knuth · shannon, three independent
derivations of the same conclusion) demotes the two orthogonal loads from the
noun. They reuse vocabulary cosmon and biology already own — **zero new naming
budget**:

- **Proof → a `seal`.** cosmon already seals artifacts with BLAKE3
  (`prompt_seal`, `briefing_seals`, `cs verify`). A TLA+-validated spore is a
  **sealed** spore; the embedded TLA+ module is the strongest seal a spore can
  carry. States become speakable: *non-sealed*, *sealed-but-unshared*, etc.
- **Portability → export / import (biology's *conjugation*).** Sharing a spore
  is an export/import event; **refining** a spore is editing its manifest
  (*recombination*). A spore can therefore be *unsealed*, *sealed-but-private*,
  or *sealed-and-shared* — three orthogonal axes a single noun must not try to
  carry.

The three loads are not equiprobable (shannon's source-coding argument):
structure is the high-entropy identity bit; proof and portability are
near-redundant *given cosmon's standing conventions* (artifacts are already
git-shareable; validation is already a cosmon value). Efficient coding spends
the scarce symbol on structure and lets the predictable bits ride in the
**definition**, where redundancy belongs as error-correction.

### D3 — The seal must gate `expand()` (the proof is real engineering only if it fails closed)

torvalds' load-bearing constraint: a sealed `.tla` that nobody honours is a
**dishonest checkbox**. The seal is real engineering **iff** the embedded proof
gates instantiation — `expand()` MUST refuse to replay an unproven or
proof-failing topology. A sealed spore that germinates anyway has lied about
its seal.

The surprising corollary (torvalds): **you cannot TLA+-prove a runtime-emergent
polymer** — a proof needs a fixed model to quantify over, and an emergent
polymer (the live mission) has nothing to hand TLA+. So the operator's *"must
be TLA+-validated"* requirement is not just a feature; it is **independent
evidence that the static-wiring manifest is a real object**. The demand for a
proof *forces* the static topology into existence. The TLA+ requirement and the
new file format are not two features — they are the same structural fact seen
twice.

### D4 — Guardrails: what `spore` is NOT

- **NOT `capsule`** — operator-rejected; it collides with `molecule`.
- **NOT `formula`** — a formula is a *stamp* (one identical molecule); a spore
  is *generative* (it builds a body never drawn in advance). "Stamp vs seed" is
  why it deserves a new word, not "big formula."
- **NOT `mission`** — that is the live *instance* (ADR-111). A spore is the
  *recipe* of a mission.
- **NOT a homogeneous clone of `decay`** — a spore expands into N **distinct**
  wired nodes, not N copies of one.
- **NOT a compound name** — no `proof-spore`, `sealed-template`,
  `verified-polymer`. The proof is a property (D2), not part of the noun
  (unanimous Q5).
- **NOT a new molecule lifecycle / typestate / runtime** — the germinated thing
  is an ordinary polymer (`nucleate → tackle → wait → done`).

### D5 — Runner-up of record

The single genuine term contest was `spore` vs **`plasmid`** (wheeler's
dissent). wheeler's case is the most rigorous Q2 answer in the panel: the
essence is *transfer*, and molecular biology owns the only register where
shareable + refinable + validated + horizontal-transfer are all native to **one**
word; his grace-SKILL-zip ↔ plasmid mapping is exact. shannon supplied the
decisive counter on wheeler's own terms (information in the channel):
**plasmid's decode cost is too high for an artifact whose whole purpose is to
be handed to a stranger** — *"plasmid requires undergraduate molecular biology
to decode; spore decodes at primary-school level."* jobs' adopter-chair test
("a person who has never seen cosmon types `cs <term> run` — do they instantly
get it?") independently confirmed: `spore` yes, `plasmid` no.

`spore` wins on **adopter-legibility**, the dimension the primitive exists to
serve. wheeler's transfer-insight is not discarded — it is *absorbed* as the
D1 rationale for why the thing deserves a name at all, and his demoted-property
vocabulary (`seal`, export/import) is exactly D2. The runner-up is recorded
honestly here, per the panel's instruction.

---

## Consequences

- **The last empty cell of the fractal template/instance table is filled.** A
  validated agent-polymer becomes a shareable, refinable, named object — the
  cosmon analogue of a SKILL zip.
- **Fractal table extends cleanly:**

  | atom | → | step       |
  |------|---|------------|
  | formula | → | molecule |
  | **spore** | → | **polymer** |
  | **sealed spore** | → | **validated mission** |

  An atom composes into a step; a formula nucleates into a molecule; a spore
  germinates into a polymer; and a *sealed* spore germinates into a *validated*
  mission. Each row is the same generative relation seen at the next scale.
- **Existing machinery is reused, nothing is duplicated:** fleet composition
  (ADR-038/039), formula variables, BLAKE3 seals + `cs verify`, content-
  addressing. The only genuinely new bytes are the wiring manifest.
- **What is added is bounded:** one file format (`.spore` / `spore.toml`), one
  pure `expand()` function, and a `cs spore` verb family. **No runtime, no new
  type, no new lifecycle.**
- **A new vocabulary lands as defined terms:** *spore*, *germinate*, *sealed
  spore*, *conjugation* (export/import), *recombination* (manifest refinement).
  Future chronicles and deliberations can use them without re-defining.
- **Rollback path:** this ADR is doc-only. Rollback = `git revert` of the
  introducing commit. The cited primitives (`Formula`, `Fleet`, BLAKE3 seals,
  `cs fleet resolve`) are untouched and remain available regardless of whether
  the *spore* word is in use.

---

## Implémentation (future) — ontology fixed, code deferred

This ADR fixes the **ontology and the format**. The build is a separate,
operator-initiated follow-up; the deliberation explicitly nucleated **no**
implementation child (premature decomposition before operator acceptance is the
grace duplicate-children pathology). The natural future `task`, briefed from
torvalds' structural cut:

- Define the `.spore` wiring-manifest schema:
  `[[spore.node]] formula / alias / vars` + `[[spore.edge]] from / to / type`
  — the only bytes `Formula` / `Fleet` cannot already hold.
- Write the pure `expand(spore, params) -> [cs nucleate --blocked-by …]` as the
  twin of `cs fleet resolve`.
- Content-address the bundle for sharing (candidate: reuse ADR-039's
  `FleetId`-style content-hashing — *content-addressing is the registry*,
  ADR-039 §1).
- Wire the optional `.tla` seal as a **fail-closed gate on `expand()`** (D3).

Launch / share / refine *intuition* (the soft strate, Q4 of the deliberation —
sketched, not settled):

- **Launch:** `cs spore run <spore-id> --var topic="…"` (or `cs spore
  germinate`), replaying `expand()` to existing `cs nucleate` calls.
- **Share:** a content-addressed `.spore` bundle, handed like the grace SKILL
  zip.
- **Refine:** edit the manifest, re-seal.

**Open design question (deferred to a future implementation ADR):** the
*sharing mechanism* — registry vs pure content-addressing, and the `.spore`
bundle layout. The deliberation marked Q4 as treated-but-partial: the gesture
is agreed, the mechanism is the next ADR's job, not settled here.

### Explicit non-goals (v0)

- No `SporeMolecule` super-type, no new typestate.
- No new scheduler — `expand()` replays to existing `cs nucleate`.
- The sharing / registry mechanism is a **separate** design question.

---

## References

### Internal (paths relative to `/srv/cosmon/cosmon/`)

- `.cosmon/state/fleets/default/molecules/delib-20260626-951f/synthesis.md` —
  five-persona panel synthesis (convergences, divergences, coverage table).
- `.cosmon/state/fleets/default/molecules/delib-20260626-951f/outcomes.md` —
  the Path-3 recommendation + the proposed ADR skeleton this ADR canonises.
- `docs/adr/004-bead-formula-molecule-distinction.md` — `formula` = template of
  ONE molecule.
- `docs/adr/026-dynamic-fleet-orchestration.md` — dynamic fleet / DAG runtime.
- `docs/adr/038-fleet-composability-v0.md`,
  `docs/adr/039-fleet-composability.md` — fleet composition, content-addressed
  `FleetId` (the sharing substrate).
- `docs/adr/111-mission-convention-existing-primitives.md` — `mission` = the
  live instance (convention, not runtime); this ADR is its verdict one scale up.

### Deliberation lineage

- Panel: wheeler (dissent: `plasmid`; transfer-confers-identity framing),
  torvalds (Q1 PROJECTION verdict; wiring-is-the-only-new-bytes structural cut;
  proof-gates-`expand()` constraint), jobs (`spore`, product-feel + adopter
  test), knuth (`spore`, the definition sentence + name≠proof hygiene), shannon
  (`spore`, lowest-collision / highest-density; the decisive counter to
  plasmid).
