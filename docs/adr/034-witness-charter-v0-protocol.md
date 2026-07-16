# ADR-034 — Witness Charter v0 protocol

**Status:** Proposed · **Date:** 2026-04-14
**Drafter:** `delib-20260414-f251` (deep-think, 10-persona panel)
**Supersedes:** none · **Related:** ADR-016, ADR-030, ADR-032, ADR-033

## Context

[ADR-032](032-p-external-witness-axiom.md) established P_external: a
cosmon-universe cannot ratify its own coherence; authority requires outside
witnesses. ADR-033 (Drop blockchain, adopt noogram) settled that the
witness substrate is git + Ed25519 signatures, not a blockchain.

What remained unspecified: the **exact operational protocol** — file layout,
seal format, signature scheme, ratification predicate, rotation/revocation
mechanics, v0→v1 migration. Deliberation `delib-20260414-f251` produced the
[Witness Charter v0 corpus](../founding/CONSTITUTION-v0.md) and this ADR is
its operational complement.

## Decision

Adopt the Witness Charter v0 protocol as specified in
[`docs/founding/CONSTITUTION-v0.md`](../founding/CONSTITUTION-v0.md). This ADR
records the **operational** choices; §1–§9 of the Charter records the
**constitutional** choices.

### 1. File layout

```
docs/founding/
├── CONSTITUTION-v0.md         ← authoritative (Witnesses sign SHA-256 of this)
├── CONSTITUTION-v0.lean       ← research consistency aid (not signed)
├── CONSTITUTION-v0.sha256     ← committed hex digest, reproducible from the .md
├── vetoers/
│   ├── REGISTRY.toml          ← additive-only: handle → pubkey
│   ├── <handle>.pub           ← Ed25519 public key, PEM or raw base64
│   ├── ROTATION-<handle>-<N>.md   ← signed by standing quorum
│   └── REVOCATION-<handle>.md     ← signed by standing quorum minus target
└── seals/
    └── v0/
        ├── <handle>-<N>.seal.md   ← append-only, N monotone per handle
        └── ...
```

The directory name `vetoers/` is preserved on disk for continuity; the
Charter itself uses the canonical term **witness**.

### 2. Seal format (normative)

YAML frontmatter + unsigned commentary. See Charter §4.3 for the schema.

**Canonical signed payload.** JSON-serialized frontmatter with `sig` removed,
keys sorted lexicographically, no trailing whitespace, UTF-8 encoding,
prefixed by the ASCII domain separator `"cosmon.charter.v0\n"`.

**Signature algorithm.** Ed25519 (`ed25519-dalek` v2). No key-derivation, no
HD wallets, no BLS aggregation — simplicity is the wedge.

### 3. `cs charter` commands

| Command | Role | Principal |
|---|---|---|
| `cs charter status` | Print current corpus hash, ratification state, witness set, last 10 seal events. Read-only. | Any |
| `cs charter hash` | Compute and print `SHA-256(canonical_utf8_bytes(CONSTITUTION-v0.md))`. | Any |
| `cs charter sign --witness <handle> --key <path>` | Produce a new seal file signed by the given key. Refuses if `handle` not in registry or key mismatch. | Witness |
| `cs charter retract --witness <handle> --seal <sha256>` | Produce a signed retraction seal. | Witness |
| `cs charter verify` | Verify every seal under `seals/v0/`: Ed25519 valid, handle resolves, `corpus_hash` matches current. Exit non-zero if ratification predicate false. | Any |
| `cs charter rotate --witness <handle>` | Additive rotation workflow: emit `ROTATION-<handle>-<N>.md` template for quorum to sign. | Operator |
| `cs charter revoke --witness <handle>` | Emit `REVOCATION-<handle>.md` template. Requires ≥2 remaining quorum signatures. | Operator |

Implementation target: `crates/cosmon-charter` (new crate), ~300 LOC over
`ed25519-dalek`, `serde_yaml`, `sha2`. No new runtime dependencies beyond
these.

### 4. Ratification predicate (evaluator)

```
Ratified(v0) :⇔  ∃ S₁, S₂, S₃ ⊆ seals/v0/ such that:
  - |distinct_handles(S₁, S₂)| ≥ 2
  - ∀ S ∈ {S₁, S₂}:
      - S.action = seal
      - ¬ ∃ R ∈ seals/v0/: R.action = retract ∧ R.retracts = sha256(S)
                          ∧ R.witness_handle = S.witness_handle
      - S.corpus_hash = cs_charter_hash()
      - S.witness_handle ∈ REGISTRY.toml
      - S.pubkey = REGISTRY.toml[S.witness_handle].pubkey
      - ed25519_verify(S.pubkey, S.sig, canonical_payload(S))
      - S.domain = "cosmon.charter.v0"
```

This predicate is evaluated **extra-theoretically** — by Rust code against
the filesystem, never by a closed Lean proof (Charter §7 anti-requirement 1;
Gödel sentence avoidance). The 2-of-3 count is hardcoded for v0; any change
is a v0→v1 migration trigger.

### 5. Rotation protocol

1. Witness `H` generates a new keypair; publishes public key as
   `vetoers/H.pub.v2`.
2. Operator generates a `ROTATION-H-<N>.md` template containing: old pubkey,
   new pubkey, effective-at timestamp, corpus hash at rotation time.
3. **Both remaining witnesses** sign the rotation file (Ed25519 over canonical
   JSON of its frontmatter, same pattern as seals).
4. New entry appended to `REGISTRY.toml` with `rotated_from: H.v1`.
5. Retiring key `H.v1` issues one final `action: retract` on its own prior
   sealed events as a visible audit trail (not required for ratification,
   required for operational hygiene).

Rotation is **not** a v0→v1 trigger — the witness set cardinality is
unchanged, and the public-key identity of the handle is preserved by the
quorum-signed rotation record.

### 6. Revocation protocol

Triggered when a witness is unresponsive or their key is compromised.

1. Operator generates `REVOCATION-H.md` with reason and effective timestamp.
2. At least two non-target witnesses sign.
3. From revocation timestamp onward, all seals by handle `H` are treated as
   retracted for the purpose of the ratification predicate.
4. A new witness onboarding is a **separate** discontinuity. If the post-
   revocation set has cardinality < 3, the Charter enters a *quorum-critical*
   state; this is a v0→v1 migration trigger (Charter §6).

### 7. v0→v1 migration (summary; full protocol in Charter §6)

Two-seal migration, never in-place amendment:

1. Compute `H1 = SHA-256(CONSTITUTION-v1.md)`.
2. Standing v0 quorum signs under `domain: cosmon.charter.v0.supersede`,
   body references `H1`.
3. Fresh 2-of-3 quorum (may overlap with v0 quorum) signs `H1` under
   `domain: cosmon.charter.v1`.
4. `cs charter status` now reports v1 as ratified.
5. v0 files and seals remain in git forever (P_trace).

### 8. What this ADR deliberately does NOT decide

- **Who the three witnesses are.** Identity selection is a separate
  operator-level decision, not a constitutional one.
- **Legal or jurisdictional meaning of the signatures.** The Charter is a
  technical governance artifact; its legal weight is zero until counsel
  evaluates it. P_external holds regardless.
- **Key storage strategy for witnesses.** Hardware tokens, passphrase-
  protected files, cloud HSMs — any of these produces a valid Ed25519 signature.
- **Rate limits, staking, slashing.** This is explicitly not a blockchain
  (ADR-033).

## Consequences

**Positive.**

- Ratification authority now has a byte-precise, testable definition.
- P_external is no longer aspirational (ADR-032): the protocol in §4 makes
  it mechanically enforceable.
- The two-file design (authoritative `.md` + research `.lean`) honors both
  the "Lean compiles = trust" faction (tolnay/godel/einstein) and the "prose
  is what humans sign" faction (jobs/torvalds/feynman) without making either
  the sole authority.
- Retraction symmetry (P_seal) means a compromised key has a bounded blast
  radius: retract + rotate.
- Zero new runtime dependencies; `cs charter` is a 300-LOC crate.

**Negative / risks.**

- Corpus hash is brittle to byte-level edits of `CONSTITUTION-v0.md`. Any
  whitespace change invalidates all seals. Mitigation: `cs charter hash`
  canonicalizes UTF-8 and normalizes line endings before hashing. The
  canonicalization rule is part of the Charter itself (§4.2 — SHA-256 of
  canonical UTF-8 bytes) and its Rust implementation is pinned by test
  vectors in `crates/cosmon-charter/tests/`.
- Lean 4 toolchain drift can render `CONSTITUTION-v0.lean` uncompilable.
  This is a signal, not a failure: authority lives in the `.md`. A
  permanent Lean kernel break is a v1 trigger (Charter §6).
- The Charter names no witnesses. It is a proposal until the genesis seal
  is committed. This is by design (the document cannot self-ratify) but
  means the artifact ships "inert" and depends on an external operator
  action to come into force.

## Alternatives considered (rejected)

- **Lean as the authoritative artifact (reject).** Too few humans parse Lean 4
  to form a diverse vetoer pool; shipping Lean-first filters the pool to zero
  (jobs). Mitigated by demoting `.lean` to research aid.
- **Blockchain / smart-contract ratification (reject).** Explicitly ruled out
  by ADR-033; also heavier than the problem demands (torvalds).
- **BLS aggregate signatures (reject).** Reduces seal file count from 3 to 1
  but breaks retraction symmetry (aggregate cannot be un-aggregated
  partially). Not worth the simplicity loss.
- **Timeout / silent ratification (reject).** Violates P_trace (einstein's
  "Silent Vetoer" gedanken): absence leaves no content-addressed trace.
- **6-section Constitution as originally proposed (reject).** 10/10 panelists
  cut it. Sparks taxonomy has no truth value; decommission criteria collapses
  into supersession (§6); the three-channels law is lore, not axiom.

## References

- [Witness Charter v0](../founding/CONSTITUTION-v0.md) — the ratified corpus.
- [CONSTITUTION-v0.lean](../founding/CONSTITUTION-v0.lean) — Lean kernel.
- [ADR-032 — P-external witness axiom](032-p-external-witness-axiom.md).
- ADR-033 — Drop blockchain, adopt noogram.
- [ADR-016 — Autonomy regimes](016-autonomy-regimes-and-resident-runtime.md).
- [ADR-030 — Cosmon archive model](030-cosmon-archive-model.md).
- Deliberation artifact: `.cosmon/state/fleets/default/molecules/delib-20260414-f251/synthesis.md`
  (panel: wheeler, einstein, godel, tolnay, torvalds, feynman, jobs, hawking, shannon, jr).
- Lore: an internal chronicle.
