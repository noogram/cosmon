// SPDX-License-Identifier: AGPL-3.0-only

//! Canonical-text form for content hashing of human-written artifacts.
//!
//! Two semantically identical prose artifacts must produce the same byte
//! sequence — and therefore the same BLAKE3 hash — regardless of how
//! they arrived on disk. macOS produces NFD, Windows produces CRLF, an
//! editor may sprinkle a BOM, a shell pipe may strip the trailing LF.
//! Without a canonical form every one of those accidents forks the hash
//! chain silently and the seal becomes worse than useless: it flags
//! *editor drift* as tampering.
//!
//! The canonical form for `canonical_version = 1` is:
//!
//! 1. **UTF-8 decode, strict.** Invalid sequences → error (no
//!    lossy-replacement). The input must already be a valid `&str`; the
//!    byte-level helper does the decode and surfaces the error.
//! 2. **Strip a leading BOM** (`U+FEFF`) if present.
//! 3. **NFC-normalize** the entire string (Unicode Normalization Form C).
//! 4. **Convert line endings to LF.** `\r\n` → `\n`, lone `\r` → `\n`.
//! 5. **Ensure exactly one trailing LF.** Strip trailing whitespace-only
//!    LF runs down to a single `\n`; if the last code point is not LF,
//!    append one.
//!
//! This is the minimum canonical-form surface that kills NFD/CRLF/BOM
//! drift. It is intentionally narrow: no Markdown normalization, no
//! whitespace
//! collapse, no code-fence tweaking. The hash is of *the bytes a
//! sensible editor would write*, not of a normalized view.
//!
//! Legacy seals used raw bytes (`canonical_version = 0`) and will
//! continue to load and verify under their old hash recipe.

use unicode_normalization::UnicodeNormalization;

/// Canonical-form version for text artifact seals.
///
/// Bumped whenever the recipe above changes. A seal records the version
/// it was produced under so a verifier can select the matching recipe.
pub const CANONICAL_VERSION_TEXT_V1: u8 = 1;

/// Canonical-form version for legacy raw-byte seals.
///
/// Predates the text canonicalization. Verifiers must fall back to
/// byte-for-byte comparison for these seals.
pub const CANONICAL_VERSION_RAW: u8 = 0;

/// Errors emitted while canonicalizing text for hashing.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalTextError {
    /// The input bytes were not valid UTF-8.
    ///
    /// The seal protocol explicitly rejects lossy-replacement so that
    /// two distinct invalid-UTF-8 inputs never collide under the
    /// canonical form. A caller receiving this error should treat the
    /// artifact as un-sealable and investigate, not silently sanitize.
    #[error("input is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
}

/// Canonicalize a string to the bytes that get hashed.
///
/// Applies the full `canonical_version = 1` recipe. Never fails — the
/// input is already guaranteed to be valid UTF-8 by the `&str` type.
#[must_use]
pub fn canonical_text_bytes(text: &str) -> Vec<u8> {
    // Step 2 — strip leading BOM.
    let trimmed = text.strip_prefix('\u{FEFF}').unwrap_or(text);

    // Step 3 — NFC normalize, collecting into a String.
    let nfc: String = trimmed.nfc().collect();

    // Steps 4 + 5 — rewrite line endings, enforce one trailing LF.
    let mut out = String::with_capacity(nfc.len() + 1);
    let bytes = nfc.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                // \r\n → \n, lone \r → \n
                out.push('\n');
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                // Copy one UTF-8 code point. We know bytes came from a
                // valid UTF-8 String so we can use char_indices via
                // slice.
                let rest = &nfc[i..];
                let ch = rest.chars().next().expect("non-empty UTF-8 rest");
                let len = ch.len_utf8();
                out.push_str(&nfc[i..i + len]);
                i += len;
            }
        }
    }

    // Collapse trailing runs of \n down to exactly one.
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }

    out.into_bytes()
}

/// Canonicalize a byte slice to canonical-text bytes.
///
/// Strict UTF-8 decode, then the same recipe as
/// [`canonical_text_bytes`]. Returns [`CanonicalTextError::InvalidUtf8`]
/// if the input is not valid UTF-8 — the seal protocol forbids
/// lossy-replacement (a caller that wants to seal a binary artifact
/// should hash raw bytes under `canonical_version = 0`).
///
/// # Errors
///
/// Returns [`CanonicalTextError::InvalidUtf8`] on invalid UTF-8.
pub fn canonical_text_from_bytes(bytes: &[u8]) -> Result<Vec<u8>, CanonicalTextError> {
    let s = std::str::from_utf8(bytes)?;
    Ok(canonical_text_bytes(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfd_and_nfc_collide_after_canonicalization() {
        // "é" composed (NFC, U+00E9) vs decomposed (NFD, U+0065 U+0301).
        let nfc = "Café\n";
        let nfd = "Cafe\u{0301}\n";
        assert_eq!(canonical_text_bytes(nfc), canonical_text_bytes(nfd));
    }

    #[test]
    fn crlf_folds_to_lf() {
        let a = canonical_text_bytes("one\r\ntwo\r\n");
        let b = canonical_text_bytes("one\ntwo\n");
        assert_eq!(a, b);
    }

    #[test]
    fn lone_cr_folds_to_lf() {
        let a = canonical_text_bytes("a\rb\n");
        let b = canonical_text_bytes("a\nb\n");
        assert_eq!(a, b);
    }

    #[test]
    fn leading_bom_stripped() {
        let a = canonical_text_bytes("\u{FEFF}hello\n");
        let b = canonical_text_bytes("hello\n");
        assert_eq!(a, b);
    }

    #[test]
    fn trailing_lf_enforced() {
        let a = canonical_text_bytes("no-trailing");
        assert_eq!(a, b"no-trailing\n");
    }

    #[test]
    fn trailing_lf_collapsed_to_one() {
        let a = canonical_text_bytes("two-trailing\n\n\n");
        assert_eq!(a, b"two-trailing\n");
    }

    #[test]
    fn empty_input_becomes_single_lf() {
        // Degenerate but well-defined — avoids the "empty file = no
        // bytes, no hash" foot-gun.
        assert_eq!(canonical_text_bytes(""), b"\n");
    }

    #[test]
    fn semantically_identical_variants_produce_same_bytes() {
        // Test vector from delib-20260420-bae4: the same prose written
        // on three different systems (macOS NFD + LF, Windows NFC +
        // CRLF, Linux NFC + LF + BOM) must all canonicalize to the
        // same bytes.
        let macos = "résumé\n";
        let nfd = "re\u{0301}sume\u{0301}\n";
        let windows = "résumé\r\n";
        let with_bom = "\u{FEFF}résumé\n";
        let a = canonical_text_bytes(macos);
        assert_eq!(a, canonical_text_bytes(nfd));
        assert_eq!(a, canonical_text_bytes(windows));
        assert_eq!(a, canonical_text_bytes(with_bom));
    }

    #[test]
    fn distinct_content_produces_distinct_bytes() {
        assert_ne!(
            canonical_text_bytes("alpha\n"),
            canonical_text_bytes("beta\n")
        );
    }

    #[test]
    fn invalid_utf8_rejected() {
        let bad: &[u8] = &[0xff, 0xfe, 0xfd];
        assert!(matches!(
            canonical_text_from_bytes(bad),
            Err(CanonicalTextError::InvalidUtf8(_))
        ));
    }

    #[test]
    fn valid_utf8_bytes_round_trip() {
        let text = "Hello, 世界!\n";
        let from_text = canonical_text_bytes(text);
        let from_bytes = canonical_text_from_bytes(text.as_bytes()).unwrap();
        assert_eq!(from_text, from_bytes);
    }

    #[test]
    fn idempotence() {
        // Canonicalizing the canonical form is a no-op.
        let once = canonical_text_bytes("mixed\r\nendings\r\n");
        let twice = canonical_text_from_bytes(&once).unwrap();
        assert_eq!(once, twice);
    }
}
