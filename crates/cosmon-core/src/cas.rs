// SPDX-License-Identifier: AGPL-3.0-only

//! Content-addressed storage types and trait.
//!
//! Defines the [`ContentHash`] newtype (SHA-256 hex digest) and the [`CasStore`]
//! trait for storing and retrieving binary assets by their content hash.
//! The hash serves as both identity and deduplication key: two identical byte
//! sequences always produce the same hash, so storing the same content twice
//! is a no-op.
//!
//! This module is pure domain logic — zero I/O. Filesystem backends live in
//! `cosmon-filestore`.
//!
//! # Relationship to `OxyMake`
//!
//! `OxyMake` uses BLAKE3 for its build cache. Cosmon uses SHA-256 for binary
//! assets. Both share the **Content-Identity Principle**: the hash of content
//! *is* its address.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CosmonError;

// ---------------------------------------------------------------------------
// ContentHash
// ---------------------------------------------------------------------------

/// SHA-256 content hash as a lowercase hex string (64 characters).
///
/// This is the address in the content-addressed store. Two byte sequences
/// with the same content always yield the same `ContentHash`.
///
/// # Examples
///
/// ```
/// use cosmon_core::cas::ContentHash;
///
/// let hash = ContentHash::new(
///     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
/// ).unwrap();
/// assert_eq!(hash.prefix(), "e3");
/// assert_eq!(hash.as_str().len(), 64);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash(String);

impl ContentHash {
    /// Create a new `ContentHash` from a hex string.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::Runtime`] if the string is not exactly 64
    /// lowercase hexadecimal characters.
    pub fn new(s: impl Into<String>) -> Result<Self, CosmonError> {
        let s = s.into();
        if s.len() != 64 {
            return Err(CosmonError::Runtime {
                reason: format!("content hash must be 64 hex chars, got {} chars", s.len()),
            });
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(CosmonError::Runtime {
                reason: "content hash must be lowercase hex".to_owned(),
            });
        }
        Ok(Self(s))
    }

    /// The raw hex string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The two-character prefix used for directory sharding (`hash[:2]`).
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.0[..2]
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ContentHash {
    type Err = CosmonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl From<ContentHash> for String {
    fn from(h: ContentHash) -> Self {
        h.0
    }
}

impl TryFrom<String> for ContentHash {
    type Error = CosmonError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

// ---------------------------------------------------------------------------
// CasStore trait
// ---------------------------------------------------------------------------

/// Trait for content-addressed binary storage.
///
/// Implementations store opaque byte blobs keyed by their SHA-256 hash.
/// The `hash[:2]/hash` directory layout is an implementation detail of the
/// filesystem backend; this trait is layout-agnostic.
///
/// # Deduplication
///
/// Storing the same content twice must be idempotent: the second `put`
/// returns the same hash without writing new data.
pub trait CasStore {
    /// Store binary content and return its content hash.
    ///
    /// If the content already exists (same hash), this is a no-op and the
    /// existing hash is returned.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on I/O or hashing failures.
    fn put(&self, data: &[u8]) -> Result<ContentHash, CosmonError>;

    /// Retrieve binary content by its hash.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] if the hash is not found or on I/O failure.
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, CosmonError>;

    /// Check whether content with the given hash exists in the store.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError`] on I/O failure.
    fn exists(&self, hash: &ContentHash) -> Result<bool, CosmonError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of empty input.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn test_content_hash_valid() {
        let h = ContentHash::new(EMPTY_SHA256).unwrap();
        assert_eq!(h.as_str(), EMPTY_SHA256);
        assert_eq!(h.prefix(), "e3");
        assert_eq!(h.to_string(), EMPTY_SHA256);
    }

    #[test]
    fn test_content_hash_rejects_wrong_length() {
        let err = ContentHash::new("abcd").unwrap_err();
        assert!(err.to_string().contains("64 hex chars"));
    }

    #[test]
    fn test_content_hash_rejects_uppercase() {
        let upper = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        let err = ContentHash::new(upper).unwrap_err();
        assert!(err.to_string().contains("lowercase hex"));
    }

    #[test]
    fn test_content_hash_rejects_non_hex() {
        let bad = "g3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let err = ContentHash::new(bad).unwrap_err();
        assert!(err.to_string().contains("lowercase hex"));
    }

    #[test]
    fn test_content_hash_from_str() {
        let h: ContentHash = EMPTY_SHA256.parse().unwrap();
        assert_eq!(h.as_str(), EMPTY_SHA256);
    }

    #[test]
    fn test_content_hash_serde_roundtrip() {
        let h = ContentHash::new(EMPTY_SHA256).unwrap();
        let json = serde_json::to_string(&h).unwrap();
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn test_content_hash_prefix_various() {
        let h =
            ContentHash::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        assert_eq!(h.prefix(), "01");
    }
}
