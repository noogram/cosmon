// SPDX-License-Identifier: AGPL-3.0-only

//! Signature schemes — the pluggable surface behind mint signing.
//!
//! The task prompt explicitly **forbids** an enum-based scheme
//! registry: locking the set of schemes in a sum type means a phase-2
//! BLS addition would break the byte layout of every persisted mint.
//! Instead we expose a [`Scheme`] trait and let each impl live behind
//! its own public key / signature newtype.
//!
//! Today this crate ships one real impl:
//!
//! - [`Ed25519Scheme`] — the MVP. BSD-3-Clause, audited, deterministic
//!   signing, 32-byte pubkeys and 64-byte signatures. This is what
//!   `cs notarize` uses.
//!
//! A second slot — [`BlsStub`] — is declared as a type with a
//! placeholder `impl Scheme` that only constructs empty signatures
//! and always verifies them as `false`. Its sole purpose is to make
//! phase-1 integration a single-file swap: the schema already has
//! somewhere for a BLS public key to live, and wiring a real
//! `blst`-backed impl means replacing the stub's method bodies, not
//! reshaping the crate.
//!
//! A third slot (post-quantum) is reserved in the ADR (Dilithium3)
//! but not populated here — we do not ship empty Dilithium plumbing
//! until NIST finalizes.

use serde::{Deserialize, Serialize};

/// Errors raised by a signing or verification operation.
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    /// The signer's secret key material was malformed or corrupt.
    #[error("invalid signing key: {0}")]
    BadKey(String),
    /// Verification failed: signature does not match the public key
    /// over the given bytes.
    #[error("signature verification failed")]
    VerifyFailed,
    /// A scheme-specific decoding error (e.g. malformed Ed25519
    /// public-key bytes).
    #[error("signature scheme encoding error: {0}")]
    Encoding(String),
}

/// Trait for a concrete signature scheme.
///
/// Implementors own the associated public-key / signature types. A
/// mint is a [`Signature`] over the commitment's
/// `content_hash().as_bytes()`, plus the [`PublicKey`] needed to
/// verify it.
///
/// The trait is intentionally narrow — no batch verification, no
/// aggregation, no precomputed-table optimization. Phase-1 BLS may
/// extend this surface; phase-0 does not.
pub trait Scheme {
    /// Sign the given bytes and return the opaque [`Signature`].
    ///
    /// # Errors
    ///
    /// Returns [`SigningError::BadKey`] if the scheme's internal key
    /// state is corrupt.
    fn sign(&self, bytes: &[u8]) -> Result<Signature, SigningError>;

    /// Verify that `signature` is a valid [`Scheme::sign`] output from
    /// `public_key` over `bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`SigningError::VerifyFailed`] if the check fails, or
    /// [`SigningError::Encoding`] if the public key / signature
    /// cannot be decoded for this scheme.
    fn verify(
        public_key: &PublicKey,
        bytes: &[u8],
        signature: &Signature,
    ) -> Result<(), SigningError>
    where
        Self: Sized;

    /// The public key matching this signer's secret.
    fn public_key(&self) -> PublicKey;

    /// A short human-readable tag (`"ed25519"`, `"bls12-381"`). Used
    /// in canonical encoding so that verifiers can route to the right
    /// [`Scheme::verify`] impl.
    fn tag(&self) -> &'static str;
}

/// An opaque public-key envelope.
///
/// Wraps the raw bytes of whatever scheme produced it. The `tag`
/// field carries the scheme identifier so a verifier can dispatch.
/// We avoid a trait object here to keep the type `Serialize` —
/// commitments must round-trip through JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKey {
    /// Scheme tag (`"ed25519"`, …). Matches [`Scheme::tag`].
    pub tag: String,
    /// Raw public-key bytes, hex-encoded (lowercase).
    pub bytes_hex: String,
}

impl PublicKey {
    /// Decode the hex-encoded bytes back to a `Vec<u8>`.
    ///
    /// # Errors
    ///
    /// Returns [`SigningError::Encoding`] if the hex is malformed.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        hex_decode(&self.bytes_hex).unwrap_or_default()
    }

    /// Construct a public key envelope from raw bytes + scheme tag.
    #[must_use]
    pub fn new(tag: &str, bytes: &[u8]) -> Self {
        Self {
            tag: tag.to_owned(),
            bytes_hex: hex_encode(bytes),
        }
    }
}

/// An opaque signature envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// Scheme tag (matches the issuing [`PublicKey::tag`]).
    pub tag: String,
    /// Raw signature bytes, hex-encoded (lowercase).
    pub bytes_hex: String,
}

impl Signature {
    /// Construct a signature envelope from raw bytes + scheme tag.
    #[must_use]
    pub fn new(tag: &str, bytes: &[u8]) -> Self {
        Self {
            tag: tag.to_owned(),
            bytes_hex: hex_encode(bytes),
        }
    }

    /// Decode the hex-encoded bytes back to a `Vec<u8>`.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        hex_decode(&self.bytes_hex).unwrap_or_default()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string has odd length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let hi = nibble(chunk[0])?;
        let lo = nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("non-hex character"),
    }
}

// ---------------------------------------------------------------------
// Ed25519 — the MVP scheme.
// ---------------------------------------------------------------------

/// Ed25519 implementation of [`Scheme`].
///
/// Wraps a [`ed25519_dalek::SigningKey`] and delegates to its `sign`
/// method. Keys can be generated fresh ([`Ed25519Scheme::generate`]),
/// derived deterministically from a seed
/// ([`Ed25519Scheme::generate_from_seed`], useful for tests), or
/// loaded from raw 32-byte secret bytes
/// ([`Ed25519Scheme::from_secret_bytes`]).
pub struct Ed25519Scheme {
    signing_key: ed25519_dalek::SigningKey,
}

impl Ed25519Scheme {
    /// Scheme tag (`"ed25519"`).
    pub const TAG: &'static str = "ed25519";

    /// Draw a fresh keypair from the OS RNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut rng = rand_core::OsRng;
        Self {
            signing_key: ed25519_dalek::SigningKey::generate(&mut rng),
        }
    }

    /// Deterministic keygen from a 32-byte seed. For tests and
    /// reproducible cross-validator fixtures — **never** call this
    /// with a non-random seed in production.
    #[must_use]
    pub fn generate_from_seed(seed: [u8; 32]) -> Self {
        Self {
            signing_key: ed25519_dalek::SigningKey::from_bytes(&seed),
        }
    }

    /// Load a scheme from raw 32-byte secret-key bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SigningError::BadKey`] if the bytes are not a valid
    /// Ed25519 secret.
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self, SigningError> {
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| SigningError::BadKey("ed25519 secret must be 32 bytes".into()))?;
        Ok(Self {
            signing_key: ed25519_dalek::SigningKey::from_bytes(&arr),
        })
    }

    /// Export the raw 32-byte secret. Treat as sensitive material.
    #[must_use]
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }
}

impl Scheme for Ed25519Scheme {
    fn sign(&self, bytes: &[u8]) -> Result<Signature, SigningError> {
        use ed25519_dalek::Signer as _;
        let sig = self.signing_key.sign(bytes);
        Ok(Signature::new(Self::TAG, &sig.to_bytes()))
    }

    fn verify(
        public_key: &PublicKey,
        bytes: &[u8],
        signature: &Signature,
    ) -> Result<(), SigningError> {
        use ed25519_dalek::Verifier as _;

        if public_key.tag != Self::TAG {
            return Err(SigningError::Encoding(format!(
                "expected ed25519 public key, got {}",
                public_key.tag
            )));
        }
        if signature.tag != Self::TAG {
            return Err(SigningError::Encoding(format!(
                "expected ed25519 signature, got {}",
                signature.tag
            )));
        }
        let pk_bytes = hex_decode(&public_key.bytes_hex)
            .map_err(|e| SigningError::Encoding(format!("public key hex: {e}")))?;
        let pk_arr: [u8; 32] = pk_bytes
            .try_into()
            .map_err(|_| SigningError::Encoding("ed25519 public key must be 32 bytes".into()))?;
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&pk_arr)
            .map_err(|e| SigningError::Encoding(format!("ed25519 pubkey parse: {e}")))?;

        let sig_bytes = hex_decode(&signature.bytes_hex)
            .map_err(|e| SigningError::Encoding(format!("signature hex: {e}")))?;
        let sig_arr: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| SigningError::Encoding("ed25519 signature must be 64 bytes".into()))?;
        let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

        verifying
            .verify(bytes, &sig)
            .map_err(|_| SigningError::VerifyFailed)
    }

    fn public_key(&self) -> PublicKey {
        let vk = self.signing_key.verifying_key();
        PublicKey::new(Self::TAG, &vk.to_bytes())
    }

    fn tag(&self) -> &'static str {
        Self::TAG
    }
}

// ---------------------------------------------------------------------
// BLS12-381 — placeholder stub.
// ---------------------------------------------------------------------

/// BLS12-381 placeholder.
///
/// **Not cryptographically functional.** This type exists so phase-2
/// integration is a targeted swap — replace the method bodies with
/// `blst` (or another audited impl), without reshaping the crate or
/// the persisted schema.
///
/// Any attempt to sign returns a deterministic zero-signature; any
/// attempt to verify returns `SigningError::VerifyFailed`. The
/// [`Scheme::tag`] is `"bls12-381"` so that a mint inadvertently
/// produced by this stub is clearly distinguishable from a real one.
pub struct BlsStub;

impl BlsStub {
    /// Scheme tag (`"bls12-381"`).
    pub const TAG: &'static str = "bls12-381";
}

impl Scheme for BlsStub {
    fn sign(&self, _bytes: &[u8]) -> Result<Signature, SigningError> {
        Err(SigningError::BadKey(
            "BLS12-381 scheme is a phase-2 stub — not available".into(),
        ))
    }

    fn verify(
        _public_key: &PublicKey,
        _bytes: &[u8],
        _signature: &Signature,
    ) -> Result<(), SigningError> {
        Err(SigningError::VerifyFailed)
    }

    fn public_key(&self) -> PublicKey {
        PublicKey::new(Self::TAG, &[])
    }

    fn tag(&self) -> &'static str {
        Self::TAG
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ed25519_sign_verify_roundtrip() {
        let scheme = Ed25519Scheme::generate_from_seed([1u8; 32]);
        let msg = b"mint-protocol-v0";
        let sig = scheme.sign(msg).unwrap();
        Ed25519Scheme::verify(&scheme.public_key(), msg, &sig).unwrap();
    }

    #[test]
    fn ed25519_rejects_tampered_message() {
        let scheme = Ed25519Scheme::generate_from_seed([2u8; 32]);
        let sig = scheme.sign(b"original").unwrap();
        assert!(matches!(
            Ed25519Scheme::verify(&scheme.public_key(), b"tampered", &sig),
            Err(SigningError::VerifyFailed)
        ));
    }

    #[test]
    fn ed25519_rejects_wrong_pubkey() {
        let a = Ed25519Scheme::generate_from_seed([3u8; 32]);
        let b = Ed25519Scheme::generate_from_seed([4u8; 32]);
        let sig = a.sign(b"hello").unwrap();
        assert!(matches!(
            Ed25519Scheme::verify(&b.public_key(), b"hello", &sig),
            Err(SigningError::VerifyFailed)
        ));
    }

    #[test]
    fn ed25519_rejects_non_matching_scheme_tags() {
        let scheme = Ed25519Scheme::generate_from_seed([5u8; 32]);
        let sig = scheme.sign(b"x").unwrap();
        let mut wrong_tag_pk = scheme.public_key();
        wrong_tag_pk.tag = "bls12-381".into();
        assert!(matches!(
            Ed25519Scheme::verify(&wrong_tag_pk, b"x", &sig),
            Err(SigningError::Encoding(_))
        ));
    }

    #[test]
    fn bls_stub_refuses_to_sign() {
        assert!(matches!(BlsStub.sign(b"x"), Err(SigningError::BadKey(_))));
    }

    #[test]
    fn secret_bytes_roundtrip() {
        let seed = [9u8; 32];
        let a = Ed25519Scheme::generate_from_seed(seed);
        let bytes = a.secret_bytes();
        let b = Ed25519Scheme::from_secret_bytes(&bytes).unwrap();
        assert_eq!(a.public_key(), b.public_key());
    }

    #[test]
    fn pubkey_hex_roundtrip() {
        let scheme = Ed25519Scheme::generate_from_seed([7u8; 32]);
        let pk = scheme.public_key();
        let json = serde_json::to_string(&pk).unwrap();
        let back: PublicKey = serde_json::from_str(&json).unwrap();
        assert_eq!(pk, back);
    }
}
