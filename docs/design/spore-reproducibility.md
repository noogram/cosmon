---
title: Spore reproducibility: the trust spectrum
status: design-note
relates_to: ADR-139, cs-verify
---

# Spore reproducibility: the trust spectrum

This note connects the [`spore`](../vocabulary.md#spore) primitive
([ADR-139](../adr/139-spore-shareable-polymer-template.md)) to cosmon's
proof surface, [`cs verify`](../cs-verify.md). The question it answers:
*when you hand a stranger a sealed spore, what exactly have you proven, and
how much do they have to re-do to trust it?*

## Two kinds of work, two kinds of proof

Not all tracked work is equally reproducible, and the seal must be honest
about which kind it is covering. The discriminating bit is whether
**re-checking the work requires re-running it**. Call it
`verify_requires_execution` (a conceptual bit here, not yet a code field):

- **Deterministic-formula molecule** (`verify_requires_execution = false`).
  A step whose `command =` gate is a pure function of its inputs (a build, a
  schema regeneration, a hash, a deterministic transform) is **cachable by
  content**: same inputs ⇒ same output bytes. `cs verify` recomputes the
  BLAKE3 of the artifact and compares to the seal. No re-execution of cognition
  is needed; the bytes either match or they do not.
- **Agentic molecule** (`verify_requires_execution = true`). A step whose work
  is an LLM session is **not** byte-reproducible: Claude's output is not
  byte-equal across runs (this is exactly the gap `cs verify` flags under
  *"What is NOT checked (yet): LLM determinism"*). Here the seal certifies the
  **process** (the artifact present at completion time, the gate that passed,
  the event chain), not a content hash that would replay identically. To trust
  it you re-run the gate and re-walk the chain; semantic equivalence is a
  separate, future `verify-semantic` formula.

The spectrum runs from *cachable-by-content* (cheapest trust, deterministic) to
*sealed-and-re-executed* (process trust, agentic). A single molecule sits at
one end. A spore spans the whole range at once, because a polymer mixes both
kinds of node.

## What a spore's seal proves that a molecule's cannot

`cs verify` audits a molecule's proof-of-work chain **after the fact**: the
work already ran, and we are checking the trace it left. A spore's seal is the
opposite arrow in time: it certifies a polymer that **has not run yet**, over
the whole space of bodies it could grow into.

This is the load-bearing point from ADR-139 (torvalds' corollary, D3): **you
cannot TLA+-prove a runtime-emergent polymer.** A live mission, foaming new
molecules as it goes, has nothing fixed to hand a model checker. The proof
only becomes possible once the wiring is frozen into a static manifest. So the
spore's seal does not prove "this run was fine" (that is `cs verify`'s job,
node by node, afterwards); it proves a structural property of the
**orchestration** ahead of time:

- **termination**: every body the spore can germinate has a bounded variant,
- **fail-closed gates**: every gate denies on ambiguity rather than leaking a
  pass,
- **deterministic parametrization**: instantiation is a function, same spore
  plus same parameters yields the same wiring.

In words: a molecule's seal certifies *one trace that happened*; a spore's
seal certifies *the safety of the topology over the space of emergent bodies it
can produce*. The first is forensics; the second is a guarantee about a
generator.

## Why the seal must gate `expand()` (fail closed, not advisory)

The molecule-level seal is, by cosmon doctrine, a **trace, not a lock**: it
catches the lazy shadow contract, never a motivated adversary (see
`architectural-invariants.md` §8b and the briefing-seals section of
`CLAUDE.md`). Seal emission must never block the hot path.

The spore-level seal is the deliberate exception. Per ADR-139 D3, a sealed
`.tla` that nobody honours is a dishonest checkbox: the seal is real
engineering **iff** the proof gates instantiation. So the future `expand(spore,
params)` function MUST refuse to replay an unproven or proof-failing topology.
A sealed spore that germinates anyway has lied about its seal. The asymmetry is
intentional: an advisory seal is fine when the work is already done and on
disk; a seal on a *generator handed to a stranger* has to fail closed, because
the recipient is trusting the promise before any node has run.

## Status

The reproducibility model above is **ontology, not yet code**. `cs verify`
ships today for molecules; the `verify_requires_execution` distinction and the
spore-level `expand()` gate are future work tracked in ADR-139
§Implémentation. This note exists so the design intent is on record before the
runner is built.

## See also

- [ADR-139 `spore`](../adr/139-spore-shareable-polymer-template.md): the
  primitive, the definition, and D3 (proof gates `expand()`).
- [`cs verify`](../cs-verify.md): the molecule-level proof-of-work surface
  this note generalizes one scale up.
- [vocabulary.md §Spore](../vocabulary.md#spore): the ontology entry.
