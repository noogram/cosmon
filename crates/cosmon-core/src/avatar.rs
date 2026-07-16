// SPDX-License-Identifier: AGPL-3.0-only

//! Domain types for the moule→avatar incarnation lifecycle.
//!
//! An instance starts as a **moule** (free, pre-binding) and becomes an
//! **avatar** (bound to one pilote in one juridiction) through a signed
//! `IncarnationAt` event — line 1 of the instance's `events.jsonl`.
//!
//! Spec: `docs/specs/avatar-incarnation-event-v1.md` (smithy d958).
//! ADR:  `docs/adr/0020-d-avatar-asymmetry-substrat-and-pilot-binding.md`.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::auth::TenantId;

// ---------------------------------------------------------------------------
// Newtypes
// ---------------------------------------------------------------------------

/// Hash of the moule at incarnation time — `blake3:<64-hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MouleSha(String);

/// Error returned when a `MouleSha` string fails validation.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid moule sha: {0}")]
pub struct MouleShaError(String);

impl MouleSha {
    /// Parse a `blake3:<64-hex>` string.
    ///
    /// # Errors
    /// Returns [`MouleShaError`] if the format is invalid.
    pub fn new(raw: impl Into<String>) -> Result<Self, MouleShaError> {
        let raw = raw.into();
        let Some(hex) = raw.strip_prefix("blake3:") else {
            return Err(MouleShaError(format!("missing blake3: prefix in {raw}")));
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(MouleShaError(format!(
                "expected 64 lowercase hex chars after blake3:, got {hex}"
            )));
        }
        Ok(Self(raw))
    }

    /// Inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MouleSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for MouleSha {
    type Error = MouleShaError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<MouleSha> for String {
    fn from(m: MouleSha) -> Self {
        m.0
    }
}

/// ISO 3166-1 alpha-2 jurisdiction code (e.g. "FR", "US").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JurisdictionCode(String);

/// Error returned when a `JurisdictionCode` string fails validation.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid jurisdiction code: {0}")]
pub struct JurisdictionCodeError(String);

impl JurisdictionCode {
    /// Parse an ISO 3166-1 alpha-2 code (exactly 2 uppercase ASCII letters).
    ///
    /// # Errors
    /// Returns [`JurisdictionCodeError`] if the format is invalid.
    pub fn new(raw: impl Into<String>) -> Result<Self, JurisdictionCodeError> {
        let raw = raw.into();
        if raw.len() != 2 || !raw.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(JurisdictionCodeError(format!(
                "expected 2 uppercase ASCII letters, got {raw}"
            )));
        }
        Ok(Self(raw))
    }

    /// Inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JurisdictionCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for JurisdictionCode {
    type Error = JurisdictionCodeError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<JurisdictionCode> for String {
    fn from(j: JurisdictionCode) -> Self {
        j.0
    }
}

/// Cryptographic identifier of the pilote — a DID string (e.g. `did:key:z6Mk...`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PiloteId(String);

/// Error returned when a `PiloteId` string fails validation.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid pilote id: {0}")]
pub struct PiloteIdError(String);

impl PiloteId {
    /// Parse a DID string (must start with `did:`).
    ///
    /// # Errors
    /// Returns [`PiloteIdError`] if the format is invalid.
    pub fn new(raw: impl Into<String>) -> Result<Self, PiloteIdError> {
        let raw = raw.into();
        if !raw.starts_with("did:") || raw.len() < 8 {
            return Err(PiloteIdError(format!(
                "expected DID format (did:<method>:<id>), got {raw}"
            )));
        }
        Ok(Self(raw))
    }

    /// Inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PiloteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for PiloteId {
    type Error = PiloteIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<PiloteId> for String {
    fn from(p: PiloteId) -> Self {
        p.0
    }
}

/// Instance identifier — `[a-z0-9][a-z0-9-]{0,127}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstanceId(String);

/// Error returned when an `InstanceId` string fails validation.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid instance id: {0}")]
pub struct InstanceIdError(String);

impl InstanceId {
    /// Parse an instance identifier.
    ///
    /// # Errors
    /// Returns [`InstanceIdError`] if the format is invalid.
    pub fn new(raw: impl Into<String>) -> Result<Self, InstanceIdError> {
        let raw = raw.into();
        if raw.is_empty() || raw.len() > 128 {
            return Err(InstanceIdError(format!(
                "length must be 1..=128, got {}",
                raw.len()
            )));
        }
        let first = raw.as_bytes()[0];
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(InstanceIdError(format!(
                "must start with [a-z0-9], got {raw}"
            )));
        }
        if !raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(InstanceIdError(format!("must match [a-z0-9-], got {raw}")));
        }
        Ok(Self(raw))
    }

    /// Inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for InstanceId {
    type Error = InstanceIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<InstanceId> for String {
    fn from(i: InstanceId) -> Self {
        i.0
    }
}

impl FromStr for InstanceId {
    type Err = InstanceIdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------

/// Signature algorithm — v1 supports ed25519 only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SignatureAlgo {
    /// Edwards-curve Digital Signature Algorithm (RFC 8032).
    Ed25519,
}

/// Pilote signature over the incarnation event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    /// Signature algorithm.
    pub algo: SignatureAlgo,
    /// Base64-encoded signature bytes.
    pub sig_b64: String,
    /// DID key identifier (e.g. `did:key:z6Mk...#key-1`).
    pub key_id: String,
}

// ---------------------------------------------------------------------------
// IncarnationAt — the event
// ---------------------------------------------------------------------------

/// Signed, non-reversible event that transforms a moule into an avatar.
///
/// Always line 1 of an incarnated instance's `events.jsonl`. A second
/// occurrence is a writer bug — detected at fold, rejected at
/// `validate_pre_append`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncarnationAt {
    /// Timestamp of the incarnation (UTC).
    pub ts: DateTime<Utc>,
    /// Hash of the moule at incarnation time.
    pub moule_sha: MouleSha,
    /// Tenant that owns this avatar.
    pub tenant_id: TenantId,
    /// Jurisdiction of the incarnated avatar (ISO 3166-1 α-2).
    pub juridiction: JurisdictionCode,
    /// Cryptographic identity of the pilote-de-naissance.
    pub pilote_id: PiloteId,
    /// Instance hosting this avatar.
    pub instance_id: InstanceId,
    /// Pilote's signature over the canonical event hash.
    pub signature_pilote: Signature,
}

/// Instance-level event for the avatar lifecycle.
///
/// Discriminated by `"type"` (distinct from the fleet-level `Event`
/// which uses `"kind"`). The instance ledger is a separate log from the
/// fleet-wide `events.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstanceEvent {
    /// Moule → avatar bascule (set-once, line 1).
    IncarnationAt(IncarnationAt),
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors related to instance-level event validation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum IncarnationError {
    /// An `IncarnationAt` event already exists in this ledger.
    #[error("incarnation_already_present: this instance is already incarnated")]
    AlreadyPresent,
    /// An `IncarnationAt` was proposed after other events in the ledger.
    #[error("incarnation_after_other: cannot incarnate after non-incarnation events")]
    AfterOther,
    /// A non-incarnation event was proposed on an empty ledger that
    /// requires incarnation first.
    #[error("pre_incarnation_event_rejected: incarnation must be the first event")]
    PreIncarnationRejected,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_incarnation() -> IncarnationAt {
        IncarnationAt {
            ts: Utc::now(),
            moule_sha: MouleSha::new(
                "blake3:7f4a3c2d1e0b9a8f7e6d5c4b3a291807f6e5d4c3b2a19087f6e5d4c3b2a19087",
            )
            .unwrap(),
            tenant_id: TenantId::new("democorp-internal").unwrap(),
            juridiction: JurisdictionCode::new("FR").unwrap(),
            pilote_id: PiloteId::new("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK")
                .unwrap(),
            instance_id: InstanceId::new("smithy-you-001").unwrap(),
            signature_pilote: Signature {
                algo: SignatureAlgo::Ed25519,
                sig_b64: "MEUCIQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
                key_id: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK#key-1".to_owned(),
            },
        }
    }

    #[test]
    fn incarnation_at_serde_roundtrip() {
        let e = sample_incarnation();
        let json = serde_json::to_string(&e).unwrap();
        let parsed: IncarnationAt = serde_json::from_str(&json).unwrap();
        assert_eq!(e, parsed);
    }

    #[test]
    fn instance_event_tagged_roundtrip() {
        let evt = InstanceEvent::IncarnationAt(sample_incarnation());
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains("\"type\":\"incarnation_at\""));
        let parsed: InstanceEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(evt, parsed);
    }

    #[test]
    fn incarnation_at_rejects_unknown_field() {
        let json = r#"{
            "ts": "2026-05-24T12:00:00Z",
            "moule_sha": "blake3:7f4a3c2d1e0b9a8f7e6d5c4b3a291807f6e5d4c3b2a19087f6e5d4c3b2a19087",
            "tenant_id": "democorp-internal",
            "juridiction": "FR",
            "pilote_id": "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
            "instance_id": "smithy-you-001",
            "signature_pilote": {"algo": "ed25519", "sig_b64": "AAAA", "key_id": "k1"},
            "unknown_field": "surprise"
        }"#;
        assert!(serde_json::from_str::<IncarnationAt>(json).is_err());
    }

    #[test]
    fn incarnation_at_canonical_bytes_are_stable() {
        let e = sample_incarnation();
        let a = cosmon_hash::canonical_serialize(&e).unwrap();
        let b = cosmon_hash::canonical_serialize(&e).unwrap();
        assert_eq!(a, b, "canonical bytes must be deterministic");
    }

    #[test]
    fn cicatrice_from_canonical_bytes() {
        let e = sample_incarnation();
        let canonical = cosmon_hash::canonical_serialize(&e).unwrap();
        let hash = cosmon_hash::Hash::of_bytes(&canonical);
        assert_eq!(hash.to_hex().len(), 64);
    }

    // -- newtype validation --

    #[test]
    fn moule_sha_valid() {
        assert!(MouleSha::new(
            "blake3:7f4a3c2d1e0b9a8f7e6d5c4b3a291807f6e5d4c3b2a19087f6e5d4c3b2a19087"
        )
        .is_ok());
    }

    #[test]
    fn moule_sha_rejects_missing_prefix() {
        assert!(
            MouleSha::new("7f4a3c2d1e0b9a8f7e6d5c4b3a291807f6e5d4c3b2a19087f6e5d4c3b2a19087")
                .is_err()
        );
    }

    #[test]
    fn moule_sha_rejects_uppercase() {
        assert!(MouleSha::new(
            "blake3:7F4A3C2D1E0B9A8F7E6D5C4B3A291807F6E5D4C3B2A19087F6E5D4C3B2A19087"
        )
        .is_err());
    }

    #[test]
    fn moule_sha_rejects_wrong_length() {
        assert!(MouleSha::new("blake3:7f4a3c").is_err());
    }

    #[test]
    fn jurisdiction_code_valid() {
        assert!(JurisdictionCode::new("FR").is_ok());
        assert!(JurisdictionCode::new("US").is_ok());
    }

    #[test]
    fn jurisdiction_code_rejects_lowercase() {
        assert!(JurisdictionCode::new("fr").is_err());
    }

    #[test]
    fn jurisdiction_code_rejects_wrong_length() {
        assert!(JurisdictionCode::new("F").is_err());
        assert!(JurisdictionCode::new("FRA").is_err());
    }

    #[test]
    fn pilote_id_valid() {
        assert!(PiloteId::new("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK").is_ok());
    }

    #[test]
    fn pilote_id_rejects_no_did_prefix() {
        assert!(PiloteId::new("key:z6Mk").is_err());
    }

    #[test]
    fn instance_id_valid() {
        assert!(InstanceId::new("smithy-you-001").is_ok());
        assert!(InstanceId::new("a").is_ok());
    }

    #[test]
    fn instance_id_rejects_empty() {
        assert!(InstanceId::new("").is_err());
    }

    #[test]
    fn instance_id_rejects_uppercase() {
        assert!(InstanceId::new("Smithy").is_err());
    }

    #[test]
    fn instance_id_rejects_leading_hyphen() {
        assert!(InstanceId::new("-foo").is_err());
    }

    #[test]
    fn signature_algo_serde() {
        let json = serde_json::to_string(&SignatureAlgo::Ed25519).unwrap();
        assert_eq!(json, "\"ed25519\"");
        let back: SignatureAlgo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SignatureAlgo::Ed25519);
    }
}
