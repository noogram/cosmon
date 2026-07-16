// SPDX-License-Identifier: AGPL-3.0-only

//! Notary Protocol v0 — test vectors.
//!
//! Three vectors, each encoding one structural invariant:
//!
//! 1. `duplicate_nonce_distinct_certs` — the same nonce used by two
//!    operators produces distinct commitments (the nonce is part of
//!    the signed payload, not a deduplication key).
//! 2. `semantically_identical_variants_same_content_hash` — two
//!    commitments that agree in every canonical field hash to the
//!    same `content_hash`, regardless of how they were constructed.
//! 3. `cross_validator_agreement` — an operator's signature verifies
//!    for any party that has the operator's public key. There is no
//!    hidden state tying verification to the original signer's
//!    process.

use cosmon_hash::Hash;
use cosmon_notary::commitment::{merkle_root_stub, Nonce};
use cosmon_notary::signature::{Ed25519Scheme, Scheme};
use cosmon_notary::{verify_seal, Commitment, Seal};

fn base_commitment_for(op: &Ed25519Scheme) -> Commitment {
    let pk = op.public_key();
    let pk_hash = Hash::of_bytes(&pk.to_bytes());
    Commitment {
        molecule_id: "task-20260420-1d61".into(),
        kind: "task".into(),
        prompt_content_hash: Hash::of_bytes(b"prompt.md canonical bytes"),
        briefing_seals_root: merkle_root_stub(&[Hash::of_bytes(b"step0")]),
        parent_commitments: vec![],
        formula_id: "task-work".into(),
        formula_version_hash: Hash::of_bytes(b"formula-toml"),
        cosmon_version: "0.1.0".into(),
        operator_pubkey: pk,
        validator_set_epoch: 0,
        validator_set_root: merkle_root_stub(&[pk_hash]),
        nucleated_at_unix_ms: 1_714_000_000_000,
        nonce: Nonce::from_bytes([0u8; 32]),
        dedup_key: None,
        canonical_version: 1,
    }
}

#[test]
fn duplicate_nonce_distinct_certs() {
    // Two different operators, same nonce — the resulting
    // commitments and seals are nevertheless distinct.
    let op_a = Ed25519Scheme::generate_from_seed([1u8; 32]);
    let op_b = Ed25519Scheme::generate_from_seed([2u8; 32]);

    let mut c_a = base_commitment_for(&op_a);
    let mut c_b = base_commitment_for(&op_b);
    let shared_nonce = Nonce::from_bytes([0xaa; 32]);
    c_a.nonce = shared_nonce;
    c_b.nonce = shared_nonce;

    // Content hashes differ because `operator_pubkey` is part of the
    // canonical bytes.
    assert_ne!(
        c_a.content_hash().unwrap(),
        c_b.content_hash().unwrap(),
        "commitments must not collide even when they share the nonce"
    );

    let seal_a = Seal::issue(c_a, &op_a).unwrap();
    let seal_b = Seal::issue(c_b, &op_b).unwrap();
    // Distinct signatures too.
    assert_ne!(seal_a.signature.bytes_hex, seal_b.signature.bytes_hex);

    // Both seals independently verify under their own operator key.
    verify_seal(&seal_a).unwrap();
    verify_seal(&seal_b).unwrap();
}

#[test]
fn semantically_identical_variants_same_content_hash() {
    // Two commitments constructed via structurally different paths
    // but agreeing in every canonical field must hash identically.
    let op = Ed25519Scheme::generate_from_seed([9u8; 32]);

    let c_direct = base_commitment_for(&op);

    // Build the same commitment by mutating a clone — same final
    // field values, exercised through a different assignment order.
    let mut c_indirect = base_commitment_for(&op);
    let saved_id = c_indirect.molecule_id.clone();
    c_indirect.molecule_id = "temporary".into();
    c_indirect.molecule_id = saved_id;
    c_indirect.dedup_key = None; // redundant; expresses the canonical spec

    assert_eq!(c_direct, c_indirect);
    assert_eq!(
        c_direct.canonical_bytes().unwrap(),
        c_indirect.canonical_bytes().unwrap(),
        "same fields → same canonical bytes"
    );
    assert_eq!(
        c_direct.content_hash().unwrap(),
        c_indirect.content_hash().unwrap(),
        "same canonical bytes → same content hash"
    );

    // Signatures over the same content_hash must verify interchangeably.
    let seal_a = Seal::issue(c_direct, &op).unwrap();
    let seal_b = Seal::issue(c_indirect, &op).unwrap();
    verify_seal(&seal_a).unwrap();
    verify_seal(&seal_b).unwrap();
}

#[test]
fn cross_validator_agreement() {
    // Third party receives only the seal (no extra state). They
    // must be able to verify using only what is in `Seal`.
    let operator = Ed25519Scheme::generate_from_seed([77u8; 32]);
    let commitment = base_commitment_for(&operator);

    let seal = Seal::issue(commitment, &operator).unwrap();

    // Serialize, ship over the wire, deserialize — mirror what a
    // validator would do.
    let wire = serde_json::to_string(&seal).unwrap();
    let received: Seal = serde_json::from_str(&wire).unwrap();

    verify_seal(&received).expect(
        "any holder of the seal + the operator's pubkey (embedded) must verify without extra state",
    );

    // Mutation on the receiving side is caught.
    let mut tampered = received.clone();
    tampered.commitment.formula_id = "different-formula".into();
    assert!(
        verify_seal(&tampered).is_err(),
        "tampering with the commitment on the wire must fail verification"
    );
}
