// SPDX-License-Identifier: Apache-2.0

//! Content identity and resolution contract for consumed provider-log segments.
//!
//! A provider path says where bytes happened to be observed. A
//! [`SegmentId`] says which bytes were observed. Keeping those two concerns
//! separate makes rotation and compaction detectable without pretending that
//! a digest can recover bytes the provider deleted.

use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Domain separator for the experimental segment identity scheme.
const SEGMENT_DOMAIN_V1: &[u8] = b"cosmon-session-probe/v1/segment\0";

/// The BLAKE3 content identity of one exact, contiguous consumed byte segment.
///
/// The identifier excludes provider, path, offset and observation time. Those
/// values are routing and provenance metadata; including them would make the
/// identity change when identical bytes move during rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SegmentId([u8; 32]);

impl SegmentId {
    /// Compute the v1 identity of exact provider bytes.
    ///
    /// The byte length is framed explicitly before the bytes so this encoding
    /// remains unambiguous if fields are added to a later domain version.
    #[must_use]
    pub fn digest(bytes: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(SEGMENT_DOMAIN_V1);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
        Self(*hasher.finalize().as_bytes())
    }

    /// Return the raw 32-byte digest for signature and commitment protocols.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for SegmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "blake3:{}",
            blake3::Hash::from_bytes(self.0).to_hex()
        )
    }
}

impl FromStr for SegmentId {
    type Err = SegmentIdParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let hex = input
            .strip_prefix("blake3:")
            .ok_or(SegmentIdParseError::MissingAlgorithm)?;
        let hash = blake3::Hash::from_hex(hex).map_err(|_| SegmentIdParseError::InvalidDigest)?;
        Ok(Self(*hash.as_bytes()))
    }
}

impl Serialize for SegmentId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SegmentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = String::deserialize(deserializer)?;
        input.parse().map_err(de::Error::custom)
    }
}

/// Why a serialized segment identity was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SegmentIdParseError {
    /// The required `blake3:` algorithm tag was absent.
    #[error("segment identity must begin with blake3:")]
    MissingAlgorithm,
    /// The digest was not exactly 32 bytes of hexadecimal BLAKE3 output.
    #[error("segment identity has an invalid BLAKE3 digest")]
    InvalidDigest,
}

/// A persistable, provider-neutral reference to consumed log bytes.
///
/// This value is deliberately not a promise of availability. A resolver can
/// later prove the bytes are present and unchanged, prove different bytes now
/// occupy a candidate location, or report that no candidate is available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentReference {
    /// Content identity of the exact consumed bytes.
    pub id: SegmentId,
    /// Length of the consumed bytes, retained as cheap framing and diagnostics.
    pub byte_length: u64,
}

impl SegmentReference {
    /// Mint a reference from the exact bytes consumed from a provider log.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            id: SegmentId::digest(bytes),
            byte_length: bytes.len() as u64,
        }
    }

    /// Classify candidate bytes without performing I/O.
    ///
    /// `None` means the resolver found no bytes, not that the referenced
    /// segment was empty. Empty segments have an ordinary, verifiable digest.
    #[must_use]
    pub fn verify(self, candidate: Option<&[u8]>) -> SegmentResolution {
        let Some(bytes) = candidate else {
            return SegmentResolution::Missing;
        };
        let observed = Self::from_bytes(bytes);
        if observed == self {
            SegmentResolution::Verified
        } else {
            SegmentResolution::Mismatch { observed }
        }
    }
}

/// What resolving a segment reference established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentResolution {
    /// The referenced bytes were found and reproduce the recorded identity.
    Verified,
    /// No candidate bytes remain available from the configured resolver.
    Missing,
    /// Candidate bytes exist but are not the referenced segment.
    Mismatch {
        /// Reference computed from the bytes that were actually found.
        observed: SegmentReference,
    },
}

/// Injectable retrieval boundary for a future notarization workflow.
///
/// Implementations may search provider paths, an archive, a remote service or
/// any combination. The port chooses no store and requires callers to keep
/// `Missing` distinct from an integrity mismatch.
pub trait SegmentResolver {
    /// Resolver-specific fault, distinct from an honest unavailable result.
    type Error;

    /// Attempt to resolve and verify one reference.
    ///
    /// # Errors
    ///
    /// Returns an implementation-specific error when the retrieval mechanism
    /// itself fails. Ordinary deletion is [`SegmentResolution::Missing`].
    fn resolve(&self, reference: SegmentReference) -> Result<SegmentResolution, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_identity_round_trips_and_verifies() {
        let reference = SegmentReference::from_bytes(b"first\nsecond\n");
        let json = serde_json::to_string(&reference).unwrap();
        let restored: SegmentReference = serde_json::from_str(&json).unwrap();

        assert_eq!(restored, reference);
        assert_eq!(
            restored.verify(Some(b"first\nsecond\n")),
            SegmentResolution::Verified
        );
    }

    #[test]
    fn absence_and_changed_content_are_different_results() {
        let reference = SegmentReference::from_bytes(b"observed\n");

        assert_eq!(reference.verify(None), SegmentResolution::Missing);
        assert!(matches!(
            reference.verify(Some(b"rewritten\n")),
            SegmentResolution::Mismatch { .. }
        ));
    }
}
