# ADR-033: Drop "Molecular Blockchain" — Adopt "Noogram"

**Status:** Accepted
**Date:** 2026-04-13
**Parent deliberation:** `delib-20260413-a010`

## Context

The operator's founding cosmic vision (2026-04-13) described cosmon as a
"molecular blockchain of cognition" — a polymorphic, append-only chain
where every cognitive act produces an indestructible molecule. The framing
captured something real: cosmon's event log is append-only, content-addressed,
and hash-linked. But the word "blockchain" carries connotations — consensus
protocols, permissionless networks, token economies — that do not apply.

Delib-20260413-a010 (a 16-persona deliberation on the cosmic vision) surfaced
this as the **single strongest convergence** across the entire panel. Seven
independent perspectives converged on the same conclusion:

- **Satoshi (the blockchain expert)** — Bitcoin solves the double-spend
  problem in adversarial, permissionless networks. Cosmon has a single
  operator, a trusted environment, and no Byzantine fault tolerance
  requirement. The reference architecture is wrong. The useful subset
  (append-only hash-linked log, Merkle proofs) maps to **Certificate
  Transparency**, not Bitcoin.
  (`responses/satoshi.md`)
- **Feynman** — "You already have a Merkle DAG — it's called git." The
  blockchain framing is decoration over an existing structure.
  (`responses/feynman.md`)
- **Jobs** — "Blockchain" is a tab-closer for serious engineers and a
  magnet for speculators. The word costs more audience than it earns.
  (`responses/jobs.md`)
- **Torvalds** — The term attracts crypto-adjacent regulatory and
  ideological energy the project does not need. Call it what it is: a
  signed event log.
  (`responses/torvalds.md`)
- **Popper** — The blockchain claim as stated is unfalsifiable (N=1,
  single operator, no adversarial test). It cannot be a scientific
  hypothesis until it specifies conditions under which it would fail.
  (`responses/popper.md`)
- **Einstein** — The mathematical structure is real (append-only,
  hash-linked, verifiable) but maps to Certificate Transparency logs,
  not to distributed consensus.
  (`responses/einstein.md`)
- **Wheeler** — Proposed the replacement term "noogram" (νοῦς + γράμμα,
  "written cognition") — capturing the real structure (every cognitive
  act is written, hash-linked, append-only) without the blockchain
  connotations.
  (`responses/wheeler.md`)

## Decision

**Drop the word "blockchain"** from all cosmon documentation, thesis
documents, and communication.

Adopt **two distinct names for two distinct audiences**:

### 1. Long-form / thesis / internal: **noogram**

- **Etymology:** νοῦς (mind, cognition) + γράμμα (written record).
  "Written cognition" — every cognitive act leaves a permanent, verifiable
  trace.
- **Verb:** *noograph* (to record a cognitive act on the noogram).
- **Adjective:** *noographic* (pertaining to the noogram structure).
- **Use in:** THESIS.md, CONSTITUTION.md, ADRs, internal design documents,
  chronicles, academic-facing communication.

### 2. Product / engineering: **signed event log + verification receipts**

- Alternative phrasing: **signed append-only molecular ledger**.
- **Use in:** README.md, landing page, engineering docs, API documentation,
  onboarding guides.

This two-layer naming reflects a real distinction: "noogram" names the
*concept* (the structure of written cognition); "signed event log" names
the *mechanism* (what the code actually does). Engineers reading the README
get immediate comprehension; thesis readers get the deeper framing.

## Consequences

### Vocabulary

- The word "blockchain" is **explicitly retired** from the cosmon lexicon.
  Future documents MUST NOT reintroduce it, even as analogy, without
  referencing this ADR and arguing why the analogy is warranted.
- "Noogram" enters the ubiquitous language (THESIS.md Part V vocabulary).
- Product-facing docs use "signed event log" or "signed append-only
  molecular ledger."

### Sweep required

- A sibling molecule (pattern: `task-20260413-33d1`) performs the full
  vocabulary sweep across THESIS.md, README, ADRs, CLAUDE.md, and source
  code comments. This ADR establishes the decision; the sweep applies it.

### Crypto-economic vocabulary

- Any future crypto-economic concepts (token incentives, multi-party
  verification markets) are deferred to a separate ADR. This decision
  does not foreclose crypto-economic design — it removes a misleading
  label from a mechanism that does not currently involve distributed
  consensus.

### Communication

- When explaining cosmon's event integrity to external audiences, lead
  with Certificate Transparency as the reference architecture, not
  Bitcoin or Ethereum. CT is the precise analogue: append-only,
  hash-linked, third-party verifiable, trusted operator.

## Rejected alternatives

- **"Molecular blockchain"** — the original framing. Rejected: implies
  distributed consensus, permissionless access, and Byzantine fault
  tolerance — none of which apply. Costs more credibility than it earns.
- **"Cognitive blockchain"** — same problems as above, with added
  vagueness.
- **"Cognitive ledger"** — too generic. Does not capture the
  content-addressed, hash-linked, append-only structure.
- **"Merkle chain"** — technically more accurate but still invokes
  blockchain associations and is not widely understood outside
  cryptography.

## Open questions

- **Should `cosmon-hash` and `cosmon-sign` crates adopt user-facing names
  hinting at "noogram"?** E.g. `cosmon-noograph` for the signing/hashing
  pipeline. Deferred to a naming ADR once the crate boundaries stabilize.
- **Does "noogram" belong in the CLI surface?** E.g. `cs noograph` as an
  alias for event attestation commands. Deferred until the verification
  pipeline exists.

## References

- Parent deliberation: `delib-20260413-a010`
- `responses/satoshi.md`, `responses/feynman.md`, `responses/torvalds.md`,
  `responses/wheeler.md`, `responses/jobs.md`, `responses/popper.md`,
  `responses/einstein.md` (same parent)
- Sibling ADR: ADR-032 (P_external — External Witness Axiom)
- Founding vision: an internal chronicle
- Sibling sweep task: vocabulary sweep across all documents (separate molecule)
