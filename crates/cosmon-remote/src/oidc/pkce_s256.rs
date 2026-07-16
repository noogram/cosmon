// SPDX-License-Identifier: AGPL-3.0-only

//! PKCE S256 crypto for the user↔cosmon OAuth2 flow (delib-20260710-33b7 C7).
//!
//! **This is NOT [`crate::pkce`].** That module is the Claude/Anthropic
//! manual-paste flow, where the PKCE crypto lives on the server (the adapter
//! talks to Anthropic) and the CLI only shuttles a pasted code. Here the CLI
//! *is* the OAuth client: it mints the `code_verifier`, derives the S256
//! `code_challenge`, and holds a per-flow CSRF `state` — none of which the
//! Claude flow ever touches. The two modules are kept apart precisely so the
//! server-side Claude semantics are never confused with the client-side Forgejo
//! crypto (a confusion the brief calls out by name).
//!
//! Everything here is RFC 7636 (PKCE) + RFC 6749 §10.12 (`state`):
//!
//! - **`code_verifier`** — 32 bytes from the OS CSPRNG, base64url-nopad encoded
//!   into a 43-character high-entropy ASCII string (the unreserved set the RFC
//!   requires).
//! - **`code_challenge`** — `base64url-nopad(SHA-256(code_verifier_ascii))`, the
//!   `S256` method. The verifier never leaves the process until the token
//!   exchange; only the challenge rides the authorize URL.
//! - **`state`** — a 32-byte CSPRNG nonce, base64url-nopad, compared
//!   byte-for-byte on the loopback callback (RFC 6749 §10.12 CSRF defence).
//!
//! There is deliberately **no OIDC `nonce`** here. The OIDC `nonce` only earns
//! its keep when the `id_token` it is stamped into is validated (`nonce` claim
//! compared to the sent value, plus `iss` / `aud` / `exp` / JWKS signature).
//! This flow never validates an `id_token` — the access token is authoritative
//! server-side, and pulling a JWT+JWKS stack in would break the crate's
//! pure-Rust 4-target static-musl build (see `Cargo.toml`). A minted-but-never-
//! checked `nonce` is a *fake oracle*: a control the docs advertise but the code
//! never enforces, which is worse than its visible absence. So it is not minted.
//! (task-20260710-05f7, closing review task-20260710-a6ae F3.)

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Number of CSPRNG bytes behind a verifier / state token. 32 bytes = 256
/// bits, base64url-nopad encoded to 43 ASCII chars — comfortably inside the
/// RFC 7636 verifier length window `[43, 128]`.
const ENTROPY_BYTES: usize = 32;

/// Draw `ENTROPY_BYTES` from the OS CSPRNG and base64url-nopad encode them.
/// [`OsRng`] is a thin handle over the platform entropy source (getrandom); it
/// is not seedable and carries no state to leak.
fn random_b64url() -> String {
    let mut bytes = [0u8; ENTROPY_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A PKCE `code_verifier`: a high-entropy secret the client keeps until it
/// exchanges the authorization code, proving it is the same party that started
/// the flow.
///
/// The verifier is treated as ephemeral secret material — it lives only for the
/// duration of one `login` and is not persisted. Its [`Debug`] is redacted so a
/// stray `{:?}` cannot leak it into a log line.
#[derive(Clone)]
pub struct CodeVerifier(String);

impl CodeVerifier {
    /// Generate a fresh verifier from the OS CSPRNG.
    ///
    /// ```
    /// use cosmon_remote::oidc::CodeVerifier;
    /// let v = CodeVerifier::generate();
    /// // 32 bytes → 43 base64url chars, all from the RFC 7636 unreserved set.
    /// assert_eq!(v.as_str().len(), 43);
    /// assert!(v.as_str().bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    /// ```
    pub fn generate() -> Self {
        Self(random_b64url())
    }

    /// Reconstruct a verifier from a known string — **test-only**.
    ///
    /// This bypasses the CSPRNG and is gated behind `cfg(test)` so it cannot
    /// appear on the crate's public API: a downstream caller must not be able to
    /// mint a low-entropy verifier and silently defeat PKCE. Production code has
    /// exactly one constructor, [`CodeVerifier::generate`].
    #[cfg(test)]
    pub(crate) fn from_string(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Borrow the verifier for the one legitimate use — the token-exchange form
    /// (`code_verifier=…`).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Derive the `S256` `code_challenge` for this verifier:
    /// `base64url-nopad(SHA-256(verifier_ascii))`.
    ///
    /// ```
    /// use cosmon_remote::oidc::CodeVerifier;
    /// let v = CodeVerifier::generate();
    /// let challenge = v.code_challenge();
    /// // SHA-256 is 32 bytes → 43 base64url-nopad chars, and the challenge is
    /// // never the verifier (it is the digest, not the secret).
    /// assert_eq!(challenge.len(), 43);
    /// assert_ne!(challenge, v.as_str());
    /// ```
    ///
    /// The RFC 7636 Appendix B vector
    /// (`dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk` →
    /// `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM`) is checked in the
    /// `rfc7636_appendix_b_vector` unit test.
    pub fn code_challenge(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }
}

impl std::fmt::Debug for CodeVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodeVerifier(REDACTED)")
    }
}

/// A single-use CSRF nonce carried as the OAuth `state`. Generated per flow,
/// echoed by the authorization server on the loopback callback, and compared
/// byte-for-byte before the code is trusted. (It is *not* reused as an OIDC
/// `nonce`; see the module header for why that control is absent by design.)
///
/// Unlike [`CodeVerifier`] this is not a secret — it is a public anti-forgery
/// token — so its [`Debug`] shows the value (it aids debugging a mismatch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nonce(String);

impl Nonce {
    /// Generate a fresh nonce from the OS CSPRNG.
    pub fn generate() -> Self {
        Self(random_b64url())
    }

    /// Reconstruct a nonce from a known string — **test-only**.
    ///
    /// Gated behind `cfg(test)` so it cannot appear on the crate's public API: a
    /// downstream caller must not be able to mint a predictable nonce and defeat
    /// the CSRF/`state` check. Production code has exactly one constructor,
    /// [`Nonce::generate`].
    #[cfg(test)]
    pub(crate) fn from_string(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The nonce value, to place in the authorize URL.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time-ish equality against a value echoed by the callback. Uses a
    /// length check plus a byte fold so the comparison does not short-circuit on
    /// the first differing byte (the value is not a MAC, but folding costs
    /// nothing and avoids a trivial timing signal).
    pub fn verify(&self, echoed: &str) -> bool {
        let a = self.0.as_bytes();
        let b = echoed.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    #[test]
    fn verifier_is_43_chars_unreserved() {
        let v = CodeVerifier::generate();
        assert_eq!(v.as_str().len(), 43);
        assert!(v
            .as_str()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    #[test]
    fn rfc7636_appendix_b_vector() {
        // The canonical PKCE test vector: verifier → challenge.
        let v = CodeVerifier::from_string("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            v.code_challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn challenge_never_equals_verifier() {
        let v = CodeVerifier::generate();
        assert_ne!(v.code_challenge(), v.as_str());
    }

    #[test]
    fn debug_redacts_the_verifier() {
        let v = CodeVerifier::from_string("s3cr3t-verifier-value");
        assert_eq!(format!("{v:?}"), "CodeVerifier(REDACTED)");
        assert!(!format!("{v:?}").contains("s3cr3t"));
    }

    #[test]
    fn nonce_verify_matches_only_exact() {
        let n = Nonce::from_string("abc123");
        assert!(n.verify("abc123"));
        assert!(!n.verify("abc124"));
        assert!(!n.verify("abc1234"));
        assert!(!n.verify(""));
    }

    #[test]
    fn generated_nonces_are_unique() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(Nonce::generate().as_str().to_owned()));
        }
    }

    proptest! {
        /// The S256 challenge is deterministic: the same verifier always yields
        /// the same challenge, and it is always a 43-char base64url string
        /// (SHA-256 is 32 bytes → 43 base64url-nopad chars).
        #[test]
        fn prop_challenge_is_deterministic_and_well_formed(seed in ".{1,200}") {
            let v = CodeVerifier::from_string(&seed);
            let c1 = v.code_challenge();
            let c2 = v.code_challenge();
            prop_assert_eq!(&c1, &c2);
            prop_assert_eq!(c1.len(), 43);
            prop_assert!(c1
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        }

        /// `verify` accepts exactly the value the nonce was built from and
        /// rejects everything else.
        #[test]
        fn prop_nonce_verify_roundtrip(value in "[A-Za-z0-9_-]{1,64}", other in "[A-Za-z0-9_-]{1,64}") {
            let n = Nonce::from_string(&value);
            prop_assert!(n.verify(&value));
            prop_assert_eq!(n.verify(&other), value == other);
        }

        /// Distinct verifiers almost never collide on their challenge (the SHA-256
        /// pre-image resistance made observable): different input → different
        /// challenge.
        #[test]
        fn prop_distinct_verifiers_distinct_challenges(a in ".{1,100}", b in ".{1,100}") {
            prop_assume!(a != b);
            let ca = CodeVerifier::from_string(&a).code_challenge();
            let cb = CodeVerifier::from_string(&b).code_challenge();
            prop_assert_ne!(ca, cb);
        }
    }
}
