// SPDX-License-Identifier: AGPL-3.0-only

//! Soft-contract hash seals for operator-intent artifacts.
//!
//! A [`BriefingSeal`] records the BLAKE3 hash of a cognitive-contract
//! artifact (`prompt.md` at nucleation, `briefing.md` at each step
//! advance) together with the wall-clock time the hash was captured and
//! the artifact's byte length. Seals are **traces, not locks**: nothing
//! in the hot path blocks on a mismatch. `cs verify` consumes them to
//! detect retrospective edits to files that should be immutable once
//! their lifecycle moment has passed.
//!
//! # Why not chmod?
//!
//! A hard write-lock (`chmod -w`) would break `git clone`, `rsync`, and
//! Docker volumes that rely on mutable working-tree semantics. The seal
//! pattern is git-like: the working tree may be modified post-commit,
//! but `git status` tells you — and the commit hash is the anchor.
//!
//! # Why not a cryptographic signature?
//!
//! Signatures require key management. The seal only needs to catch the
//! *lazy* shadow contract — an LLM silently rewriting `briefing.md`
//! after nucleation — not a motivated adversary with filesystem access.
//! Anyone who can write `briefing.md` can also rewrite `state.json` to
//! match; the seal is a smoke alarm, not a vault door.
//!
//! # Canonical-form versions
//!
//! - `canonical_version = 0` (legacy / raw). Hash is BLAKE3 of the
//!   file's raw bytes. Sensitive to NFD/NFC, CRLF/LF, BOM, and trailing
//!   whitespace — *editor drift* alone can break the seal. Preserved
//!   for backwards-compat with pre-ADR-056 molecules; read-only.
//! - `canonical_version = 1` (text-v1). Hash is BLAKE3 of
//!   [`cosmon_hash::canonical_text_bytes`] applied to the artifact.
//!   Collapses NFD→NFC, CRLF→LF, strips BOM, enforces one trailing LF.
//!   All new seals emitted by `cs nucleate` / `cs evolve` use this
//!   recipe.
//!
//! ADR-056 (mint-protocol-v0) governs the canonical form and the
//! version bumping discipline.

use chrono::{DateTime, Utc};
use cosmon_hash::{canonical_text_bytes, CANONICAL_VERSION_RAW, CANONICAL_VERSION_TEXT_V1};
use serde::{Deserialize, Serialize};

/// A BLAKE3 hash seal captured at a specific lifecycle moment.
///
/// The `step` field is meaningful for briefing seals (0 = step 1's
/// briefing, 1 = step 2's briefing, …). Prompt seals use `step: 0`
/// by convention since there is only ever one prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefingSeal {
    /// Zero-based step index this seal refers to. `0` for prompt seals.
    pub step: u32,
    /// BLAKE3 hash of the artifact as 64-char lowercase hex.
    pub hash: String,
    /// Wall-clock time the hash was computed.
    pub sealed_at: DateTime<Utc>,
    /// Byte length of the hashed (canonical) artifact, informational.
    ///
    /// For `canonical_version = 1` this is the length *after*
    /// canonicalization — so comparing it to `std::fs::metadata().len()`
    /// will disagree when the file used NFD or CRLF. That's expected:
    /// the seal measures the *canonical* surface, not the on-disk byte
    /// count.
    pub briefing_bytes: u64,
    /// Canonical-form recipe used to produce the hash.
    ///
    /// Defaults to [`CANONICAL_VERSION_RAW`] for backwards-compat with
    /// seals written before ADR-056 landed: pre-existing `state.json`
    /// files have no `canonical_version` field and must still
    /// deserialize and verify. New seals use
    /// [`CANONICAL_VERSION_TEXT_V1`].
    #[serde(default)]
    pub canonical_version: u8,
    /// The exact content that was sealed, captured at seal time.
    ///
    /// This is the fix for the flagship tamper-evidence false positive.
    /// Some sealed artifacts are **legitimately rewritten** after the
    /// seal is stamped: cosmon regenerates `briefing.md` at every step
    /// (and `cs complete` rewrites it once more), and the bootstrap walk
    /// covers the operator's ambient `AGENTS.md` / `CLAUDE.md`, which
    /// drift outside the molecule's control. Verifying such a seal
    /// against the *current* file therefore fires on cosmon's own honest
    /// evolution, not on tampering.
    ///
    /// When a snapshot is present, `cs verify` compares the seal against
    /// this immutable, in-molecule content — the artifact **as it was at
    /// this epoch** — instead of the mutable live file. Genuine
    /// tamper-evidence is preserved: the snapshot is still checked
    /// against [`hash`](Self::hash) via [`matches`](Self::matches), so
    /// any post-hoc rewrite of the recorded snapshot (without a matching
    /// hash) is caught.
    ///
    /// `None` for prompt seals (whose file is genuinely immutable, so the
    /// live comparison is correct) and for legacy seals written before
    /// this field existed; a snapshot-less briefing/bootstrap seal is
    /// **inconclusive** at verify time, never a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

impl BriefingSeal {
    /// Compute a seal over raw bytes (legacy `canonical_version = 0`).
    ///
    /// Kept for backwards-compatibility and for artifacts that are
    /// genuinely binary (attachments). Prose artifacts should use
    /// [`BriefingSeal::of_text`] so that NFD/CRLF/BOM editor drift does
    /// not break the seal.
    #[must_use]
    pub fn of_bytes(step: u32, bytes: &[u8]) -> Self {
        Self {
            step,
            hash: cosmon_hash::Hash::of_bytes(bytes).to_hex(),
            sealed_at: Utc::now(),
            briefing_bytes: bytes.len() as u64,
            canonical_version: CANONICAL_VERSION_RAW,
            snapshot: None,
        }
    }

    /// Compute a seal over a UTF-8 text artifact using canonical-text-v1.
    ///
    /// Two files that produce the same
    /// [`cosmon_hash::canonical_text_bytes`] — e.g. the same prose
    /// copy-pasted on macOS (NFD + LF) and on Windows (NFC + CRLF) —
    /// hash to the same value. This is the seal a prose artifact
    /// should use; see ADR-056.
    #[must_use]
    pub fn of_text(step: u32, text: &str) -> Self {
        let canonical = canonical_text_bytes(text);
        Self {
            step,
            hash: cosmon_hash::Hash::of_bytes(&canonical).to_hex(),
            sealed_at: Utc::now(),
            briefing_bytes: canonical.len() as u64,
            canonical_version: CANONICAL_VERSION_TEXT_V1,
            snapshot: None,
        }
    }

    /// Attach the sealed content as an immutable in-molecule snapshot.
    ///
    /// The seal then carries the artifact **as it was at this epoch**, so
    /// `cs verify` can check it against this content instead of the
    /// mutable live file. Use this for artifacts cosmon legitimately
    /// rewrites after sealing — the per-step `briefing.md` and the
    /// ambient-file bootstrap walk — so that honest evolution does not
    /// read as tampering. See [`snapshot`](Self::snapshot) for the full
    /// rationale.
    ///
    /// The content is stored verbatim; [`matches`](Self::matches) applies
    /// the seal's canonical form when comparing, so passing the raw
    /// (pre-canonicalization) text is correct.
    #[must_use]
    pub fn with_snapshot(mut self, content: &str) -> Self {
        self.snapshot = Some(content.to_owned());
        self
    }

    /// Compute a seal over bytes read from disk that *ought* to be text.
    ///
    /// Falls back to [`BriefingSeal::of_bytes`] when the bytes are not
    /// valid UTF-8 — we never silently drop invalid sequences. This is
    /// the convenience entry point for callers that have already read a
    /// file into `Vec<u8>` (which is how most of cosmon accesses
    /// `prompt.md` and `briefing.md`).
    #[must_use]
    pub fn of_text_or_bytes(step: u32, bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(s) => Self::of_text(step, s),
            Err(_) => Self::of_bytes(step, bytes),
        }
    }

    /// Compare a seal's hash to the hash of some candidate bytes.
    ///
    /// Dispatches on `canonical_version`: `0` → raw bytes, `1` →
    /// canonical text. Unknown versions fail closed (`false`) — a
    /// verifier that cannot interpret a seal must not claim it matched.
    #[must_use]
    pub fn matches(&self, candidate: &[u8]) -> bool {
        match self.canonical_version {
            CANONICAL_VERSION_RAW => self.hash == cosmon_hash::Hash::of_bytes(candidate).to_hex(),
            CANONICAL_VERSION_TEXT_V1 => match std::str::from_utf8(candidate) {
                Ok(s) => {
                    let canonical = canonical_text_bytes(s);
                    self.hash == cosmon_hash::Hash::of_bytes(&canonical).to_hex()
                }
                Err(_) => false,
            },
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_is_deterministic() {
        let a = BriefingSeal::of_bytes(0, b"hello");
        let b = BriefingSeal::of_bytes(0, b"hello");
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.briefing_bytes, 5);
    }

    #[test]
    fn matches_detects_identity() {
        let seal = BriefingSeal::of_bytes(1, b"original");
        assert!(seal.matches(b"original"));
        assert!(!seal.matches(b"tampered"));
    }

    #[test]
    fn seal_captures_step_index() {
        let seal = BriefingSeal::of_bytes(3, b"x");
        assert_eq!(seal.step, 3);
    }

    #[test]
    fn serde_roundtrip() {
        let seal = BriefingSeal::of_bytes(2, b"data");
        let json = serde_json::to_string(&seal).unwrap();
        let back: BriefingSeal = serde_json::from_str(&json).unwrap();
        assert_eq!(seal, back);
    }

    #[test]
    fn text_seal_ignores_crlf_vs_lf() {
        // The whole point of canonical_version = 1.
        let a = BriefingSeal::of_text(0, "line one\r\nline two\r\n");
        let b = BriefingSeal::of_text(0, "line one\nline two\n");
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn text_seal_ignores_nfd_vs_nfc() {
        let composed = "café\n";
        let decomposed = "cafe\u{0301}\n";
        assert_eq!(
            BriefingSeal::of_text(0, composed).hash,
            BriefingSeal::of_text(0, decomposed).hash,
        );
    }

    #[test]
    fn text_seal_version_recorded() {
        assert_eq!(BriefingSeal::of_text(0, "x").canonical_version, 1);
        assert_eq!(BriefingSeal::of_bytes(0, b"x").canonical_version, 0);
    }

    #[test]
    fn legacy_seal_without_version_field_defaults_to_zero() {
        // A JSON blob written by an old cosmon build has no
        // `canonical_version` field — it must deserialize as v0.
        let legacy =
            r#"{"step":0,"hash":"00","sealed_at":"2024-01-01T00:00:00Z","briefing_bytes":0}"#;
        let seal: BriefingSeal = serde_json::from_str(legacy).unwrap();
        assert_eq!(seal.canonical_version, 0);
    }

    #[test]
    fn matches_dispatches_on_version() {
        let text = "hello\nworld\n";
        let text_alt_newlines = "hello\r\nworld\r\n";
        let v1 = BriefingSeal::of_text(0, text);
        // Same semantic content under v1 matches regardless of line endings.
        assert!(v1.matches(text.as_bytes()));
        assert!(v1.matches(text_alt_newlines.as_bytes()));

        // Under v0 the line endings change the hash.
        let v0 = BriefingSeal::of_bytes(0, text.as_bytes());
        assert!(v0.matches(text.as_bytes()));
        assert!(!v0.matches(text_alt_newlines.as_bytes()));
    }

    #[test]
    fn of_text_or_bytes_prefers_text_when_utf8() {
        let seal = BriefingSeal::of_text_or_bytes(0, b"hello\n");
        assert_eq!(seal.canonical_version, 1);
    }

    #[test]
    fn of_text_or_bytes_falls_back_on_invalid_utf8() {
        let bad: &[u8] = &[0xff, 0xfe];
        let seal = BriefingSeal::of_text_or_bytes(0, bad);
        assert_eq!(seal.canonical_version, 0);
    }

    #[test]
    fn with_snapshot_stores_content_and_still_matches_its_hash() {
        let seal = BriefingSeal::of_text(1, "step briefing\n").with_snapshot("step briefing\n");
        assert_eq!(seal.snapshot.as_deref(), Some("step briefing\n"));
        // The stored snapshot verifies against the seal's own hash.
        assert!(seal.matches(seal.snapshot.as_ref().unwrap().as_bytes()));
    }

    #[test]
    fn snapshot_defaults_none_and_is_skipped_when_absent() {
        let seal = BriefingSeal::of_text(0, "x");
        assert!(seal.snapshot.is_none());
        let json = serde_json::to_string(&seal).unwrap();
        assert!(
            !json.contains("snapshot"),
            "None snapshot must not be serialized: {json}"
        );
        let back: BriefingSeal = serde_json::from_str(&json).unwrap();
        assert_eq!(seal, back);
    }

    #[test]
    fn tampered_snapshot_no_longer_matches_the_hash() {
        // hash is over the honest content; the snapshot is swapped.
        let seal = BriefingSeal::of_text(1, "honest\n").with_snapshot("tampered\n");
        assert!(
            !seal.matches(seal.snapshot.as_ref().unwrap().as_bytes()),
            "a mutated snapshot must not match the sealed hash"
        );
    }

    #[test]
    fn snapshot_survives_serde_roundtrip() {
        let seal = BriefingSeal::of_text(2, "content\n").with_snapshot("content\n");
        let json = serde_json::to_string(&seal).unwrap();
        let back: BriefingSeal = serde_json::from_str(&json).unwrap();
        assert_eq!(seal, back);
        assert_eq!(back.snapshot.as_deref(), Some("content\n"));
    }
}
