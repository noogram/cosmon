// SPDX-License-Identifier: AGPL-3.0-only

//! Step validation modes — content-addressed identity for formula steps.
//!
//! See ADR-043 (step-hash-validation-modes) for the full rationale.
//!
//! Three concrete validators cover the pragmatic cases:
//!
//! * [`ValidationMode::MTime`] — fast path: compare input mtimes, no hashing.
//!   Cheap, non-cryptographic, appropriate for dev/iterative loops.
//! * [`ValidationMode::Blake3`] — cryptographic default. Reuses the
//!   crate-level [`Hash`](struct@crate::Hash) (BLAKE3-256).
//! * [`ValidationMode::Sha256`] — SHA-256 for interoperability with SLSA,
//!   Sigstore, git object names, and external systems.
//!
//! A fourth variant, [`ValidationMode::KeyedBlake3`], is a placeholder for
//! future keyed-MAC signatures (e.g. an sshsig 2-of-3 threshold scheme).
//! Implementations live behind a separate feature flag in a follow-up PR.
//!
//! # Zero-I/O core
//!
//! Validators operate on an abstract [`InputRef`] — a name plus opaque
//! bytes and an optional mtime. Resolution from disk (reading files,
//! statting timestamps) is the caller's responsibility; this keeps the
//! crate pure and easy to test.

use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::{canonical_serialize, CanonicalError, Hash};

/// Validation mode for a formula step.
///
/// Controls how the step's inputs are hashed/compared for memoization,
/// drift detection, and `cs verify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationMode {
    /// Compare mtimes only — non-cryptographic, fast, development default.
    MTime,
    /// BLAKE3-256 — cryptographic, fast, the release default.
    Blake3,
    /// SHA-256 — for interoperability with external systems (SLSA, git).
    Sha256,
    /// Keyed BLAKE3 — placeholder for project-scoped MAC signatures.
    ///
    /// Not yet wired; selecting this mode currently errors at validate
    /// time. Reserved so the enum is forward-compatible.
    KeyedBlake3,
}

impl ValidationMode {
    /// Parse from a lowercase string: `"mtime" | "blake3" | "sha256" | "keyed_blake3"`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::UnknownMode`] if the string is not one
    /// of the recognised variants.
    pub fn parse(s: &str) -> Result<Self, ValidationError> {
        match s {
            "mtime" => Ok(Self::MTime),
            "blake3" => Ok(Self::Blake3),
            "sha256" => Ok(Self::Sha256),
            "keyed_blake3" => Ok(Self::KeyedBlake3),
            other => Err(ValidationError::UnknownMode(other.to_owned())),
        }
    }

    /// Short lowercase name, suitable for TOML/JSON round-trips.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MTime => "mtime",
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
            Self::KeyedBlake3 => "keyed_blake3",
        }
    }
}

impl Default for ValidationMode {
    /// Default is [`ValidationMode::MTime`] — the "dev is fast" choice.
    ///
    /// Release/CI pipelines should opt into `Blake3` explicitly.
    fn default() -> Self {
        Self::MTime
    }
}

/// A single input to a step — the thing the validator observes.
///
/// `bytes` is optional because mtime-only validation doesn't read content.
/// Callers that use [`ValidationMode::Blake3`] or [`ValidationMode::Sha256`]
/// MUST populate `bytes`; validators error otherwise.
#[derive(Debug, Clone)]
pub struct InputRef<'a> {
    /// Stable identifier (usually a file path or logical name). Participates
    /// in the hash so reorderings of the same content are still detected.
    pub name: &'a str,
    /// Raw input bytes (file contents, serialized value, …).
    pub bytes: Option<&'a [u8]>,
    /// Last-modification time in seconds-since-epoch, if available.
    pub mtime: Option<u64>,
}

impl<'a> InputRef<'a> {
    /// Construct an `InputRef` with bytes and no mtime.
    #[must_use]
    pub const fn new(name: &'a str, bytes: &'a [u8]) -> Self {
        Self {
            name,
            bytes: Some(bytes),
            mtime: None,
        }
    }

    /// Construct an mtime-only `InputRef` (no bytes).
    #[must_use]
    pub const fn mtime(name: &'a str, mtime: u64) -> Self {
        Self {
            name,
            bytes: None,
            mtime: Some(mtime),
        }
    }
}

/// The content-addressed identity of a completed step.
///
/// Stored on `EventV2::MoleculeStepCompleted` (as `Option`) and in
/// per-step git commit subjects. `None` means the step was validated in
/// a mode that produced no hash (e.g. [`ValidationMode::MTime`]) or by a
/// runtime that pre-dates the hash field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepHash {
    /// Algorithm the digest was produced with.
    pub mode: ValidationMode,
    /// The 32-byte digest (BLAKE3 or SHA-256 — both are 32 bytes).
    pub digest: Hash,
}

impl StepHash {
    /// Lowercase hex of the digest.
    #[must_use]
    pub fn hex(&self) -> String {
        self.digest.to_hex()
    }

    /// Truncated 8-byte (16-char) prefix, suitable for commit subjects.
    #[must_use]
    pub fn short(&self) -> String {
        self.digest.to_hex()[..16].to_owned()
    }
}

/// Errors returned by the validators.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    /// Inputs declared no bytes, but the requested mode needs them.
    #[error("{mode:?} requires input bytes for `{input}`, but none were supplied")]
    MissingBytes {
        /// The mode that was requested.
        mode: ValidationMode,
        /// The offending input name.
        input: String,
    },
    /// Inputs declared no mtime, but the requested mode needs one.
    #[error("mtime-mode requires mtime for input `{0}`, but none was supplied")]
    MissingMtime(String),
    /// Mode parsed from configuration was not recognised.
    #[error("unknown validation mode: `{0}`")]
    UnknownMode(String),
    /// Keyed-BLAKE3 is declared but the runtime has no key wired yet.
    #[error("keyed_blake3 is not yet implemented (reserved — see ADR-043)")]
    NotImplemented,
    /// Canonical serialization failed while computing the hash.
    #[error("canonical serialization failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// The single entry point for validating a step's inputs.
///
/// Implementations must be pure: given the same `(inputs, previous)` pair
/// they must produce the same output. No clocks, no randomness, no I/O.
pub trait StepValidator {
    /// The mode this validator implements.
    fn mode(&self) -> ValidationMode;

    /// Validate a step's inputs against an optional previous hash.
    ///
    /// * If `previous` is `Some` and the fresh hash matches, returns
    ///   [`Validation::Unchanged`] — the caller MAY skip re-executing the
    ///   step (memoization).
    /// * If `previous` is `None` or the hash differs, returns
    ///   [`Validation::Fresh { hash }`]; the caller MUST re-execute.
    /// * For [`ValidationMode::MTime`], `hash` is `None` — no cryptographic
    ///   digest was computed.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] if the inputs don't match the mode's
    /// requirements.
    fn validate(
        &self,
        inputs: &[InputRef<'_>],
        previous: Option<&StepHash>,
    ) -> Result<Validation, ValidationError>;
}

/// Outcome of a [`StepValidator::validate`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validation {
    /// Inputs match the previous hash — the step may be skipped.
    Unchanged {
        /// The hash that was compared. `None` for `MTime` mode.
        hash: Option<StepHash>,
    },
    /// Inputs differ or no previous hash was given — the step must run.
    Fresh {
        /// The freshly computed hash. `None` for `MTime` mode.
        hash: Option<StepHash>,
    },
}

// ---------------------------------------------------------------------------
// MTime
// ---------------------------------------------------------------------------

/// Validator that compares input mtimes — the fast, non-cryptographic path.
///
/// Produces no digest. A previous [`StepHash`] is ignored; `Unchanged` vs
/// `Fresh` is decided by the caller tracking the *latest* mtime across
/// invocations.  For a pure pipeline the natural convention is: the
/// caller stores `max(mtime)` externally and re-calls `validate` only
/// when it changes.
#[derive(Debug, Default, Clone, Copy)]
pub struct MTimeValidator;

impl StepValidator for MTimeValidator {
    fn mode(&self) -> ValidationMode {
        ValidationMode::MTime
    }

    fn validate(
        &self,
        inputs: &[InputRef<'_>],
        _previous: Option<&StepHash>,
    ) -> Result<Validation, ValidationError> {
        for i in inputs {
            if i.mtime.is_none() {
                return Err(ValidationError::MissingMtime(i.name.to_owned()));
            }
        }
        Ok(Validation::Fresh { hash: None })
    }
}

// ---------------------------------------------------------------------------
// Blake3
// ---------------------------------------------------------------------------

/// BLAKE3-256 validator. Default cryptographic mode for release pipelines.
#[derive(Debug, Default, Clone, Copy)]
pub struct Blake3Validator;

impl StepValidator for Blake3Validator {
    fn mode(&self) -> ValidationMode {
        ValidationMode::Blake3
    }

    fn validate(
        &self,
        inputs: &[InputRef<'_>],
        previous: Option<&StepHash>,
    ) -> Result<Validation, ValidationError> {
        let canon = canonical_inputs(inputs, ValidationMode::Blake3)?;
        let digest = Hash::of_bytes(&canon);
        let fresh = StepHash {
            mode: ValidationMode::Blake3,
            digest,
        };
        Ok(match previous {
            Some(p) if *p == fresh => Validation::Unchanged { hash: Some(fresh) },
            _ => Validation::Fresh { hash: Some(fresh) },
        })
    }
}

// ---------------------------------------------------------------------------
// Sha256
// ---------------------------------------------------------------------------

/// SHA-256 validator. Use for SLSA/Sigstore/git-object interoperability.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sha256Validator;

impl StepValidator for Sha256Validator {
    fn mode(&self) -> ValidationMode {
        ValidationMode::Sha256
    }

    fn validate(
        &self,
        inputs: &[InputRef<'_>],
        previous: Option<&StepHash>,
    ) -> Result<Validation, ValidationError> {
        let canon = canonical_inputs(inputs, ValidationMode::Sha256)?;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&canon);
        let arr: [u8; 32] = hasher.finalize().into();
        let digest = Hash::from_bytes(arr);
        let fresh = StepHash {
            mode: ValidationMode::Sha256,
            digest,
        };
        Ok(match previous {
            Some(p) if *p == fresh => Validation::Unchanged { hash: Some(fresh) },
            _ => Validation::Fresh { hash: Some(fresh) },
        })
    }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Build a boxed [`StepValidator`] for the requested mode.
///
/// Convenience for formula consumers that carry the mode as a domain value.
///
/// # Errors
///
/// Returns [`ValidationError::NotImplemented`] for
/// [`ValidationMode::KeyedBlake3`].
pub fn validator_for(mode: ValidationMode) -> Result<Box<dyn StepValidator>, ValidationError> {
    match mode {
        ValidationMode::MTime => Ok(Box::new(MTimeValidator)),
        ValidationMode::Blake3 => Ok(Box::new(Blake3Validator)),
        ValidationMode::Sha256 => Ok(Box::new(Sha256Validator)),
        ValidationMode::KeyedBlake3 => Err(ValidationError::NotImplemented),
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Build the canonical byte sequence hashed by Blake3/Sha256 validators.
///
/// Inputs are sorted by `name` (lexicographic bytes) so that call-site
/// order does not affect the digest. This is the "input-order invariance"
/// the proptest guards against.
fn canonical_inputs(
    inputs: &[InputRef<'_>],
    mode: ValidationMode,
) -> Result<Vec<u8>, ValidationError> {
    #[derive(Serialize)]
    struct CanonEntry<'a> {
        name: &'a str,
        bytes_b3: String, // hex BLAKE3 of the bytes (keeps the envelope small)
    }

    let mut entries: Vec<CanonEntry<'_>> = inputs
        .iter()
        .map(|i| {
            let bytes = i.bytes.ok_or_else(|| ValidationError::MissingBytes {
                mode,
                input: i.name.to_owned(),
            })?;
            Ok(CanonEntry {
                name: i.name,
                bytes_b3: Hash::of_bytes(bytes).to_hex(),
            })
        })
        .collect::<Result<_, ValidationError>>()?;
    entries.sort_by(|a, b| a.name.cmp(b.name));

    Ok(canonical_serialize(&entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_roundtrip() {
        for m in [
            ValidationMode::MTime,
            ValidationMode::Blake3,
            ValidationMode::Sha256,
            ValidationMode::KeyedBlake3,
        ] {
            assert_eq!(ValidationMode::parse(m.as_str()).unwrap(), m);
        }
        assert!(ValidationMode::parse("md5").is_err());
    }

    #[test]
    fn default_mode_is_mtime() {
        assert_eq!(ValidationMode::default(), ValidationMode::MTime);
    }

    #[test]
    fn step_hash_serde_roundtrip() {
        let h = StepHash {
            mode: ValidationMode::Blake3,
            digest: Hash::of_bytes(b"x"),
        };
        let j = serde_json::to_string(&h).unwrap();
        let back: StepHash = serde_json::from_str(&j).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn blake3_detects_unchanged_and_fresh() {
        let v = Blake3Validator;
        let inputs = [InputRef::new("a", b"hello"), InputRef::new("b", b"world")];
        let Validation::Fresh { hash: Some(h1) } = v.validate(&inputs, None).unwrap() else {
            panic!("expected Fresh with hash");
        };
        match v.validate(&inputs, Some(&h1)).unwrap() {
            Validation::Unchanged { hash: Some(h2) } => assert_eq!(h1, h2),
            other => panic!("expected Unchanged, got {other:?}"),
        }
        let inputs2 = [InputRef::new("a", b"hellO"), InputRef::new("b", b"world")];
        matches!(
            v.validate(&inputs2, Some(&h1)).unwrap(),
            Validation::Fresh { .. }
        );
    }

    #[test]
    fn blake3_is_input_order_invariant() {
        let v = Blake3Validator;
        let a = [InputRef::new("x", b"1"), InputRef::new("y", b"2")];
        let b = [InputRef::new("y", b"2"), InputRef::new("x", b"1")];
        let Validation::Fresh { hash: Some(ha) } = v.validate(&a, None).unwrap() else {
            unreachable!()
        };
        let Validation::Fresh { hash: Some(hb) } = v.validate(&b, None).unwrap() else {
            unreachable!()
        };
        assert_eq!(ha, hb);
    }

    #[test]
    fn sha256_differs_from_blake3() {
        let inputs = [InputRef::new("a", b"payload")];
        let Validation::Fresh { hash: Some(hb) } = Blake3Validator.validate(&inputs, None).unwrap()
        else {
            unreachable!()
        };
        let Validation::Fresh { hash: Some(hs) } = Sha256Validator.validate(&inputs, None).unwrap()
        else {
            unreachable!()
        };
        assert_ne!(hb.digest, hs.digest);
        assert_eq!(hb.mode, ValidationMode::Blake3);
        assert_eq!(hs.mode, ValidationMode::Sha256);
    }

    #[test]
    fn mtime_requires_mtime() {
        let v = MTimeValidator;
        let no_mtime = [InputRef::new("x", b"irrelevant")];
        assert!(v.validate(&no_mtime, None).is_err());
        let with_mtime = [InputRef::mtime("x", 42)];
        matches!(
            v.validate(&with_mtime, None).unwrap(),
            Validation::Fresh { hash: None }
        );
    }

    #[test]
    fn blake3_requires_bytes() {
        let v = Blake3Validator;
        let no_bytes = [InputRef::mtime("x", 42)];
        assert!(v.validate(&no_bytes, None).is_err());
    }

    #[test]
    fn keyed_blake3_is_reserved() {
        assert!(matches!(
            validator_for(ValidationMode::KeyedBlake3),
            Err(ValidationError::NotImplemented)
        ));
    }

    #[test]
    fn short_is_16_chars() {
        let h = StepHash {
            mode: ValidationMode::Blake3,
            digest: Hash::of_bytes(b"x"),
        };
        assert_eq!(h.short().len(), 16);
        assert_eq!(h.hex().len(), 64);
    }

    proptest::proptest! {
        #[test]
        fn blake3_order_independence(
            names in proptest::collection::vec("[a-z]{1,8}", 1..8),
            payloads in proptest::collection::vec(proptest::collection::vec(proptest::num::u8::ANY, 0..32), 1..8),
        ) {
            let n = names.len().min(payloads.len());
            let mut pairs: Vec<(String, Vec<u8>)> = names.into_iter().zip(payloads).take(n).collect();
            // Dedup names so InputRef semantics are unambiguous.
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs.dedup_by(|a, b| a.0 == b.0);

            let refs_fwd: Vec<InputRef<'_>> = pairs.iter().map(|(n, b)| InputRef::new(n, b)).collect();
            let mut pairs_rev = pairs.clone();
            pairs_rev.reverse();
            let refs_rev: Vec<InputRef<'_>> = pairs_rev.iter().map(|(n, b)| InputRef::new(n, b)).collect();

            let Validation::Fresh { hash: Some(h1) } = Blake3Validator.validate(&refs_fwd, None).unwrap() else { unreachable!() };
            let Validation::Fresh { hash: Some(h2) } = Blake3Validator.validate(&refs_rev, None).unwrap() else { unreachable!() };
            proptest::prop_assert_eq!(h1, h2);
        }
    }
}
