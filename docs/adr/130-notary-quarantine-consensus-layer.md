# ADR-130 — `cosmon-notary`: keep the signature primitive, quarantine the consensus layer

**Status:** accepted
**Date:** 2026-06-23
**Parent task:** `task-20260622-25f1` (keep-or-quarantine decision)
**Parent deliberation:** `delib-20260622-187a` (pre-publication adversarial architecture review)
**Supersedes (in part):** [ADR-056](056-notary-protocol-v0.md) — Notary Protocol v0 (status was already `proposed`, never `accepted`)
**Stress-tested by:** buterin (mechanism-design steelman), feynman (cut-line verification)
**Coordination:** code surgery folds into C1 / `task-20260622-eeb9` (workspace trim)

## The question

Does the `cosmon-notary` crate (~1,340 LOC) earn its keep against a real
adversary before the public flip, or is it quarantined until one exists?

The single binding test, from the review: **is there a named adversary for
whom the signer and the file-owner are different principals?** A signature
over a file that the signer can rewrite at will protects against nobody —
that is the cargo-cult pattern, the form of integrity without the substance.

## What we found: the crate is two strata, not one

The review framed this as keep-or-cut the *whole crate*. That framing is
wrong, and the deletion test in the finding was incomplete. The crate splits
cleanly along its own `use` graph (verified: `signature.rs` imports only
`serde`; every dependency arrow points the other way, `commitment.rs:121`,
`notarization.rs:20`, `verify.rs:18` → `signature`):

### Stratum A — the signature primitive (`src/signature.rs`). **KEEP.**

`Ed25519Scheme`, `PublicKey`, the `Scheme` trait, `sign`/`verify`. This is a
genuine reused crypto primitive with a **real second principal**:

- **`cosmon-rpp-adapter::scope_badge`** (a crate in the default ship closure,
  `Cargo.toml:77`) calls `Ed25519Scheme::verify` (`scope_badge.rs:438`) to
  check a detached federation badge: instance *Dave* signs, instance *Casey*
  verifies, offline and sneakernet-first. The test suite exercises an
  `impostor()` key that must fail. **signer ≠ verifier.** This is the
  auth≠authz badge of ADR-0023, and it is load-bearing — delete the crate
  wholesale and the default build breaks.
- The **pitch-deck path** (`theater/pitch-ilb`) signs slide bytes with
  `cs notarize` and ships `slides.notarization.json` to an external party who
  runs `cs notarize verify`. **Noogram signs → investor verifies.** Again
  signer ≠ verifier. A right-sized detached signature over content earns its
  keep here, exactly as a PGP-signed release does: the trust anchor (key
  distribution) is out of band, which is normal, not a flaw.

### Stratum B — the consensus scaffolding. **QUARANTINE.**

`validator_set_root`, `validator_set_epoch` (`commitment.rs:126-131`),
`parent_commitments`-as-Merkle (`:108`), `briefing_seals_root` (`:105`),
`merkle_root_stub` (`:274-298`), `NotarizationCertificate`
(`notarization.rs:98`), `BlsStub` (inside `signature.rs:300-344`), and the
phase-1/2/3 roadmap. This is the vocabulary of a consensus protocol —
validator sets with epochs, Merkle roots, countersignatures — stacked on a
self-signed hash.

**It protects against nobody today.** `validator_set_epoch` is always `0`;
`validator_set_root` is always `merkle_root_stub(&[operator_pubkey])`, a
singleton self-set; `NotarizationCertificate` is never constructed. The
operator owns both the key and the file.

The strongest pro-keep argument (buterin's steelman) is the
*anti-painting-into-a-corner* property the code itself claims
(`commitment.rs:128-131`): signing `validator_set_root` into every commitment
from epoch 0 stops a *future* validator from claiming retroactive authority
over already-minted seals. The property is real — but the adversary is not
present, **because you cannot paint into a corner an adversary who holds the
paintbrush and the wall.** If a future validator could forge retroactive
authority, the operator can equally re-mint every historical seal under any
`validator_set_root` they choose — the byte-layout commitment binds the only
party who can also re-issue. The property switches on the *day a second
validator's signature exists that the operator cannot produce* — which is
phase-2 by ADR-056's own roadmap (`056:118`), not today.

feynman's sharpening, adopted verbatim into this decision: the
`cross_validator_agreement` test vector and the `NotarizationCertificate`
type **test a coalition that has no members. A mechanism whose only test
exercises an empty validator set is not collusion-resistant — it is
collusion-*shaped*.** Ship the shape the day the second signer is named, not
before.

## Decision

1. **KEEP `src/signature.rs` unchanged** — Ed25519 sign/verify is a real
   primitive with a real signer≠verifier consumer (`scope_badge`) in the
   default ship closure.

2. **KEEP a right-sized `Seal`** — a detached Ed25519 signature over a
   content hash, for the pitch-deck signer≠verifier case. The minimum
   load-bearing commitment is: `content_hash` (the attested bytes),
   `operator_pubkey` (the signer, checked at `notarization.rs:69`), `nonce`
   (`commitment.rs:137`, blocks signature collision), `canonical_version`
   (`:146`, verifier refuses on drift), and the `DOMAIN_SEPARATOR` +
   `canonical_bytes`/`content_hash` machinery (`:73,156-188`, anti-cross-
   protocol-replay), wrapped by `Seal { commitment, signature, sealed_at }`.

3. **QUARANTINE Stratum B** until a named non-operator validator ships
   (the remote Noogram witness of ADR-055/ADR-056 phase-2). Quarantine means:
   - strip `validator_set_root`, `validator_set_epoch`, `parent_commitments`,
     `briefing_seals_root`, `merkle_root_stub`, `NotarizationCertificate`,
     and `BlsStub` from the active build; **or** feature-gate them behind
     `--cfg notary_phase2`, off by default;
   - `validator_set_*` *fields* MAY be retained in the `Commitment` struct as
     **explicitly-documented reserved/inert fields** (the one defensible
     sliver: it lets a phase-2 validator land as an additive swap without a
     canonical-form bump) — but they are documented as reserved, never as
     active consensus state. The `merkle_root_stub`, the `Certificate`, the
     `BlsStub`, and the epoch-increment machinery are pure ceremony and are
     removed, not merely gated.

4. **KEEP the §8b briefing seal regardless.** The CLAUDE.md §8b seal,
   `cs verify`, and the one-BLAKE3-over-`prompt.md` trace are honest and
   right-sized — they are described as "a trace, not a lock" and make no
   claim the code cannot deliver. This decision does not touch them.

5. **Fold the code surgery into C1 / `task-20260622-eeb9`** (workspace trim).
   This molecule is the *decision*; the excision is a deliberate, tested edit
   that changes the canonical byte layout (it will break existing `mint.json`
   / `slides.notarization.json` files) and therefore belongs in the trim PR
   with its own test pass, not smuggled through here. **This molecule
   intentionally ships no `cosmon-notary` code change** — deleting a
   1,340-LOC crate on a decision-shaped molecule would be exactly the
   reflex-delete the deliberation warned against.

## Consequences

- The default build keeps a small, honest Ed25519 primitive that two real
  signer≠verifier consumers depend on. No present-day capability is lost.
- The consensus vocabulary that reads as a blockchain — and would read, to a
  pre-publication external reviewer, as cargo-cult cryptography — is removed
  from the shipped surface until the adversary it presupposes exists.
- ADR-056 is downgraded from `proposed` to **superseded-in-part**: its
  signature primitive and right-sized seal survive; its phase-2/3 consensus
  roadmap is parked, not deleted, and re-activates the day a named remote
  validator ships (at which point a successor ADR re-opens Stratum B with a
  real adversary model and its byte layout is re-justified, not assumed).
- The `cs notarize` / `cs key` CLI verbs remain, scoped to the right-sized
  seal. Their `--help` / `man cs` text must drop the validator-set/epoch
  language when C1 executes the strip (CLI-doc-sync invariant).

## Tattoo

> A validator set with no second principal is a wooden control tower — the
> form of integrity without the substance. Keep what has a second principal;
> strip what only has a paintbrush and a wall. Ship the shape the day the
> second signer is named, not before.
