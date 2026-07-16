# ADR-032: P_external — External Witness Axiom

**Status:** Accepted (constitutional axiom)
**Date:** 2026-04-13
**Parent deliberation:** `delib-20260413-a010`

## Context

Delib-20260413-a010 asked whether cosmon can be self-contained — whether a
cosmon universe can prove its own consistency, ground its own DAG, and supply
its own free energy. The panel returned a unanimous negative finding from six
independent disciplinary perspectives:

- **Gödel (logic)** — `Con(Constitution)` is unprovable from within any
  sufficiently expressive system. An external witness is required to license
  the consistency claim.
  (`responses/godel.md`)
- **von Neumann (game theory / DAG)** — every DAG must terminate at a
  *grounding node* — an external reality the system cannot fake. Universes
  without grounding nodes are abstractions in free fall.
  (`responses/von-neumann.md`)
- **Knuth (formal verification)** — self-verification is precluded by Gödel.
  The CompCert model (external prover) is the only credible path.
  (`responses/knuth.md`)
- **Hawking (boundaries)** — the meta-level must be fundamentally outside the
  system. A "verifier of universes" (e.g. Neurion, Foundry) must not itself be
  a cosmon universe.
  (`responses/hawking.md`)
- **Turing (computability)** — a meta-verifier of the multiverse cannot itself
  be a cosmon universe without creating a self-referential decision problem.
  (`responses/turing.md`)
- **Carnot (thermodynamics)** — the operator is the external thermodynamic
  environment that supplies the free energy driving the system's gradient.
  Without an external reservoir, the system relaxes to equilibrium (heat death).
  (`responses/carnot.md`)

The operator's existing chronicle "the human pilot as a formula" already
implicitly invokes this principle. This ADR promotes it from implicit practice
to an explicit constitutional axiom, sitting alongside P0–P6.

The governing synthesis is `synthesis.md` §"Tension C: Closed System vs
External Anchor" in the parent deliberation.

## Decision

Adopt **P_external** as the 8th constitutional axiom of cosmon:

> **P_external (External Witness Axiom).** Every cosmon universe has at
> least one designated **External Witness** — typically the human operator,
> but extensible to formal verifiers, market outcomes, test suites, or other
> universes mutually witnessing each other in a non-self-referential graph.
> The External Witness supplies:
>
> 1. **The consistency claim** that the universe cannot prove about itself
>    (Gödel, Knuth).
> 2. **The grounding node** that prevents the DAG from floating free of
>    reality (von Neumann, Hawking, Turing).
> 3. **The free energy** that drives the system's thermodynamic gradient
>    (Carnot).

## Consequences

### Constitutional

- `CONSTITUTION.md` (sibling `task-20260413-8a2f`) lists P_external as the
  eighth axiom (P0–P6 + P_external).
- Self-verifying claims are **constitutionally forbidden**. Any component
  (notably Foundry) that purports to prove cosmon's own consistency must
  explicitly mark the claim as dependent on an external witness, or the
  claim is rejected as ill-formed.

### Operational

- Every universe's `.cosmon/config.toml` gains an `[external_witness]`
  section declaring its witness(es). Schema details are an open question
  (see below), but the section is mandatory once P_external lands in code.
- Every DAG SHOULD terminate at at least one **grounding node** — a
  molecule whose value function is outside cosmon's control (test pass,
  market outcome, human review, external benchmark, signed attestation).
  A DAG with no grounding node is detectable and warn-worthy.
- **Federations form a non-self-referential graph** of mutual witnesses.
  A cycle `A → B → A` violates P_external: each universe would be proving
  its consistency via the other, recreating the forbidden self-reference
  one level up. Federation topologies MUST be a DAG (or at minimum, every
  cycle must include at least one grounding node outside cosmon).

### Architectural

- Neurion, Foundry, and any future "verifier of universes" are
  **not themselves cosmon universes**. They are external witness
  infrastructure. If they ever need cosmon's lifecycle, it is as clients
  of a separate cosmon instance they do not witness.
- Future ADRs that introduce shared state between universes (signal bus,
  surface projection, cross-fleet beads) must explicitly argue how the
  non-self-referential graph is preserved.

## Rejected alternatives

- **"Self-verifying cosmon"** — precluded by Gödel. Any attempt to prove
  `Con(Constitution)` inside cosmon is either incomplete or inconsistent.
- **"Neurion as universal trust root"** — if Neurion is itself a cosmon
  universe, it creates a witness cycle at the root. Neurion stays outside
  the cosmon universe class.
- **"No external dependency" (Hawking's no-boundary cosmon)** — useful as
  a theoretical gauge (what would a closed cosmon look like?) but not
  achievable as a design. Kept as a foil, not a target.

## Open questions

- **Schema for `[external_witness]`** — what fields? `kind` (operator /
  verifier / market / test-suite / peer-universe), `identity` (handle,
  URI, hash), `scope` (what claims it witnesses)? Deferred to a separate
  ADR once a second witness kind lands.
- **Is "the operator's review action" a first-class grounding node?**
  Today it is implicit in `cs done`. A future ADR may promote operator
  review to a typed molecule kind so grounding is legible in the DAG,
  not just in the pilot's head.
- **Detection and enforcement** — should `cs reconcile` warn when a DAG
  has no grounding node? Should `cs init` refuse to scaffold a project
  without an `[external_witness]` declaration? Deferred to implementation
  ADRs.

## References

- `synthesis.md` §"Tension C: Closed System vs External Anchor"
  (parent: `delib-20260413-a010`)
- `responses/godel.md`, `responses/von-neumann.md`, `responses/knuth.md`,
  `responses/hawking.md`, `responses/turing.md`, `responses/carnot.md`
  (same parent)
- Chronicle: "the human pilot as a formula"
- Sibling task: `task-20260413-8a2f` (update `CONSTITUTION.md` with
  P_external)
