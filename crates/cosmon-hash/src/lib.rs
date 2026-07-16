// SPDX-License-Identifier: AGPL-3.0-only

//! Content-addressed hashing for Cosmon (Plumbing v2 — Month 1).
//!
//! This crate provides a single, narrow capability: turn any
//! `Serialize` value into a stable byte sequence and hash it with
//! [BLAKE3]. It is the foundation for the content-addressed layer:
//! hash chains for the event log, content hashes for terminal molecule
//! snapshots, and eventually signatures and verification receipts.
//!
//! # Why a separate crate
//!
//! Hashing is a **plumbing** concern — it must be auditable, dependency-light,
//! and reusable from `cosmon-filestore`, `cosmon-core`, future
//! `cosmon-sign`, and `cs verify`. Keeping it isolated forbids accidental
//! coupling to domain types and lets us swap algorithms behind the
//! [`Hash`] newtype if we ever need to.
//!
//! # Canonical serialization
//!
//! JSON is *not* canonical by default: object key order, whitespace, and
//! number formatting are unconstrained, so two semantically identical values
//! can hash differently. [`canonical_serialize`] enforces:
//!
//! * Object keys sorted lexicographically (UTF-8 byte order).
//! * No whitespace between tokens.
//! * Numbers re-emitted via `serde_json`'s shortest-round-trip representation.
//!
//! This matches the discipline Torvalds called out: "every hash-chain system
//! learns this the hard way. Skip it and the chain is worthless."
//!
//! # Clock skew
//!
//! Hashes do **not** trust timestamps. The chain order is given by
//! `prev_hash` linkage, not by `timestamp` fields inside events. A clock
//! that jumps backwards cannot fork the chain — only a real edit can.
//!
//! [BLAKE3]: https://github.com/BLAKE3-team/BLAKE3

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::str::FromStr;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

mod canonical;
mod canonical_text;
mod galaxy_seed;
pub mod validation;

pub use canonical::{canonical_serialize, CanonicalError};
pub use canonical_text::{
    canonical_text_bytes, canonical_text_from_bytes, CanonicalTextError, CANONICAL_VERSION_RAW,
    CANONICAL_VERSION_TEXT_V1,
};
pub use galaxy_seed::{
    galaxy_seed, galaxy_seed_raw, nondeterministic_fields, VOLATILE_GENESIS_FIELDS,
};
pub use validation::{
    validator_for, Blake3Validator, InputRef, MTimeValidator, Sha256Validator, StepHash,
    StepValidator, Validation, ValidationError, ValidationMode,
};

/// A 32-byte BLAKE3 digest.
///
/// Displayed and serialized as lowercase hexadecimal (64 chars). The
/// underlying byte array is exposed only via [`Hash::as_bytes`] to keep
/// the type opaque enough that we can revisit the algorithm later
/// without rippling through callers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Construct a hash from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// View the raw 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hash a byte slice with BLAKE3.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Hex string (64 lowercase chars).
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", (*self).to_hex())
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&(*self).to_hex())
    }
}

/// Error parsing a [`Hash`] from a hex string.
#[derive(Debug, thiserror::Error)]
pub enum ParseHashError {
    /// Hex string was not exactly 64 characters.
    #[error("expected 64 hex chars, got {0}")]
    BadLength(usize),
    /// String contained a non-hex character.
    #[error("non-hex character in hash string")]
    NonHex,
}

impl FromStr for Hash {
    type Err = ParseHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(ParseHashError::BadLength(s.len()));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0])?;
            let lo = hex_nibble(chunk[1])?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

fn hex_nibble(b: u8) -> Result<u8, ParseHashError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(ParseHashError::NonHex),
    }
}

impl Serialize for Hash {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&(*self).to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(D::Error::custom)
    }
}

/// Hash any serializable value via canonical JSON.
///
/// Two values that produce the same canonical JSON byte sequence will
/// produce the same hash, regardless of how their fields were ordered
/// in source code or how the JSON happened to be whitespaced on disk.
///
/// # Errors
///
/// Returns [`CanonicalError`] if the value cannot be canonically
/// serialized (which generally only happens when `Serialize` itself
/// fails, e.g. a map with non-string keys).
pub fn hash_value<T: Serialize>(value: &T) -> Result<Hash, CanonicalError> {
    Ok(Hash::of_bytes(&canonical_serialize(value)?))
}

/// Hash an event together with its predecessor's hash.
///
/// The chain rule is intentionally simple: hash the canonical form of
/// the JSON object `{"prev_hash": ..., "event": ...}`. Genesis events
/// (no predecessor) use `prev_hash: null`. This is the same recipe git
/// uses for commit objects — copy it, don't reinvent it.
///
/// `event` is anything that serializes; in practice it is the
/// [`cosmon_core::event::Envelope`] *minus* its own `prev_hash`/`hash`
/// fields (the chain inputs and outputs are kept off the hashed payload
/// to avoid circularity). Callers are responsible for stripping those
/// fields before passing the value in.
///
/// # Errors
///
/// Propagates [`CanonicalError`] from serialization.
pub fn hash_event<T: Serialize>(
    event: &T,
    prev_hash: Option<Hash>,
) -> Result<Hash, CanonicalError> {
    #[derive(Serialize)]
    struct Linked<'a, T: Serialize> {
        prev_hash: Option<Hash>,
        event: &'a T,
    }
    hash_value(&Linked { prev_hash, event })
}

/// Hash the terminal-state snapshot of a molecule.
///
/// Distinct from [`hash_event`] only in intent: a snapshot is the final,
/// frozen view of a molecule used to seal its identity for cross-galaxy
/// import/export (Month 2) and for `cs verify` to compare against the
/// reconstructed chain head. Canonicalization rules are the same.
///
/// # Errors
///
/// Propagates [`CanonicalError`] from serialization.
pub fn hash_molecule_snapshot<T: Serialize>(snapshot: &T) -> Result<Hash, CanonicalError> {
    hash_value(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        let a = Hash::of_bytes(b"hello");
        let b = Hash::of_bytes(b"hello");
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_with_input() {
        assert_ne!(Hash::of_bytes(b"a"), Hash::of_bytes(b"b"));
    }

    #[test]
    fn hex_roundtrip() {
        let h = Hash::of_bytes(b"some bytes");
        let s = h.to_hex();
        assert_eq!(s.len(), 64);
        let back: Hash = s.parse().unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn parse_rejects_bad_length() {
        assert!(matches!(
            "abc".parse::<Hash>(),
            Err(ParseHashError::BadLength(3))
        ));
    }

    #[test]
    fn parse_rejects_non_hex() {
        let bad = "z".repeat(64);
        assert!(matches!(bad.parse::<Hash>(), Err(ParseHashError::NonHex)));
    }

    #[test]
    fn serde_roundtrip() {
        let h = Hash::of_bytes(b"x");
        let j = serde_json::to_string(&h).unwrap();
        assert!(j.starts_with('"') && j.ends_with('"'));
        let back: Hash = serde_json::from_str(&j).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn hash_event_chains() {
        let e1 = serde_json::json!({"kind": "spawn", "who": "quartz"});
        let h1 = hash_event(&e1, None).unwrap();
        let e2 = serde_json::json!({"kind": "step", "n": 1});
        let h2 = hash_event(&e2, Some(h1)).unwrap();
        // Same event with different prev → different hash.
        let h2_alt = hash_event(&e2, None).unwrap();
        assert_ne!(h2, h2_alt);
    }

    #[test]
    fn hash_value_is_order_independent() {
        // Two semantically-equal objects with different field order must
        // produce the same hash. This is the whole point.
        let a: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert_eq!(hash_value(&a).unwrap(), hash_value(&b).unwrap());
    }
}
