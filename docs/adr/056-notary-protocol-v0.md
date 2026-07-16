# ADR-056 — Notary Protocol v0 (`cosmon-notary` crate, canonical form v1, operator signature)

**Status:** superseded-in-part by [ADR-130](130-notary-quarantine-consensus-layer.md) (2026-06-23) — the Ed25519 signature primitive and a right-sized detached `Seal` survive (real signer≠verifier consumers exist); the phase-2/3 consensus layer below (validator set, epochs, Merkle roots, `NotarizationCertificate`) is **quarantined** until a named non-operator validator ships. Was never `accepted`; the roadmap is parked, not deleted. The original status note follows.

**Original status:** proposed (not accepted — promotion requires the phase-2 validator-countersignature layer to ship and the byte layout to survive one cross-galaxy round-trip)
**Date:** 2026-04-20
**Parent task:** `task-20260420-81ec` (manual finalization in session, worker ghost-completed commit prose)
**Governing deliberation:** `delib-20260420-bae4` — 9-persona panel (wheeler, shannon, niel, godin, hawking, knuth, einstein, turing, feynman)
**Related chronicles:**
- *Le mot "mint" qui valait 2,56 $* (`ebc0ec9a9`) — knuth's epistemic-debt audit
- *La monnaie que personne n'avait prévu de frapper* (`a37f971dd`) — articulation of the three strata
- *Le triangle qui se mord la queue* (`c857d8d2f`) — the three-deliberation recognition
- *Le triangle qui s'est résolu* (`f84f6c8e5`) — final convergence in four layers
**Related ADRs:**
- [ADR-052](052-one-ledger-one-writer-one-witness.md) — one ledger, one writer, one witness
- [ADR-055](055-cosmon-residence.md) — cosmon residence (phase 2 preparation for remote witnessing)
- [ADR-030](030-cosmon-archive-model.md) — archive format (terminal snapshots)
- [ADR-047](047-event-log-protocol-v0.md) — event-log substrate

## Context

`delib-20260420-bae4` brought a 9-persona panel onto the question *"what does it mean for a molecule to be minted?"*. Five verdicts converged; the third reached consensus from six of them independently: **the existing `BriefingSeal` is the right seed, but it needs five additive fields before it can carry evidential weight**. knuth drafted the full spec. einstein + turing prescribed the honest primitive name: *notarize*, not *mint*.

A parallel-session conflict landed a `crates/cosmon-mint` skeleton with the rejected *mint* name (SEO collision with Mint Protocol on Dymension). This ADR governs the renamed surface: `crates/cosmon-notary` + `cs notarize`.

## Honest naming

Marketing may call the combined artifact *"proof-of-cognition"*. The **code, the types, and the docs in this crate** do not. The honest primitives:

- **Proof-of-Sealed-Presence (PoSP).** A signed attestation that a specific content hash (prompt + briefing seals + formula version + operator key) existed at a specific moment. Nothing more.
- **Proof-of-LLM-Expenditure (PoLE).** An economic attestation that tokens were burned producing the content. Not implemented in v0 — the schema reserves an `energy_receipt` slot that a future version will populate from `claudion`.

The combination is what the panel calls *notarization*. Not cognition. Not creation. **Notarization.**

## Commitment schema

A `Commitment` is the canonical tuple an operator commits to at notarize time:

```text
{
  molecule_id            : MoleculeId    // newtype, UTF-8 ASCII
  kind                   : MoleculeKind  // idea | task | decision | issue | signal | deliberation
  prompt_content_hash    : Blake3Hash    // over canonical prompt bytes
  briefing_seals_root    : Blake3Hash    // Merkle root over [BriefingSeal]
  parent_commitments     : Vec<Blake3Hash>  // operator-declared parent DAG edges (empty = root)
  formula_id             : FormulaId     // newtype
  formula_version_hash   : Blake3Hash    // hash of the formula TOML at commit time
  cosmon_version         : String        // crate version emitting the notarization
  operator_pubkey        : PublicKey     // Ed25519 32-byte
  validator_set_epoch    : u64           // 0 in phase 1 (solo); >0 once validator set is known
  nucleated_at_unix_ms   : i64           // operator's wall clock
  nonce                  : [u8; 32]      // OS randomness, 256 bits
}
```

## Seal + Notarization + Certificate

Three layered objects, each named for what it proves:

- **Seal** = `BLAKE3(canonical(Commitment))` — content-invariant. Deterministic. Any operator with the same `Commitment` reaches the same `Seal`.
- **Notarization** = `Sign_{sk_operator}(Seal)` — operator-authored attestation. Proves key-holder committed to the Seal at a specific time.
- **NotarizationCertificate** (phase 2+) = `Sign_{sk_validator}(Notarization || validator_time || validator_nonce)` — countersigned by a validator (Noogram or federated). Optional in v0.

## Canonical form v1

To make `Seal` deterministic across operators, canonical serialization MUST:

1. Decode input as UTF-8, rejecting invalid sequences (no `U+FFFD` substitution).
2. Strip leading BOM.
3. Normalize to NFC.
4. Replace CRLF and CR with LF.
5. Ensure exactly one trailing LF.
6. Forbid floating-point values in the commitment schema — integers and decimal strings only.
7. Encode timestamps as `i64` unix milliseconds (never signed ISO-8601 strings).
8. Prefix the BLAKE3 input with the domain separator `b"cosmon-notary/v1/commitment\x00"`.

The `canonical_version` field (currently `1`) is embedded in the commitment to let future canonicalization revisions coexist with v1 notarizations.

## Signature scheme

- **Primary:** Ed25519 (pure, 64-byte signatures, 32-byte public keys). `ed25519-dalek` crate.
- **Secondary (stub in v0, real in phase 2):** BLS12-381 for signature aggregation across validators. A stub `BlsStub` implementation exists behind the `Scheme` trait so phase-2 code lands as an additive swap.
- **Reserved slot (schema only, no implementation):** Dilithium3 for post-quantum insurance. ~30 MB payload if populated; not populated in v0.

The `Scheme` trait (not an enum) prevents scheme-lock-in: a future scheme can be added by implementing the trait, without forcing every verifier to handle a new enum variant.

## Invariants

| ID | Invariant | Enforcement |
|----|-----------|-------------|
| **I1** | `Seal` is deterministic for a given canonical `Commitment`. | Test `semantically_identical_variants_same_content_hash`. |
| **I2** | An operator can verify a `Notarization` without the validator's cooperation (phase-1 autonomy). | Test `happy_path_verifies`. |
| **I3** | Validator revocation is **forbidden**. Dis-attestation (adding a new signed "this notarization is contested" record) is permitted. | Schema-level — the protocol has no `revoke` verb. |
| **I4** | `witnesses` is `Vec<Witness>` (plural). Never `Option<Witness>`. | Type-level. |
| **I5** | Agents cannot notarize on their own account. The root of trust is operator-key-only. | CLI-level — `cs notarize` refuses when `COSMON_PARENT_MOL_ID` is set in the environment. |
| **I6** | Individual notarizations are **not tradable** at the protocol level. The asset is the DAG of notarizations, not any single notarization. | Protocol omission — no transfer verb, no ledger. |
| **I7** | `canonical_version: u8` starts at `1`. Legacy unsealed artifacts carry `0` and are readable but not verifiable. | `SealVerifyError::UnsupportedCanonicalVersion`. |

## What this crate is *not*

- Not a blockchain. No consensus, no ordering, no token.
- Not a lock. A notarization is a **trace, not an enforcement point**.
- Not a PKI. Operator public keys are bring-your-own-filesystem.
- Not multi-party. Validators are a phase-2 concern.

## Why not mint

- **SEO collision:** *Mint Protocol* (Dymension-based blockchain for NFT + Creator Economy) occupies the search term.
- **Epistemic debt:** knuth's audit (chronicle *"Le mot 'mint' qui valait 2,56 $"*) — "mint" promises cryptographic authority that the code cannot deliver without signatures + validator countersigning. Calling a BLAKE3 hash a "seal" and an Ed25519 signature a "mint" is a category error.
- **Panel verdict (bae4):** einstein explicit — *"drop 'mint' for 'notarize'"*. turing explicit — *"the operator is building an extremely well-architected notary service and calling it 'proof-of-cognition'"*.

The rename does not change a single byte of the protocol. It changes the name by which the protocol is searched, cited, and taught.

## Phase roadmap

| Phase | Crate surface | Notes |
|-------|---------------|-------|
| **0 (today)** | Commitment schema + canonical encoding + Ed25519 local sign/verify + `cs notarize` CLI + 3 knuth test vectors. | No validator, no HTTP, no remote witness. Operator self-notarizes. |
| **1** | BLS12-381 scheme impl (stub today at `BlsStub`); Merkle `parent_commitments` activated. | Still single-operator. |
| **2** | Remote Noogram validator (HTTP over ADR-055 `remote` residence). `NotarizationCertificate` emitted by second signer. | Phase 2 of the Noogram trajectory (delib bae4 §roadmap). |
| **3** | Validator set epochs (sorted-pubkey root), dis-attestation rules, Certificate Transparency-style log layer. | **Consensus-free** (hawking: phase 2→3 is topology soft-fork, not blockchain-hard migration, **unless** molecule tradability is in scope — which it is not). |

## Relation to ADR-055 (Residence)

Notary v0 is the **phase-2 preparation** for the `remote` residence. In `solo`/`team`/`encrypted` residences a molecule can live its whole life without ever being notarized — notarization is opt-in. In `remote` residence, notarization becomes the natural handshake with the server: the operator signs locally, the server countersigns and stores the certificate.

The four residences and the notarization layer are **orthogonal**: notarization adds proof on top of any residence; residence decides where the bytes live.

## Open questions

- **Key management.** `cs notarize --key` accepts a raw Ed25519 file. A future ADR must cover rotation, revocation, and multi-device use. Touched but not solved in v0.
- **Agent minting root-of-trust.** I5 says agents cannot notarize on their own account. The operator-key model has not been stress-tested against high-volume autonomous workflows.
- **Energy receipt.** `PoLE` is named but unpopulated. Phase 1+ must integrate `claudion` token metering into `Commitment`.

## Test vectors (v0)

Three knuth-drafted vectors in `crates/cosmon-notary/tests/test_vectors.rs`:

1. `duplicate_nonce_distinct_certs` — forbid exact duplicate; distinct nonces produce distinct Seals.
2. `semantically_identical_variants_same_content_hash` — NFD/NFC/CRLF/LF/BOM-equivalent inputs produce the same Seal after canonicalization.
3. `cross_validator_agreement` — two validators signing the same `Commitment` produce signatures that both verify against the same `Seal`.

All three pass as of 2026-04-20 landing.

## Governance

If this ADR and the code disagree, **the ADR wins** — file a bead to fix the code.

When this ADR transitions to `accepted`, a chronicle entry lands marking the moment. Until then the crate is callable and testable but explicitly pre-stable (canonical form, field additions, scheme choices are all still subject to change).
