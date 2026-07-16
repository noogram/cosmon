// SPDX-License-Identifier: AGPL-3.0-only

//! `galaxy-seed` — the cross-instance canonical referent of a galaxy.
//!
//! A `galaxy-seed` is the BLAKE3 digest of a galaxy's **genesis event**
//! (line 1 of its `events.jsonl` ledger). It is the object-identifier that
//! a federation [`ScopeBadge`] anchors to (smithy ADR-0023 §D4/§D8): it
//! says *which galaxy* we are talking about, independent of either side's
//! namespace, and it survives the death of the emitter because any peer can
//! recompute it from a received bundle.
//!
//! # Two recipes, one robust
//!
//! Hashing the genesis line admits two strategies:
//!
//! * [`galaxy_seed_raw`] — `BLAKE3(line_bytes)`. Trivially deterministic
//!   when the transport is byte-exact (a `git bundle` preserves blob bytes),
//!   but **fragile**: a single re-serialization, key reordering, trailing
//!   whitespace, or `LF`→`CRLF` normalization changes the digest. This is
//!   the forensic path — it answers "are these the same bytes?".
//!
//! * [`galaxy_seed`] — `BLAKE3(canonical_json(parse(line)))`. Parses the
//!   line into a `serde_json::Value` and re-emits it through
//!   [`canonical_serialize`](crate::canonical_serialize) (sorted keys, no
//!   whitespace) before hashing. This is **schema-independent** (it does not
//!   deserialize into a Rust struct, so a peer running a newer cosmon binary
//!   with extra event fields still recomputes the same seed for the same
//!   logical event) and **robust** to the benign mutations a transport may
//!   introduce. This is the recipe a `ScopeBadge` MUST use.
//!
//! # Determinism is "frozen-at-birth", not "regenerated"
//!
//! A genesis event is written **once**, at galaxy nucleation, and is then
//! immutable (D9 DNA — cosmon-state append-only). The clone scenario the
//! badge relies on — Dave bundles `speck`, sneakernets to Casey, Casey
//! reconstructs — **replicates the frozen bytes**; it does not re-run
//! nucleation. The seed is therefore deterministic for the clone path.
//!
//! It is emphatically **not** reproducible by re-nucleation: a genesis event
//! carries clock-derived fields (`timestamp`, `ts`) and, for the
//! `operator_present` genesis shape, a session-local field (`sid`, e.g.
//! `"cli-67799"`). Re-nucleating the "same" galaxy on a different host yields
//! a different seed by construction. Use [`nondeterministic_fields`] to
//! enumerate the fields that make re-nucleation non-reproducible.

use serde_json::Value;

use crate::{canonical_serialize, CanonicalError, Hash};

/// Field names known to be clock-, session-, or host-derived in genesis
/// events across the observed galaxy shapes (`molecule_nucleated` and
/// `operator_present`).
///
/// These are the fields that make a genesis event **non-reproducible by
/// re-nucleation** (they are fixed at birth and only survive via byte-exact
/// cloning). They are listed so a verifier can audit a candidate genesis
/// event and report *why* a seed is frozen-at-birth rather than regenerable.
pub const VOLATILE_GENESIS_FIELDS: &[&str] = &["timestamp", "ts", "sid"];

/// Compute the canonical `galaxy-seed` from a genesis line.
///
/// Parses `genesis_line` as a JSON object and hashes its
/// [`canonical_serialize`](crate::canonical_serialize) form. This is the
/// recipe a [`ScopeBadge`] anchors to: robust to key reordering, whitespace,
/// and line-ending normalization, and independent of the reader's struct
/// schema.
///
/// `genesis_line` should be the raw bytes of line 1 of `events.jsonl`,
/// with or without a trailing newline (the newline is not part of the JSON
/// value and does not affect the result).
///
/// # Errors
///
/// Returns [`CanonicalError`] if `genesis_line` is not valid JSON.
pub fn galaxy_seed(genesis_line: &[u8]) -> Result<Hash, CanonicalError> {
    let v: Value = serde_json::from_slice(genesis_line)?;
    Ok(Hash::of_bytes(&canonical_serialize(&v)?))
}

/// Compute the byte-exact (forensic) `galaxy-seed` from a genesis line.
///
/// `BLAKE3` of the raw bytes, after stripping a single trailing `\n` and/or
/// `\r` so that the JSONL line separator does not leak into the digest. Use
/// this only to answer "are these the *same bytes*?" — prefer [`galaxy_seed`]
/// as the federation referent.
#[must_use]
pub fn galaxy_seed_raw(genesis_line: &[u8]) -> Hash {
    let mut end = genesis_line.len();
    while end > 0 && (genesis_line[end - 1] == b'\n' || genesis_line[end - 1] == b'\r') {
        end -= 1;
    }
    Hash::of_bytes(&genesis_line[..end])
}

/// Enumerate the volatile (clock/session/host-derived) fields actually
/// present in a candidate genesis line.
///
/// Returns the subset of [`VOLATILE_GENESIS_FIELDS`] that appear as keys in
/// the parsed genesis object, in declaration order. An empty result means
/// the genesis event is, on its face, regenerable; a non-empty result is the
/// auditable reason the seed must be cloned rather than re-nucleated.
///
/// # Errors
///
/// Returns [`CanonicalError`] if `genesis_line` is not a valid JSON object.
pub fn nondeterministic_fields(genesis_line: &[u8]) -> Result<Vec<&'static str>, CanonicalError> {
    let v: Value = serde_json::from_slice(genesis_line)?;
    let obj = v.as_object();
    Ok(VOLATILE_GENESIS_FIELDS
        .iter()
        .filter(|f| obj.is_some_and(|m| m.contains_key(**f)))
        .copied()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real genesis lines captured from live galaxy ledgers (2026-06-17).
    const SPECK: &[u8] = br#"{"seq":0,"timestamp":"2026-06-04T14:26:33.899332Z","emitter_kind":"unknown","emitter_id":"","meta_level":0,"type":"operator_present","sid":"cli-67799","phase":"Biological","ts":"2026-06-04T14:26:33.899246Z","source":"internal"}"#;
    const COSMON: &[u8] = br#"{"seq":0,"mol_seq":0,"timestamp":"2026-04-20T10:13:37.570883Z","type":"molecule_nucleated","molecule_id":"delib-20260420-74b8","formula_id":"deep-think"}"#;

    #[test]
    fn galaxy_seed_canonical_invariant_to_key_order_and_whitespace() {
        // Same logical event, keys reordered + whitespace + trailing newline.
        let reordered = br#"{ "formula_id":"deep-think", "type":"molecule_nucleated", "molecule_id":"delib-20260420-74b8", "mol_seq":0, "timestamp":"2026-04-20T10:13:37.570883Z", "seq":0 }
"#;
        assert_eq!(
            galaxy_seed(COSMON).unwrap(),
            galaxy_seed(reordered).unwrap(),
            "canonical seed must be stable under key reorder + whitespace"
        );
    }

    #[test]
    fn raw_seed_is_fragile_where_canonical_is_robust() {
        let reordered = br#"{"formula_id":"deep-think","type":"molecule_nucleated","molecule_id":"delib-20260420-74b8","mol_seq":0,"timestamp":"2026-04-20T10:13:37.570883Z","seq":0}"#;
        // Raw digest differs (bytes differ) ...
        assert_ne!(galaxy_seed_raw(COSMON), galaxy_seed_raw(reordered));
        // ... canonical digest is identical (same logical event).
        assert_eq!(
            galaxy_seed(COSMON).unwrap(),
            galaxy_seed(reordered).unwrap()
        );
    }

    #[test]
    fn raw_seed_ignores_trailing_newline() {
        let mut with_nl = COSMON.to_vec();
        with_nl.push(b'\n');
        assert_eq!(galaxy_seed_raw(COSMON), galaxy_seed_raw(&with_nl));
    }

    #[test]
    fn two_byte_exact_clones_match() {
        // Simulates bundle/sneakernet byte-exact transport.
        let clone_a = SPECK.to_vec();
        let clone_b = SPECK.to_vec();
        assert_eq!(galaxy_seed_raw(&clone_a), galaxy_seed_raw(&clone_b));
        assert_eq!(
            galaxy_seed(&clone_a).unwrap(),
            galaxy_seed(&clone_b).unwrap()
        );
    }

    #[test]
    fn nondeterministic_fields_detected_per_shape() {
        // operator_present genesis carries a session-local id → not regenerable.
        assert_eq!(
            nondeterministic_fields(SPECK).unwrap(),
            vec!["timestamp", "ts", "sid"]
        );
        // molecule_nucleated genesis is clock-stamped but session-free.
        assert_eq!(nondeterministic_fields(COSMON).unwrap(), vec!["timestamp"]);
    }

    #[test]
    fn rust_referent_matches_shell_proof_bit_for_bit() {
        // Pinned from tools/galaxy-seed-bundle-roundtrip.sh on the live
        // cosmon genesis line. Locks Rust `galaxy_seed` == the JCS+BLAKE3
        // recipe the shell harness uses, so the two proofs cannot drift.
        assert_eq!(
            galaxy_seed(COSMON).unwrap().to_hex(),
            "eb75899dbd67e1ef9ba496a2d23e9854cce5ab169998a42d9f3c1886c91595e4"
        );
    }

    #[test]
    fn distinct_galaxies_have_distinct_seeds() {
        assert_ne!(galaxy_seed(SPECK).unwrap(), galaxy_seed(COSMON).unwrap());
    }

    #[test]
    fn invalid_json_is_an_error_not_a_panic() {
        assert!(galaxy_seed(b"not json").is_err());
        assert!(nondeterministic_fields(b"not json").is_err());
    }
}
