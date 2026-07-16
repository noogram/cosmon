// SPDX-License-Identifier: AGPL-3.0-only

//! The mint — an operator-signed attestation over a [`Commitment`].
//!
//! A [`Seal`] is the signed envelope the operator issues at mint
//! time. A [`NotarizationCertificate`] is a second signature layered on top
//! by a validator; phase-0 does not emit certificates (there is no
//! remote validator yet) but the type is defined so phase-2 can land
//! without reshaping the crate.
//!
//! The two-layer design is deliberate: a [`Seal`] alone proves
//! operator presence (`PoSP`). A [`NotarizationCertificate`] adds validator
//! witness, turning it into a replayable receipt. Neither is a
//! blockchain; there is no consensus, no ordering, no token.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commitment::{Commitment, CommitmentError};
use crate::signature::{PublicKey, Scheme, Signature, SigningError};

/// Errors raised while producing or validating a mint.
#[derive(Debug, thiserror::Error)]
pub enum NotaryError {
    /// The underlying commitment could not be canonicalized.
    #[error("commitment error: {0}")]
    Commitment(#[from] CommitmentError),
    /// The signer refused or encountered a key-material problem.
    #[error("signing error: {0}")]
    Signing(#[from] SigningError),
    /// The commitment's `operator_pubkey` field does not match the
    /// public key derived from the provided signer. Prevents the
    /// "signs with key A, claims to be key B" confusion.
    #[error("operator pubkey in commitment does not match signer pubkey")]
    OperatorPubkeyMismatch,
}

/// A [`Seal`] — the operator's signature over a commitment.
///
/// Serialized form is JSON with the commitment, signature envelope,
/// and sealing wall-clock. The commitment is stored *in full* (not
/// just its hash) so a verifier can recompute `content_hash` and
/// check the signature without needing any external lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seal {
    /// The facts that were signed.
    pub commitment: Commitment,
    /// The operator's signature over
    /// `commitment.content_hash().as_bytes()`.
    pub signature: Signature,
    /// Wall-clock time the seal was produced. Informational — the
    /// binding timestamp is `commitment.nucleated_at_unix_ms`.
    pub sealed_at: DateTime<Utc>,
}

impl Seal {
    /// Produce a seal by signing the commitment with the given scheme.
    ///
    /// The signer's public key must match `commitment.operator_pubkey`
    /// — otherwise [`NotaryError::OperatorPubkeyMismatch`] is returned.
    /// This guards against the silent error where a caller populates
    /// the commitment with one pubkey and signs with a different key.
    ///
    /// # Errors
    ///
    /// Returns a [`NotaryError`] on canonicalization failure, signing
    /// failure, or pubkey mismatch.
    pub fn issue<S: Scheme>(commitment: Commitment, scheme: &S) -> Result<Self, NotaryError> {
        if commitment.operator_pubkey != scheme.public_key() {
            return Err(NotaryError::OperatorPubkeyMismatch);
        }
        let digest = commitment.content_hash()?;
        let signature = scheme.sign(digest.as_bytes())?;
        Ok(Self {
            commitment,
            signature,
            sealed_at: Utc::now(),
        })
    }

    /// The operator public key recorded in the commitment.
    #[must_use]
    pub fn operator_pubkey(&self) -> &PublicKey {
        &self.commitment.operator_pubkey
    }
}

/// A [`NotarizationCertificate`] — a validator's counter-signature over a
/// [`Seal`].
///
/// Phase-0 does not produce certificates (there is no validator yet).
/// The type is declared so phase-2 can land without reshaping
/// on-disk records: an unsigned mint is just `certificate: None`.
///
/// The validator signs the tuple `(seal_signature ||
/// validator_time_unix_ms || validator_nonce)` — explicit in the
/// canonical form so a verifier can reconstruct the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotarizationCertificate {
    /// Wall-clock time the validator received the seal. Unix-ms
    /// (signed i64) to match the commitment's timestamp convention.
    pub validator_time_unix_ms: i64,
    /// Fresh 256-bit random nonce drawn by the validator. Hex-encoded.
    pub validator_nonce_hex: String,
    /// Validator public key.
    pub validator_pubkey: PublicKey,
    /// Signature over `(seal.signature || validator_time ||
    /// validator_nonce)`.
    pub signature: Signature,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commitment::{merkle_root_stub, Nonce};
    use crate::signature::Ed25519Scheme;
    use cosmon_hash::Hash;

    fn fixture(operator: &Ed25519Scheme) -> Commitment {
        let pk = operator.public_key();
        let pk_hash = Hash::of_bytes(&pk.to_bytes());
        Commitment {
            molecule_id: "task-xxx".into(),
            kind: "task".into(),
            prompt_content_hash: Hash::of_bytes(b"prompt"),
            briefing_seals_root: merkle_root_stub(&[Hash::of_bytes(b"step0")]),
            parent_commitments: vec![],
            formula_id: "task-work".into(),
            formula_version_hash: Hash::of_bytes(b"formula"),
            cosmon_version: "0.1.0".into(),
            operator_pubkey: pk,
            validator_set_epoch: 0,
            validator_set_root: merkle_root_stub(&[pk_hash]),
            nucleated_at_unix_ms: 1_714_000_000_000,
            nonce: Nonce::from_bytes([1u8; 32]),
            dedup_key: None,
            canonical_version: 1,
        }
    }

    #[test]
    fn issue_signs_commitment() {
        let op = Ed25519Scheme::generate_from_seed([10u8; 32]);
        let seal = Seal::issue(fixture(&op), &op).unwrap();
        // Sanity: the signature is non-empty.
        assert!(!seal.signature.bytes_hex.is_empty());
    }

    #[test]
    fn issue_refuses_mismatched_pubkey() {
        let op = Ed25519Scheme::generate_from_seed([11u8; 32]);
        let other = Ed25519Scheme::generate_from_seed([12u8; 32]);
        // Commitment claims `op`'s pubkey but we sign with `other`.
        let err = Seal::issue(fixture(&op), &other).unwrap_err();
        assert!(matches!(err, NotaryError::OperatorPubkeyMismatch));
    }

    #[test]
    fn seal_serde_roundtrip() {
        let op = Ed25519Scheme::generate_from_seed([13u8; 32]);
        let seal = Seal::issue(fixture(&op), &op).unwrap();
        let j = serde_json::to_string(&seal).unwrap();
        let back: Seal = serde_json::from_str(&j).unwrap();
        assert_eq!(seal, back);
    }
}
