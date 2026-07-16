// SPDX-License-Identifier: AGPL-3.0-only

//! The commitment schema — what actually gets signed.
//!
//! A [`Commitment`] is the tuple of facts an operator attests to at
//! nucleation time. The canonical byte encoding is what enters
//! BLAKE3, and the resulting hash is what Ed25519 signs. Two
//! commitments that differ in any field produce distinct signatures;
//! two commitments that agree in every field produce byte-identical
//! canonical forms, regardless of how they were serialized on disk.
//!
//! # Canonical form v1
//!
//! 1. JSON object, keys **sorted** lexicographically (UTF-8 byte order).
//! 2. No whitespace between tokens.
//! 3. Strings escaped exactly as `serde_json` does.
//! 4. Numbers emitted as integers only — **no floats**. Timestamps are
//!    `i64` unix-milliseconds; amounts are integers or decimal strings.
//! 5. Single trailing LF after the closing `}`.
//! 6. Prefixed with the domain separator
//!    [`DOMAIN_SEPARATOR`] (`b"cosmon-notary/v1/commitment\x00"`) before
//!    hashing. The separator is **not** part of the JSON — it is
//!    prepended to the canonical bytes only when computing the
//!    `content_hash` that gets signed. This prevents cross-protocol
//!    collisions (a signature over a commitment cannot be replayed as
//!    a signature over some other JSON document with the same field
//!    layout).
//!
//! # Schema
//!
//! Fields are required unless marked optional. Every required field is
//! populated by `cs notarize` from the molecule's state; optional fields
//! are reserved for phase-2+ features and default to safe values.
//!
//! ```text
//!   molecule_id              MoleculeId            (string)
//!   kind                     MoleculeKind          (enum-as-string)
//!   prompt_content_hash      Hash                  (hex string, 64 chars)
//!   briefing_seals_root      Hash                  (hex string — Merkle root over briefing_seals)
//!   parent_commitments       Vec<Hash>             (hex strings; may be empty)
//!   formula_id               String                (the formula this mol ran)
//!   formula_version_hash     Hash                  (hex string — BLAKE3 of the formula TOML)
//!   cosmon_version           String                (crate version emitting the mint)
//!   operator_pubkey          PublicKey             (hex string, 64 chars for Ed25519)
//!   validator_set_epoch      u64                   (see validator_set_root)
//!   validator_set_root       Hash                  (root of the validator pubkey set in force)
//!   nucleated_at_unix_ms     i64                   (wall-clock at mint)
//!   nonce                    [u8; 32]              (random, hex-encoded in canonical form)
//!   dedup_key                Option<u64>           (optional explicit idempotence key)
//!   canonical_version        u8                    (= 1)
//! ```
//!
//! `briefing_seals_root`, `parent_commitments`, and
//! `validator_set_root` are Merkle-root stubs today: phase 2 turns
//! them into real roots (sorted-leaf BLAKE3 Merkle). Phase 0 computes
//! them as `hash(concat(sorted(leaves)))` — see
//! [`merkle_root_stub`].

use chrono::{DateTime, Utc};
use cosmon_hash::Hash;
use serde::{Deserialize, Serialize};

/// The canonical-form version this crate emits. Bumped whenever the
/// byte layout or the domain separator changes.
pub const CANONICAL_COMMITMENT_VERSION: u8 = 1;

/// Domain separator for mint commitments (canonical-form v1).
///
/// Prepended to the canonical JSON bytes before BLAKE3 hashing. The
/// trailing `\x00` is a literal NUL byte: it cannot appear in valid
/// JSON, so the separator cannot be confused with a JSON payload by
/// any parser in the wild.
pub const DOMAIN_SEPARATOR: &[u8] = b"cosmon-notary/v1/commitment\x00";

/// Errors raised while producing or validating a [`Commitment`].
#[derive(Debug, thiserror::Error)]
pub enum CommitmentError {
    /// Canonical serialization failed (`serde_json` I/O).
    #[error("canonical serialization failed: {0}")]
    Canonical(#[from] cosmon_hash::CanonicalError),
    /// A required field was empty (`molecule_id`, `formula_id`, …).
    #[error("required field is empty: {0}")]
    MissingField(&'static str),
    /// `canonical_version` is not supported by this crate build.
    #[error("unsupported canonical_version {0}; this build supports 1")]
    UnsupportedVersion(u8),
}

/// The tuple of facts an operator commits to at mint time.
///
/// Every field is part of the signed payload. Omitting a field is not
/// allowed: a mint that did not commit to (say) `formula_version_hash`
/// could be replayed against a different formula version after the
/// fact, silently reshaping the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commitment {
    /// The molecule being minted. String form of `cosmon_core::MoleculeId`.
    pub molecule_id: String,
    /// Molecule kind at mint time (idea / task / decision / …).
    pub kind: String,
    /// BLAKE3 hash of the canonical `prompt.md` at nucleation (from
    /// `MoleculeData::prompt_seal.hash`, already hex-encoded).
    pub prompt_content_hash: Hash,
    /// Merkle stub root over all `briefing.md` seals (one per step).
    pub briefing_seals_root: Hash,
    /// Hashes of parent commitments (DAG predecessors). Empty for a
    /// genesis molecule. Phase-2 will make this a real Merkle root.
    pub parent_commitments: Vec<Hash>,
    /// The formula that executed this molecule.
    pub formula_id: String,
    /// BLAKE3 of the canonical formula-TOML bytes. If the formula is
    /// edited after mint, verification catches it.
    pub formula_version_hash: Hash,
    /// Crate version emitting the mint — lets a verifier reject mints
    /// from known-broken cosmon builds.
    pub cosmon_version: String,
    /// Operator's public key (the signer of the outer [`crate::Seal`]).
    ///
    /// Stored as `PublicKey` (hex-encoded bytes). Ed25519 → 32 bytes →
    /// 64 hex chars.
    pub operator_pubkey: super::signature::PublicKey,
    /// Epoch index for the validator set in force. Phase 0 always uses
    /// epoch 0, with `validator_set_root = hash({operator_pubkey})`
    /// (one-element set); phase 3+ increments this when a validator
    /// joins, leaves, or rotates.
    pub validator_set_epoch: u64,
    /// BLAKE3 over the sorted validator-pubkey set. Present in every
    /// commitment so phase-2 validators cannot claim retroactive
    /// authority (the operator cannot be "painted into a corner" —
    /// see ADR-056 §invariants).
    pub validator_set_root: Hash,
    /// Nucleation wall-clock, in unix milliseconds. Signed integer so
    /// timestamps before the epoch round-trip cleanly.
    pub nucleated_at_unix_ms: i64,
    /// 256-bit random nonce chosen at mint time. Prevents two mints of
    /// the same molecule from colliding on a shared signature.
    pub nonce: Nonce,
    /// Optional idempotence key. When populated, two mints with the
    /// same `dedup_key` from the same operator are intended to refer
    /// to the same logical attestation (the verifier can deduplicate).
    /// `None` means "no idempotence claim".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<u64>,
    /// Version of the canonical-form recipe used to produce the
    /// signed bytes. Always `1` in this crate build.
    pub canonical_version: u8,
}

impl Commitment {
    /// Serialize to canonical JSON bytes (without the domain separator).
    ///
    /// # Errors
    ///
    /// Returns [`CommitmentError::Canonical`] if `serde_json` cannot
    /// serialize the value (should not happen for this schema).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CommitmentError> {
        if self.canonical_version != CANONICAL_COMMITMENT_VERSION {
            return Err(CommitmentError::UnsupportedVersion(self.canonical_version));
        }
        if self.molecule_id.is_empty() {
            return Err(CommitmentError::MissingField("molecule_id"));
        }
        if self.formula_id.is_empty() {
            return Err(CommitmentError::MissingField("formula_id"));
        }
        let mut bytes = cosmon_hash::canonical_serialize(self)?;
        // Ensure canonical form ends with exactly one LF. `serde_json`
        // itself does not emit one — we enforce it per ADR-056.
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        Ok(bytes)
    }

    /// Hash the domain-separated canonical bytes and return the digest
    /// that will be signed (or that a verifier recomputes).
    ///
    /// # Errors
    ///
    /// Propagates [`CommitmentError::Canonical`] from
    /// [`Commitment::canonical_bytes`].
    pub fn content_hash(&self) -> Result<Hash, CommitmentError> {
        let canonical = self.canonical_bytes()?;
        let mut buf = Vec::with_capacity(DOMAIN_SEPARATOR.len() + canonical.len());
        buf.extend_from_slice(DOMAIN_SEPARATOR);
        buf.extend_from_slice(&canonical);
        Ok(Hash::of_bytes(&buf))
    }
}

/// 256-bit random nonce, hex-encoded in canonical form.
///
/// Stored as 32 raw bytes internally; serde encodes it as a 64-char
/// lowercase hex string (matching `Hash`). A fresh nonce must be drawn
/// per mint — reusing one would let two mints collide on a shared
/// signature, which is the attack vector `nonce` exists to block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Nonce([u8; 32]);

impl Nonce {
    /// Construct a nonce from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Draw a fresh nonce from the OS's cryptographic RNG.
    ///
    /// Uses `rand_core::OsRng` which is the same source `ed25519-dalek`
    /// uses for key generation. A fallback to a zero nonce is
    /// **forbidden** — if OS randomness is unavailable the mint must
    /// refuse to proceed.
    ///
    /// # Panics
    ///
    /// Panics only if `OsRng` itself panics, which on all supported
    /// platforms means the OS has no entropy source — a condition
    /// under which signing is not safe regardless.
    #[must_use]
    pub fn random() -> Self {
        use rand_core::RngCore;
        let mut bytes = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }
}

impl Serialize for Nonce {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
        }
        ser.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for Nonce {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let s = String::deserialize(de)?;
        if s.len() != 64 {
            return Err(D::Error::custom(format!(
                "nonce must be 64 hex chars, got {}",
                s.len()
            )));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0]).map_err(D::Error::custom)?;
            let lo = hex_nibble(chunk[1]).map_err(D::Error::custom)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

fn hex_nibble(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("non-hex character in nonce"),
    }
}

/// Phase-0 Merkle root stub.
///
/// Concatenates `leaves` in the order provided and returns
/// `BLAKE3(concat)`. Empty input returns `BLAKE3(DOMAIN_SEPARATOR ||
/// "empty")` so that two distinct empty-set positions cannot collide.
///
/// Phase 2 replaces this with a real sorted-leaf binary Merkle tree.
/// Until then, callers should pre-sort leaves for determinism (the
/// stub does not sort — that decision belongs to the caller's domain,
/// e.g. `validator_set_root` sorts pubkeys, `parent_commitments` keeps
/// DAG-edge order).
#[must_use]
pub fn merkle_root_stub(leaves: &[Hash]) -> Hash {
    if leaves.is_empty() {
        let mut buf = Vec::with_capacity(DOMAIN_SEPARATOR.len() + 5);
        buf.extend_from_slice(DOMAIN_SEPARATOR);
        buf.extend_from_slice(b"empty");
        return Hash::of_bytes(&buf);
    }
    let mut buf = Vec::with_capacity(32 * leaves.len());
    for leaf in leaves {
        buf.extend_from_slice(leaf.as_bytes());
    }
    Hash::of_bytes(&buf)
}

/// Convert a `DateTime<Utc>` to unix milliseconds.
#[must_use]
pub fn unix_ms(dt: DateTime<Utc>) -> i64 {
    dt.timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::{Ed25519Scheme, Scheme};

    fn fixture_commitment() -> Commitment {
        let scheme = Ed25519Scheme::generate_from_seed([7u8; 32]);
        let pk = scheme.public_key();
        let pk_hash = Hash::of_bytes(&pk.to_bytes());
        Commitment {
            molecule_id: "task-20260420-1d61".into(),
            kind: "task".into(),
            prompt_content_hash: Hash::of_bytes(b"prompt"),
            briefing_seals_root: merkle_root_stub(&[Hash::of_bytes(b"step0")]),
            parent_commitments: vec![],
            formula_id: "task-work".into(),
            formula_version_hash: Hash::of_bytes(b"formula-toml"),
            cosmon_version: env!("CARGO_PKG_VERSION").into(),
            operator_pubkey: pk,
            validator_set_epoch: 0,
            validator_set_root: merkle_root_stub(&[pk_hash]),
            nucleated_at_unix_ms: 1_714_000_000_000,
            nonce: Nonce::from_bytes([3u8; 32]),
            dedup_key: None,
            canonical_version: 1,
        }
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        let c = fixture_commitment();
        let a = c.canonical_bytes().unwrap();
        let b = c.canonical_bytes().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_bytes_end_with_single_lf() {
        let c = fixture_commitment();
        let bytes = c.canonical_bytes().unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_ne!(bytes.len(), 0);
        // Exactly one trailing LF, not two.
        assert_ne!(
            bytes.get(bytes.len().saturating_sub(2)),
            Some(&b'\n'),
            "canonical form must end with exactly one LF"
        );
    }

    #[test]
    fn content_hash_changes_with_nonce() {
        let mut c = fixture_commitment();
        let h1 = c.content_hash().unwrap();
        c.nonce = Nonce::from_bytes([4u8; 32]);
        let h2 = c.content_hash().unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn content_hash_includes_domain_separator() {
        // Hashing the naked canonical bytes must differ from
        // content_hash (which prepends the domain separator).
        let c = fixture_commitment();
        let canonical = c.canonical_bytes().unwrap();
        let naked = Hash::of_bytes(&canonical);
        let domain_sep = c.content_hash().unwrap();
        assert_ne!(naked, domain_sep);
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut c = fixture_commitment();
        c.canonical_version = 99;
        assert!(matches!(
            c.canonical_bytes(),
            Err(CommitmentError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn rejects_empty_molecule_id() {
        let mut c = fixture_commitment();
        c.molecule_id.clear();
        assert!(matches!(
            c.canonical_bytes(),
            Err(CommitmentError::MissingField("molecule_id"))
        ));
    }

    #[test]
    fn nonce_serde_roundtrip() {
        let n = Nonce::from_bytes([0xab; 32]);
        let j = serde_json::to_string(&n).unwrap();
        assert_eq!(j.len(), 66); // 64 hex + 2 quotes
        let back: Nonce = serde_json::from_str(&j).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn merkle_stub_is_empty_safe() {
        let empty = merkle_root_stub(&[]);
        let singleton = merkle_root_stub(&[Hash::of_bytes(b"x")]);
        assert_ne!(empty, singleton);
    }

    #[test]
    fn different_leaf_order_produces_different_roots() {
        // The stub is order-sensitive by design — callers must
        // pre-sort if they want set semantics.
        let a = merkle_root_stub(&[Hash::of_bytes(b"a"), Hash::of_bytes(b"b")]);
        let b = merkle_root_stub(&[Hash::of_bytes(b"b"), Hash::of_bytes(b"a")]);
        assert_ne!(a, b);
    }
}
