# ADR-046: P_legibility — Artifact Legibility Axiom

**Status:** Proposed (pending evidence from O-ring test)
**Date:** 2026-04-16
**Parent deliberation:** `delib-20260416-d04c`
**Depends on evidence:** `task-20260416-021a` (O-ring test)

## Context

Deliberation `delib-20260416-d04c` examined convention propagation across
galaxy boundaries — the mechanism by which agents in external repositories
produce cosmon-compatible artifacts after reading a CLAUDE.md file, without
knowledge of cosmon itself. The panel converged on a missing axiom in the
Witness Charter v0 ([`CONSTITUTION-v0.md`](../founding/CONSTITUTION-v0.md)).

The existing four axioms constrain internal behavior:

| Axiom | Constrains |
|-------|-----------|
| **P_external** (ADR-032) | Authority must come from outside the universe |
| **P_trace** | Every mutation produces a durable artifact |
| **P_channels** | Communication partitioned into prompt / tool / output |
| **P_seal** | Ratification is symmetric and append-only |

None of these says anything about whether the artifacts produced are
**interpretable by an outsider**. A cosmon-governed universe could satisfy
all four axioms while producing artifacts opaque to any reader without
cosmon knowledge — commit messages full of molecule IDs, logs referencing
internal DAG positions, synthesis files assuming lifecycle vocabulary. The
artifacts would be durable (P_trace), externally witnessed (P_external),
channel-correct (P_channels), and properly sealed (P_seal) — yet useless
to anyone outside the system.

This gap matters because artifact legibility is the conserved quantity that
enables convention propagation. When an artifact crosses a galaxy boundary
(via git, PR, or surface file), its meaning must survive the crossing
without the receiver possessing cosmon's internal model.

## Decision

Adopt **P_legibility** as a proposed constitutional axiom:

> **P_legibility (Artifact Legibility Axiom).** Every artifact produced
> under cosmon governance must be fully interpretable by an agent with no
> knowledge of cosmon.

"Fully interpretable" means: the artifact's purpose, content, and
relationship to other artifacts can be understood from the artifact itself
and the standard context of the repository (README, file structure, git
history) — without reference to molecule IDs, lifecycle phases, DAG
structure, energy budgets, temperature tags, or any cosmon-specific
vocabulary.

## Independence Proof

P_legibility is independent of the existing four axioms. Independence
requires showing that (1) P_legibility is not entailed by any combination
of the four, and (2) P_legibility does not entail any of them.

### P_legibility is not entailed by {P_external, P_trace, P_channels, P_seal}

**Counter-model.** Consider a cosmon universe that:

- Has an external witness who seals its charter (P_external ✓)
- Records every state mutation as a durable artifact (P_trace ✓)
- Partitions communication into prompt / tool / output (P_channels ✓)
- Uses symmetric, append-only seals (P_seal ✓)

But produces commit messages like:

```
evolve(mol-a3f2): step 2/4 — entangle(mol-b7c1, DecayProduct)
```

This commit is durable, witnessed, channel-correct, and properly sealed —
yet unintelligible to an agent unfamiliar with cosmon's `evolve`, `mol-*`,
`entangle`, and `DecayProduct` vocabulary. All four axioms hold;
P_legibility does not. Therefore P_legibility is not derivable from the
existing axioms.

### P_legibility does not entail any of {P_external, P_trace, P_channels, P_seal}

**Counter-models for each:**

- **¬P_external + P_legibility**: A self-certifying universe whose artifacts
  are perfectly legible markdown — readable by anyone, but with no external
  witness. Legibility is a property of artifacts, not of authority.

- **¬P_trace + P_legibility**: A universe that produces legible artifacts
  but stores them only in ephemeral memory — fully interpretable while they
  exist, but not durable. Legibility is about interpretability, not
  persistence.

- **¬P_channels + P_legibility**: A universe where agents mix prompt and
  tool content in a single stream, but every artifact produced is
  self-describing. Channel discipline is orthogonal to artifact clarity.

- **¬P_seal + P_legibility**: A universe with mutable, asymmetric
  ratification (seals can be silently overwritten), but whose artifacts
  remain perfectly legible. Seal integrity is about governance protocol,
  not about whether outsiders can read the output.

### Relationship to P_trace

P_trace and P_legibility are complementary but distinct:

| | Durable | Legible |
|---|---|---|
| P_trace ✓, P_legibility ✗ | ✓ | ✗ — opaque but persistent |
| P_trace ✗, P_legibility ✓ | ✗ — ephemeral but self-describing | ✓ |
| Both ✓ | ✓ | ✓ — the target state |

P_trace guarantees artifacts **exist**. P_legibility guarantees they
**mean something** to an outside reader. Neither implies the other.

## Relationship to Constitutional Projection

Deliberation `delib-20260416-d04c` identified the mechanism of
**constitutional projection**: conventions encoded on a shared substrate
(CLAUDE.md) are independently reconstructed by any agent reading that
substrate — without runtime coupling, without installation, without
knowledge of cosmon.

P_legibility is the **conservation law** underlying this mechanism.
Constitutional projection works *because* the artifacts satisfy
P_legibility: they carry their meaning with them. An artifact that
violates P_legibility cannot propagate its conventions across a galaxy
boundary — the receiver lacks the decoder ring.

The relationship is analogous to `cs reconcile` and surface files:
`cs reconcile` projects internal state onto standard surfaces (STATUS.md,
ISSUES.md) that non-participants can read. Constitutional projection does
the same for conventions — it projects behavioral constraints onto
CLAUDE.md. Both projections succeed only when their output satisfies
P_legibility. The axiom is what makes the projection **lossless at the
boundary**.

This means P_legibility is not about the projection mechanism itself (which
is a constructive phenomenon, not an axiom-level constraint) but about the
property that artifacts must have for *any* boundary-crossing mechanism to
work. The mechanism may change; the conservation law holds.

## What P_legibility Forbids

P_legibility is a constraint on output, not on internal operations. It
does not forbid cosmon from using its own vocabulary internally. It forbids
that vocabulary from leaking into artifacts that cross the boundary.

### Concrete examples

| Artifact | P_legibility? | Reasoning |
|----------|--------------|-----------|
| `fix: correct off-by-one in range check` | ✓ | Self-describing; no cosmon knowledge needed |
| `evolve(mol-a3f2): step 3/4 — dispatch panel` | ✗ | `evolve()`, `mol-*` prefix, step numbering require cosmon knowledge |
| `feat(auth): add token refresh endpoint` | ✓ | Conventional commit; any developer can read it |
| `chore(state): track artifacts for delib-20260416-d04c` | Borderline | `delib-*` ID is cosmon-specific, but commit body could make the change self-evident |
| `synthesis.md` with section headers and prose | ✓ | Readable as a standalone document |
| `events.jsonl` with molecule state transitions | ✗ | Requires cosmon lifecycle knowledge to interpret |
| `STATUS.md` rendered by `cs reconcile` | ✓ if well-projected | The surface should be legible; the projection command is internal |

### The borderline case

Commit messages of the form `evolve(<id>): step N/M — <description>` are
the most common P_legibility tension. The `evolve()` prefix and molecule
ID require cosmon knowledge, but the trailing description is
self-describing. This ADR does not mandate a specific resolution — the
tension exists because these commits serve dual roles (internal lifecycle
tracking and external artifact record). A future ADR may address commit
message format for boundary-crossing contexts.

## Pending Evidence

**This ADR has status `proposed`, not `accepted`.** Ratification requires
empirical evidence that the conservation law is load-bearing — that
artifact legibility measurably affects convention propagation.

### Required evidence: O-ring test (task-20260416-021a)

The O-ring test, proposed by the Feynman persona in `delib-20260416-d04c`,
measures convention compliance with and without CLAUDE.md:

1. **Control**: Remove CLAUDE.md from an external repository (e.g.,
   noesis) for a defined period. Measure agent artifact compliance with
   cosmon conventions (conventional commits, truth pointers, atomicity).
2. **Treatment**: Restore CLAUDE.md. Measure the same metrics.
3. **Signal**: If compliance drops measurably in the control condition,
   CLAUDE.md is the active channel — and P_legibility is the property that
   makes that channel effective.

### What constitutes sufficient evidence

- **Strong**: Statistically significant compliance drop (>20% on at least
  two of three metrics: commit format, truth pointer adherence, commit
  atomicity) during the control period, with recovery during treatment.
- **Weak**: Compliance drop <10% or confounded by other variables (agent
  model updates, human intervention). In this case, P_legibility may still
  be well-formed as an axiom but lacks the empirical grounding to justify
  constitutional status.
- **Negative**: No measurable compliance drop. P_legibility remains a
  useful design principle but should not be elevated to axiom status — the
  mechanism it conserves is not empirically validated.

### Ratification gate

P_legibility advances from `proposed` to `accepted` only when:

1. O-ring test results are committed as an evidence artifact
2. Results meet the "strong" threshold above
3. A follow-up ADR references the evidence and requests ratification
4. Standard Witness Charter ratification protocol applies (2-of-3 quorum
   if elevated to constitutional status)

## Implications for Existing Artifacts

If ratified, P_legibility would require auditing existing artifact
patterns:

1. **Commit messages**: The `evolve(<id>)` prefix pattern needs a
   legibility-preserving alternative for commits that cross galaxy
   boundaries (e.g., when a branch is merged to main and visible to
   external agents).

2. **Surface files**: Already designed for legibility (`cs reconcile`
   renders STATUS.md as plain markdown). No change needed.

3. **Molecule artifacts** (prompt.md, briefing.md, synthesis.md): These
   are generally legible as standalone documents. The frontmatter uses
   cosmon-specific fields (molecule_id, formula) but the body is
   self-describing. The frontmatter is metadata, not content — acceptable
   under P_legibility as long as the prose stands alone.

4. **events.jsonl**: Internal operational log. Not a boundary-crossing
   artifact. P_legibility does not apply to purely internal state.

## Consequences

- **Positive**: Establishes a clear, testable constraint on artifact
  quality. Makes convention propagation a first-class architectural
  concern. Provides the theoretical grounding for constitutional
  projection.

- **Negative**: Creates tension with internal lifecycle tracking (commit
  prefixes, molecule IDs in artifact names). Some internal convenience
  may need to yield to boundary legibility.

- **Neutral**: Does not change the existing four axioms. Does not require
  changes to the Lean kernel. Does not affect the witness protocol.

## References

- **Parent deliberation**: `delib-20260416-d04c` (synthesis.md §C7, §D4)
- **Einstein panel response**: `delib-20260416-d04c/responses/einstein.md`
  — original proposal of P_legibility as conservation law
- **Witness Charter v0**: [`CONSTITUTION-v0.md`](../founding/CONSTITUTION-v0.md) §1
- **P_external axiom**: [ADR-032](032-p-external-witness-axiom.md)
- **Surface Sync Protocol**: [`surface-sync-protocol.md`](../surface-sync-protocol.md)
  — existing legibility-preserving projection mechanism
- **O-ring test**: `task-20260416-021a` (pending)
