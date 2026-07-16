// SPDX-License-Identifier: AGPL-3.0-only

//! Seal verification — recompute and re-check.
//!
//! Verification is **offline and self-contained**: given a [`Seal`],
//! recompute the canonical commitment bytes, reconstruct the
//! content hash, and verify the signature under the scheme advertised
//! by the commitment's `operator_pubkey.tag`. No network, no
//! external state.
//!
//! Return style mirrors `cs verify`: `Ok(())` when every check
//! passes, otherwise a typed [`SealVerifyError`]. Callers that want
//! exit-code semantics should mirror the convention in
//! `crates/cosmon-cli/src/cmd/verify.rs`.

use crate::commitment::CommitmentError;
use crate::notarization::Seal;
use crate::signature::{Ed25519Scheme, Scheme, SigningError};

/// A verification failure reason.
#[derive(Debug, thiserror::Error)]
pub enum SealVerifyError {
    /// Canonical form could not be recomputed.
    #[error("commitment canonicalization failed: {0}")]
    Canonical(#[from] CommitmentError),
    /// Signature check failed for the advertised scheme.
    #[error("signature invalid: {0}")]
    Signature(#[from] SigningError),
    /// The commitment advertises a scheme this build does not know.
    #[error("unknown signature scheme tag: {0}")]
    UnknownScheme(String),
    /// The commitment's `canonical_version` is not supported by this
    /// build. The verifier must refuse rather than re-interpret.
    #[error("unsupported canonical_version {0}")]
    UnsupportedVersion(u8),
}

/// Verify a seal.
///
/// # Errors
///
/// Returns [`SealVerifyError`] on any failure: canonical-form
/// mismatch, signature check failure, unknown scheme, or
/// version-drift.
pub fn verify_seal(seal: &Seal) -> Result<(), SealVerifyError> {
    if seal.commitment.canonical_version != crate::commitment::CANONICAL_COMMITMENT_VERSION {
        return Err(SealVerifyError::UnsupportedVersion(
            seal.commitment.canonical_version,
        ));
    }
    let digest = seal.commitment.content_hash()?;
    let tag = seal.commitment.operator_pubkey.tag.as_str();
    match tag {
        Ed25519Scheme::TAG => {
            Ed25519Scheme::verify(
                &seal.commitment.operator_pubkey,
                digest.as_bytes(),
                &seal.signature,
            )?;
            Ok(())
        }
        other => Err(SealVerifyError::UnknownScheme(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commitment::{merkle_root_stub, Commitment, Nonce};
    use crate::notarization::Seal;
    use crate::signature::Ed25519Scheme;
    use cosmon_hash::Hash;

    fn fixture() -> (Ed25519Scheme, Commitment) {
        let op = Ed25519Scheme::generate_from_seed([77u8; 32]);
        let pk = op.public_key();
        let pk_hash = Hash::of_bytes(&pk.to_bytes());
        let c = Commitment {
            molecule_id: "task-xx".into(),
            kind: "task".into(),
            prompt_content_hash: Hash::of_bytes(b"p"),
            briefing_seals_root: merkle_root_stub(&[Hash::of_bytes(b"b0")]),
            parent_commitments: vec![],
            formula_id: "task-work".into(),
            formula_version_hash: Hash::of_bytes(b"f"),
            cosmon_version: "0.1.0".into(),
            operator_pubkey: pk,
            validator_set_epoch: 0,
            validator_set_root: merkle_root_stub(&[pk_hash]),
            nucleated_at_unix_ms: 1_714_000_000_000,
            nonce: Nonce::from_bytes([42u8; 32]),
            dedup_key: None,
            canonical_version: 1,
        };
        (op, c)
    }

    #[test]
    fn happy_path_verifies() {
        let (op, c) = fixture();
        let seal = Seal::issue(c, &op).unwrap();
        verify_seal(&seal).unwrap();
    }

    #[test]
    fn edit_to_commitment_fails_verification() {
        let (op, c) = fixture();
        let mut seal = Seal::issue(c, &op).unwrap();
        // Tamper with the molecule id post-seal.
        seal.commitment.molecule_id = "task-yy".into();
        assert!(matches!(
            verify_seal(&seal),
            Err(SealVerifyError::Signature(SigningError::VerifyFailed))
        ));
    }

    #[test]
    fn unsupported_canonical_version_refused() {
        let (op, mut c) = fixture();
        c.canonical_version = 1;
        let seal_ok = Seal::issue(c.clone(), &op).unwrap();
        let mut seal_bad = seal_ok;
        seal_bad.commitment.canonical_version = 2;
        assert!(matches!(
            verify_seal(&seal_bad),
            Err(SealVerifyError::UnsupportedVersion(2))
        ));
    }
}
