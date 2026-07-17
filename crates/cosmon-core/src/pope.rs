// SPDX-License-Identifier: AGPL-3.0-only

//! Proof of Productive Expenditure (`PoPE`) — verifiable receipts for token consumption.
//!
//! `PoPE` binds a molecule's claimed resource consumption to cryptographic evidence.
//! A downstream verifier — or the External Witness (`P_external`) — can accept or
//! reject the claim.
//!
//! # Attestation levels
//!
//! | Level | Name | Trust model |
//! |-------|------|-------------|
//! | L0 | Self-attested | Worker reports its own consumption |
//! | L1 | Operator-attested | Operator cross-references provider dashboard |
//! | L2 | Provider-signed | Cryptographic signature from API provider |
//!
//! L0 exists today via `EnergyRecord`. L1 is the v0 target: the operator
//! independently verifies claims and signs with HMAC-SHA256. L2 is specified
//! but not implemented (no provider offers signed receipts yet).
//!
//! See ADR-033 for the full design rationale.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::pope::{
//!     ReceiptPayload, AttestationLevel, SigningKey, Receipt,
//! };
//! use cosmon_core::energy::{TokenCount, TokenCost};
//! use cosmon_core::id::{MoleculeId, WorkerId};
//! use chrono::Utc;
//!
//! // Build a receipt payload
//! let payload = ReceiptPayload {
//!     molecule: MoleculeId::new("cs-20260413-ab12").unwrap(),
//!     worker: WorkerId::new("topaz").unwrap(),
//!     model: "claude-opus-4-6".to_owned(),
//!     input_tokens: TokenCount::new(1500),
//!     output_tokens: TokenCount::new(500),
//!     cost: TokenCost::new(0.006),
//!     timestamp: Utc::now(),
//!     evidence_hash: None,
//!     attestation: AttestationLevel::SelfAttested,
//! };
//!
//! // Sign it
//! let key = SigningKey::new(b"operator-secret-key");
//! let receipt = Receipt::sign(&payload, &key);
//!
//! // Verify — authentic receipt passes
//! assert!(receipt.verify(&key).is_ok());
//! ```
//!
//! ```
//! use cosmon_core::pope::{
//!     ReceiptPayload, AttestationLevel, SigningKey, Receipt, VerifyError,
//! };
//! use cosmon_core::energy::{TokenCount, TokenCost};
//! use cosmon_core::id::{MoleculeId, WorkerId};
//! use chrono::Utc;
//!
//! let payload = ReceiptPayload {
//!     molecule: MoleculeId::new("cs-20260413-ab12").unwrap(),
//!     worker: WorkerId::new("topaz").unwrap(),
//!     model: "claude-opus-4-6".to_owned(),
//!     input_tokens: TokenCount::new(1500),
//!     output_tokens: TokenCount::new(500),
//!     cost: TokenCost::new(0.006),
//!     timestamp: Utc::now(),
//!     evidence_hash: None,
//!     attestation: AttestationLevel::SelfAttested,
//! };
//!
//! let key = SigningKey::new(b"operator-secret-key");
//! let mut receipt = Receipt::sign(&payload, &key);
//!
//! // Tamper: inflate the token count
//! receipt.payload.input_tokens = TokenCount::new(150_000);
//!
//! // Verification fails — the signature no longer matches
//! assert!(matches!(receipt.verify(&key), Err(VerifyError::SignatureMismatch)));
//! ```

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cas::ContentHash;
use crate::energy::{TokenCost, TokenCount};
use crate::id::{MoleculeId, WorkerId};

// ---------------------------------------------------------------------------
// AttestationLevel
// ---------------------------------------------------------------------------

/// The trust level of a `PoPE` receipt.
///
/// Ordered from weakest to strongest. Each level subsumes the guarantees of
/// the levels below it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationLevel {
    /// L0: the worker self-reports its own consumption.
    /// Trivially forgeable by the worker, but creates an auditable trail.
    SelfAttested,

    /// L1: the operator (External Witness) independently verifies the worker's
    /// claims against provider evidence (dashboard screenshot, invoice, API
    /// usage export) and signs the receipt.
    ///
    /// This is the answer to the adversary's concern: "what to do when the
    /// provider signature is a screenshot of a usage dashboard." The operator
    /// bridges unstructured provider evidence to a structured, signed receipt.
    OperatorAttested,

    /// L2: the API provider includes a cryptographic signature in the response.
    /// Specified in v0; implementation deferred until providers offer this.
    ProviderSigned,
}

impl fmt::Display for AttestationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelfAttested => f.write_str("L0:self-attested"),
            Self::OperatorAttested => f.write_str("L1:operator-attested"),
            Self::ProviderSigned => f.write_str("L2:provider-signed"),
        }
    }
}

// ---------------------------------------------------------------------------
// ReceiptPayload
// ---------------------------------------------------------------------------

/// The content of a `PoPE` receipt — everything that gets signed.
///
/// Fields are chosen to match [`EnergyRecord`](crate::energy::EnergyRecord)
/// plus the attestation-specific fields (evidence hash, attestation level).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceiptPayload {
    /// The molecule that consumed the resources.
    pub molecule: MoleculeId,
    /// The worker that performed the work.
    pub worker: WorkerId,
    /// The LLM model used (e.g. `"claude-opus-4-6"`).
    pub model: String,
    /// Number of input tokens consumed.
    pub input_tokens: TokenCount,
    /// Number of output tokens produced.
    pub output_tokens: TokenCount,
    /// Monetary cost of this API call.
    pub cost: TokenCost,
    /// When the consumption occurred.
    pub timestamp: DateTime<Utc>,
    /// SHA-256 hash of the evidence artifact (API response JSON, screenshot,
    /// invoice). `None` for L0 self-attested receipts.
    pub evidence_hash: Option<ContentHash>,
    /// The attestation level of this receipt.
    pub attestation: AttestationLevel,
}

impl ReceiptPayload {
    /// Compute the canonical hash of this payload.
    ///
    /// The canonical form is deterministic JSON (sorted keys via `serde_json`'s
    /// default object serialization, which is insertion-order — but since we
    /// control the struct, field order is stable across compilations).
    ///
    /// Returns the SHA-256 hex digest of the canonical JSON bytes.
    ///
    /// # Panics
    ///
    /// Panics if the payload cannot be serialized to JSON (should never happen
    /// since all fields implement `Serialize`).
    #[must_use]
    pub fn canonical_hash(&self) -> String {
        let json = serde_json::to_vec(self).expect("ReceiptPayload is always serializable");
        let digest = Sha256::digest(&json);
        hex_encode(&digest)
    }

    /// Total tokens (input + output) for this receipt.
    #[must_use]
    pub fn total_tokens(&self) -> TokenCount {
        self.input_tokens + self.output_tokens
    }
}

// ---------------------------------------------------------------------------
// SigningKey
// ---------------------------------------------------------------------------

/// An HMAC-SHA256 signing key.
///
/// In v0, this is the operator's secret key used for L0 and L1 attestation.
/// For L2 (provider-signed), this would be derived from the provider's public
/// key infrastructure — deferred to a future version.
#[derive(Clone)]
pub struct SigningKey {
    /// Raw key material, padded/hashed to 64 bytes (SHA-256 block size).
    key: Vec<u8>,
}

impl SigningKey {
    /// Create a new signing key from raw bytes.
    ///
    /// Keys longer than 64 bytes are hashed with SHA-256 first (per HMAC spec,
    /// RFC 2104). Keys shorter than 64 bytes are zero-padded.
    #[must_use]
    pub fn new(raw: &[u8]) -> Self {
        let key = if raw.len() > 64 {
            let hash = Sha256::digest(raw);
            hash.to_vec()
        } else {
            raw.to_vec()
        };
        Self { key }
    }

    /// Compute HMAC-SHA256(key, message).
    ///
    /// Implements RFC 2104:
    /// `HMAC(K, m) = H((K' ⊕ opad) || H((K' ⊕ ipad) || m))`
    /// where K' is the key padded to block size (64 bytes).
    fn hmac_sha256(&self, message: &[u8]) -> Vec<u8> {
        const BLOCK_SIZE: usize = 64;
        const IPAD: u8 = 0x36;
        const OPAD: u8 = 0x5c;

        // Pad key to block size
        let mut padded_key = [0u8; BLOCK_SIZE];
        let key_len = self.key.len().min(BLOCK_SIZE);
        padded_key[..key_len].copy_from_slice(&self.key[..key_len]);

        // Inner hash: H((K' ⊕ ipad) || message)
        let mut inner_hasher = Sha256::new();
        let inner_key: Vec<u8> = padded_key.iter().map(|b| b ^ IPAD).collect();
        inner_hasher.update(&inner_key);
        inner_hasher.update(message);
        let inner_hash = inner_hasher.finalize();

        // Outer hash: H((K' ⊕ opad) || inner_hash)
        let mut outer_hasher = Sha256::new();
        let outer_key: Vec<u8> = padded_key.iter().map(|b| b ^ OPAD).collect();
        outer_hasher.update(&outer_key);
        outer_hasher.update(inner_hash);
        let outer_hash = outer_hasher.finalize();

        outer_hash.to_vec()
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ReceiptSignature
// ---------------------------------------------------------------------------

/// HMAC-SHA256 signature over a receipt's canonical payload hash.
///
/// Stored as a lowercase hex string (64 characters, like [`ContentHash`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReceiptSignature(String);

impl ReceiptSignature {
    /// The raw hex string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReceiptSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Receipt
// ---------------------------------------------------------------------------

/// A signed `PoPE` receipt binding resource consumption to cryptographic evidence.
///
/// The receipt is the atomic unit of expenditure verification. It wraps a
/// [`ReceiptPayload`] (the claim) with a [`ReceiptSignature`] (the proof).
///
/// # Verification
///
/// Call [`Receipt::verify`] with the expected signing key. If any field in
/// the payload has been tampered with since signing, verification fails with
/// [`VerifyError::SignatureMismatch`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    /// The receipt content — what was attested.
    pub payload: ReceiptPayload,
    /// HMAC-SHA256 signature over the canonical payload hash.
    pub signature: ReceiptSignature,
}

impl Receipt {
    /// Sign a receipt payload, producing a complete receipt.
    ///
    /// The signature is HMAC-SHA256 of the payload's canonical hash using the
    /// provided key.
    #[must_use]
    pub fn sign(payload: &ReceiptPayload, key: &SigningKey) -> Self {
        let canonical = payload.canonical_hash();
        let sig_bytes = key.hmac_sha256(canonical.as_bytes());
        let sig_hex = hex_encode(&sig_bytes);
        Self {
            payload: payload.clone(),
            signature: ReceiptSignature(sig_hex),
        }
    }

    /// Verify this receipt against the expected signing key.
    ///
    /// Recomputes the canonical payload hash and HMAC, then compares against
    /// the stored signature. Returns `Ok(())` if the receipt is authentic,
    /// or a [`VerifyError`] describing the failure.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::SignatureMismatch`] if the payload has been
    /// tampered with (any field changed after signing).
    pub fn verify(&self, key: &SigningKey) -> Result<(), VerifyError> {
        let canonical = self.payload.canonical_hash();
        let expected_bytes = key.hmac_sha256(canonical.as_bytes());
        let expected_hex = hex_encode(&expected_bytes);

        if expected_hex != self.signature.0 {
            return Err(VerifyError::SignatureMismatch);
        }

        Ok(())
    }

    /// The attestation level of this receipt.
    #[must_use]
    pub fn attestation_level(&self) -> &AttestationLevel {
        &self.payload.attestation
    }

    /// Content hash of the full receipt (payload + signature) for CAS storage.
    ///
    /// # Panics
    ///
    /// Panics if the receipt cannot be serialized to JSON (should never happen
    /// since all fields implement `Serialize`).
    #[must_use]
    pub fn content_hash(&self) -> String {
        let json = serde_json::to_vec(self).expect("Receipt is always serializable");
        let digest = Sha256::digest(&json);
        hex_encode(&digest)
    }
}

// ---------------------------------------------------------------------------
// VerifyError
// ---------------------------------------------------------------------------

/// Errors returned by [`Receipt::verify`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The recomputed signature does not match the stored signature.
    /// This means one or more fields in the payload were modified after signing.
    #[error("receipt signature mismatch: payload was tampered with")]
    SignatureMismatch,
}

// ---------------------------------------------------------------------------
// ReceiptVerifier trait
// ---------------------------------------------------------------------------

/// Trait for pluggable receipt verification backends.
///
/// The default implementation (`HmacVerifier`) checks HMAC-SHA256 signatures.
/// Future implementations may support:
/// - `EdDSA` signatures (L2 provider-signed)
/// - Certificate Transparency log inclusion proofs
/// - Multi-party threshold signatures
pub trait ReceiptVerifier {
    /// Verify a receipt, returning `Ok(())` if authentic.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError`] if the receipt fails verification.
    fn verify(&self, receipt: &Receipt) -> Result<(), VerifyError>;
}

/// HMAC-SHA256 receipt verifier — the v0 default.
///
/// Wraps a [`SigningKey`] and delegates to [`Receipt::verify`].
pub struct HmacVerifier {
    key: SigningKey,
}

impl HmacVerifier {
    /// Create a new HMAC verifier with the given key.
    #[must_use]
    pub fn new(key: SigningKey) -> Self {
        Self { key }
    }
}

impl ReceiptVerifier for HmacVerifier {
    fn verify(&self, receipt: &Receipt) -> Result<(), VerifyError> {
        receipt.verify(&self.key)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            use fmt::Write;
            write!(s, "{b:02x}").expect("hex formatting cannot fail");
            s
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::energy::{TokenCost, TokenCount};
    use crate::id::{MoleculeId, WorkerId};

    fn sample_payload() -> ReceiptPayload {
        ReceiptPayload {
            molecule: MoleculeId::new("cs-20260413-ab12").unwrap(),
            worker: WorkerId::new("topaz").unwrap(),
            model: "claude-opus-4-6".to_owned(),
            input_tokens: TokenCount::new(1500),
            output_tokens: TokenCount::new(500),
            cost: TokenCost::new(0.006),
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-04-13T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            evidence_hash: None,
            attestation: AttestationLevel::SelfAttested,
        }
    }

    fn operator_key() -> SigningKey {
        SigningKey::new(b"test-operator-secret-key-v0")
    }

    // -- Attestation level --

    #[test]
    fn test_attestation_level_ordering() {
        assert!(AttestationLevel::SelfAttested < AttestationLevel::OperatorAttested);
        assert!(AttestationLevel::OperatorAttested < AttestationLevel::ProviderSigned);
    }

    #[test]
    fn test_attestation_level_display() {
        assert_eq!(
            AttestationLevel::SelfAttested.to_string(),
            "L0:self-attested"
        );
        assert_eq!(
            AttestationLevel::OperatorAttested.to_string(),
            "L1:operator-attested"
        );
        assert_eq!(
            AttestationLevel::ProviderSigned.to_string(),
            "L2:provider-signed"
        );
    }

    #[test]
    fn test_attestation_level_serde_roundtrip() {
        let level = AttestationLevel::OperatorAttested;
        let json = serde_json::to_string(&level).unwrap();
        assert_eq!(json, "\"operator_attested\"");
        let back: AttestationLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(level, back);
    }

    // -- Payload --

    #[test]
    fn test_payload_canonical_hash_deterministic() {
        let p = sample_payload();
        let h1 = p.canonical_hash();
        let h2 = p.canonical_hash();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn test_payload_canonical_hash_changes_on_mutation() {
        let p1 = sample_payload();
        let mut p2 = sample_payload();
        p2.input_tokens = TokenCount::new(9999);
        assert_ne!(p1.canonical_hash(), p2.canonical_hash());
    }

    #[test]
    fn test_payload_total_tokens() {
        let p = sample_payload();
        assert_eq!(p.total_tokens().get(), 2000);
    }

    // -- Signing and verification --

    #[test]
    fn test_sign_and_verify_authentic() {
        let payload = sample_payload();
        let key = operator_key();
        let receipt = Receipt::sign(&payload, &key);
        assert!(receipt.verify(&key).is_ok());
    }

    #[test]
    fn test_tampered_token_count_rejected() {
        let payload = sample_payload();
        let key = operator_key();
        let mut receipt = Receipt::sign(&payload, &key);

        // Tamper: inflate input tokens
        receipt.payload.input_tokens = TokenCount::new(150_000);

        assert_eq!(receipt.verify(&key), Err(VerifyError::SignatureMismatch));
    }

    #[test]
    fn test_tampered_cost_rejected() {
        let payload = sample_payload();
        let key = operator_key();
        let mut receipt = Receipt::sign(&payload, &key);

        // Tamper: inflate cost
        receipt.payload.cost = TokenCost::new(999.99);

        assert_eq!(receipt.verify(&key), Err(VerifyError::SignatureMismatch));
    }

    #[test]
    fn test_tampered_molecule_id_rejected() {
        let payload = sample_payload();
        let key = operator_key();
        let mut receipt = Receipt::sign(&payload, &key);

        // Tamper: change molecule
        receipt.payload.molecule = MoleculeId::new("cs-20260413-xxxx").unwrap();

        assert_eq!(receipt.verify(&key), Err(VerifyError::SignatureMismatch));
    }

    #[test]
    fn test_tampered_model_rejected() {
        let payload = sample_payload();
        let key = operator_key();
        let mut receipt = Receipt::sign(&payload, &key);

        // Tamper: change model to cheaper one
        receipt.payload.model = "claude-haiku-4-5".to_owned();

        assert_eq!(receipt.verify(&key), Err(VerifyError::SignatureMismatch));
    }

    #[test]
    fn test_tampered_attestation_level_rejected() {
        let payload = sample_payload();
        let key = operator_key();
        let mut receipt = Receipt::sign(&payload, &key);

        // Tamper: upgrade attestation without re-signing
        receipt.payload.attestation = AttestationLevel::ProviderSigned;

        assert_eq!(receipt.verify(&key), Err(VerifyError::SignatureMismatch));
    }

    #[test]
    fn test_wrong_key_rejected() {
        let payload = sample_payload();
        let sign_key = operator_key();
        let wrong_key = SigningKey::new(b"wrong-key");

        let receipt = Receipt::sign(&payload, &sign_key);

        assert_eq!(
            receipt.verify(&wrong_key),
            Err(VerifyError::SignatureMismatch)
        );
    }

    #[test]
    fn test_tampered_signature_rejected() {
        let payload = sample_payload();
        let key = operator_key();
        let mut receipt = Receipt::sign(&payload, &key);

        // Tamper: corrupt the signature directly
        receipt.signature = ReceiptSignature("0".repeat(64));

        assert_eq!(receipt.verify(&key), Err(VerifyError::SignatureMismatch));
    }

    // -- Evidence hash --

    #[test]
    fn test_receipt_with_evidence_hash() {
        let mut payload = sample_payload();
        payload.attestation = AttestationLevel::OperatorAttested;
        payload.evidence_hash = Some(
            ContentHash::new("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2")
                .unwrap(),
        );

        let key = operator_key();
        let receipt = Receipt::sign(&payload, &key);
        assert!(receipt.verify(&key).is_ok());

        // Tamper: change evidence hash
        let mut tampered = receipt;
        tampered.payload.evidence_hash = Some(
            ContentHash::new("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
                .unwrap(),
        );
        assert_eq!(tampered.verify(&key), Err(VerifyError::SignatureMismatch));
    }

    // -- Serde roundtrip --

    #[test]
    fn test_receipt_serde_roundtrip() {
        let payload = sample_payload();
        let key = operator_key();
        let receipt = Receipt::sign(&payload, &key);

        let json = serde_json::to_string_pretty(&receipt).unwrap();
        let back: Receipt = serde_json::from_str(&json).unwrap();
        assert_eq!(receipt, back);

        // Deserialized receipt still verifies
        assert!(back.verify(&key).is_ok());
    }

    // -- Content hash --

    #[test]
    fn test_receipt_content_hash_deterministic() {
        let payload = sample_payload();
        let key = operator_key();
        let receipt = Receipt::sign(&payload, &key);

        let h1 = receipt.content_hash();
        let h2 = receipt.content_hash();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    // -- HmacVerifier trait impl --

    #[test]
    fn test_hmac_verifier_trait() {
        let payload = sample_payload();
        let key = operator_key();
        let receipt = Receipt::sign(&payload, &key);

        let verifier = HmacVerifier::new(operator_key());
        assert!(verifier.verify(&receipt).is_ok());
    }

    // -- SigningKey edge cases --

    #[test]
    fn test_signing_key_long_key_hashed() {
        let long_key = vec![0xab; 128]; // > 64 bytes, gets hashed
        let key = SigningKey::new(&long_key);
        let payload = sample_payload();
        let receipt = Receipt::sign(&payload, &key);
        assert!(receipt.verify(&key).is_ok());
    }

    #[test]
    fn test_signing_key_debug_redacted() {
        let key = operator_key();
        let debug = format!("{key:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("test-operator"));
    }

    // -- Display --

    #[test]
    fn test_receipt_signature_display() {
        let payload = sample_payload();
        let key = operator_key();
        let receipt = Receipt::sign(&payload, &key);
        let display = receipt.signature.to_string();
        assert_eq!(display.len(), 64);
        assert!(display.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
