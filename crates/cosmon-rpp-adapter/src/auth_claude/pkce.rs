// SPDX-License-Identifier: AGPL-3.0-only

//! PKCE (RFC 7636) helpers: random verifier, S256 challenge, and a
//! short-form session ID generator. All randomness sourced from
//! `rand::rngs::OsRng` (CSPRNG).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{Datelike, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Length of the PKCE `code_verifier` in raw random bytes. The
/// resulting URL-safe base64 string is ~43 characters (well within
/// RFC 7636's [43, 128] bound).
pub const CODE_VERIFIER_RAW_BYTES: usize = 32;

/// Length of the CSRF `state` parameter in raw random bytes. 32 bytes
/// → 43 characters of URL-safe base64 — the **exact** length the official
/// `claude auth login` CLI emits. claude.com *validates the state length*:
/// a 16-byte (22-char) state is rejected with `Invalid request format`,
/// even though it is a perfectly valid CSRF nonce. Byte-for-byte parity
/// with the official client therefore includes the LENGTH of random
/// values, not just the structure of the params. Discovered 2026-05-20 by
/// an isolation test against the real CLI (the genuine URL displayed the
/// code; ours errored) — see smithy ADR-0017 §12 (erratum #2).
pub const STATE_RAW_BYTES: usize = 32;

/// Length of the session-id random suffix in hex characters
/// (e.g. `auth-20260519-a8f2c1` → 6 hex chars from 3 random bytes).
pub const SESSION_SUFFIX_RAW_BYTES: usize = 3;

/// Produce a fresh PKCE `code_verifier` — 32 cryptographically-random
/// bytes, base64-url-no-pad encoded.
#[must_use]
pub fn new_code_verifier() -> String {
    let mut buf = [0u8; CODE_VERIFIER_RAW_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Derive the PKCE `code_challenge` for the S256 method from a
/// `code_verifier`. Per RFC 7636 §4.2: `BASE64URL(SHA256(verifier))`.
#[must_use]
pub fn s256_challenge(code_verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Produce a fresh CSRF `state` parameter (32 random bytes,
/// base64-url-no-pad → 43 chars, matching the official CLI's state length
/// which claude.com validates — see [`STATE_RAW_BYTES`]).
#[must_use]
pub fn new_oauth_state() -> String {
    let mut buf = [0u8; STATE_RAW_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Produce a fresh session ID of the form `auth-YYYYMMDD-<6 hex>`.
/// The date prefix lets operators eyeball-correlate session IDs with
/// daily activity; the hex suffix gives ~2^24 collision space per day.
#[must_use]
pub fn new_session_id() -> String {
    let now = Utc::now();
    let mut buf = [0u8; SESSION_SUFFIX_RAW_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    format!(
        "auth-{year:04}{month:02}{day:02}-{suffix}",
        year = now.year(),
        month = now.month(),
        day = now.day(),
        suffix = hex_lower(&buf),
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

/// Build the Anthropic authorize URL with PKCE challenge embedded.
///
/// Parameters per RFC 6749 + RFC 7636, **plus the proprietary
/// `code=true` flag** that Anthropic's `claude auth login` CLI sends.
/// Without `code=true` claude.com treats the request as a real
/// browser redirect to `redirect_uri` and rejects the headless /
/// copy-paste flow with `Invalid request format`. `code=true` is the
/// switch that puts the consent screen into *manual code display* mode
/// (it shows the authorization code for copy-paste instead of
/// redirecting). Discovered 2026-05-20 by byte-diffing our URL against
/// the official CLI — see smithy ADR-0017 §10.
///
/// Parameter order mirrors the official CLI exactly (matching the real
/// client byte-for-byte is safer than relying on OAuth order-agnosticism):
/// - `code=true`            (proprietary manual-display flag, first)
/// - `client_id=<CLIENT_ID>`
/// - `response_type=code`
/// - `redirect_uri=<MANUAL_REDIRECT_URL>`  (percent-encoded: `%3A%2F%2F…`)
/// - `scope=<space-separated>`             (**form-encoded: spaces → `+`**)
/// - `code_challenge=<S256(verifier)>`
/// - `code_challenge_method=S256`
/// - `state=<csrf>`
///
/// **Scope is the lone form-encoded param.** Anthropic's authorize
/// endpoint requires `application/x-www-form-urlencoded` spacing for the
/// scope tokens (`org%3Acreate_api_key+user%3Aprofile+…`). A `%20`-spaced
/// scope — though equally RFC 3986-valid — is rejected with
/// `Invalid request format`. Every other param keeps standard RFC 3986
/// percent-encoding (the `redirect_uri`'s `%2F` must NOT become `+`).
/// Discovered 2026-05-20 by byte-diffing against the official CLI — this
/// is the *second* time form-vs-percent parity bit us (after `code=true`):
/// the invariant is byte-for-byte parity with the official client, not
/// mere RFC-compliance. See smithy ADR-0017 §11.
#[must_use]
pub fn build_authorize_url(
    base: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &str,
    code_verifier: &str,
    oauth_state: &str,
) -> String {
    let challenge = s256_challenge(code_verifier);
    let mut url = String::with_capacity(base.len() + 512);
    url.push_str(base);
    url.push('?');
    push_qs(&mut url, "code", "true", true);
    push_qs(&mut url, "client_id", client_id, false);
    push_qs(&mut url, "response_type", "code", false);
    push_qs(&mut url, "redirect_uri", redirect_uri, false);
    push_qs_form(&mut url, "scope", scopes, false);
    push_qs(&mut url, "code_challenge", &challenge, false);
    push_qs(&mut url, "code_challenge_method", "S256", false);
    push_qs(&mut url, "state", oauth_state, false);
    url
}

fn push_qs(out: &mut String, key: &str, value: &str, first: bool) {
    if !first {
        out.push('&');
    }
    out.push_str(key);
    out.push('=');
    out.push_str(&percent_encode(value));
}

/// Like [`push_qs`] but form-encodes the value (spaces → `+`). Used for
/// the `scope` param only — see [`percent_encode_form`].
fn push_qs_form(out: &mut String, key: &str, value: &str, first: bool) {
    if !first {
        out.push('&');
    }
    out.push_str(key);
    out.push('=');
    out.push_str(&percent_encode_form(value));
}

/// Minimal RFC 3986 percent-encoding (unreserved set per §2.3 plus `~`;
/// space → `%20`). Avoids pulling `url` or `percent-encoding` crates into
/// the dep tree. Used for every query param **except** `scope`.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_upper_digit(b >> 4));
            out.push(hex_upper_digit(b & 0x0F));
        }
    }
    out
}

/// `application/x-www-form-urlencoded` encoding: identical to
/// [`percent_encode`] except ASCII space becomes `+` rather than `%20`
/// (a literal `+` in the input still percent-encodes to `%2B`). This is
/// the encoding Anthropic's authorize endpoint demands for the `scope`
/// param; a `%20`-spaced scope is rejected with `Invalid request format`
/// even though it is equally RFC 3986-valid. Scope is the *only* param
/// that uses this — see [`build_authorize_url`].
fn percent_encode_form(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b == b' ' {
            out.push('+');
        } else if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_upper_digit(b >> 4));
            out.push(hex_upper_digit(b & 0x0F));
        }
    }
    out
}

fn hex_upper_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'A' + n - 10) as char,
        _ => unreachable!("hex digit out of range"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_is_url_safe_and_long_enough() {
        let v = new_code_verifier();
        // 32 bytes base64-url-no-pad → ceil(32 * 4 / 3) = 43 chars.
        assert_eq!(v.len(), 43);
        assert!(v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')));
    }

    #[test]
    fn s256_challenge_known_vector() {
        // Vector from RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = s256_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn state_is_url_safe_and_exactly_43_chars() {
        let s = new_oauth_state();
        assert!(s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')));
        // 32 bytes base64-url-no-pad → ceil(32 * 4 / 3) = 43 chars — the
        // exact length the official `claude auth login` CLI emits.
        // claude.com VALIDATES this length: a 22-char (16-byte) state is
        // rejected with `Invalid request format`. See ADR-0017 §12.
        assert_eq!(
            s.len(),
            43,
            "state must be 43 chars (32 bytes) to match the official CLI, got {} ({s:?})",
            s.len()
        );
    }

    #[test]
    fn session_id_shape() {
        let id = new_session_id();
        // auth-YYYYMMDD-XXXXXX = 5 + 8 + 1 + 6 = 20
        assert_eq!(id.len(), 20, "session_id should be 20 chars, got {id:?}");
        assert!(id.starts_with("auth-"));
    }

    #[test]
    fn authorize_url_contains_all_params() {
        let url = build_authorize_url(
            "https://claude.com/cai/oauth/authorize",
            "client-123",
            "https://platform.claude.com/oauth/code/callback",
            "user:profile user:inference",
            "verifier-foo",
            "state-bar",
        );
        // `code=true` must be the FIRST query param — it puts the
        // Anthropic consent screen into manual code-display mode.
        // Without it the flow fails with `Invalid request format`.
        assert!(url.starts_with("https://claude.com/cai/oauth/authorize?code=true&"));
        assert!(url.contains("code=true"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client-123"));
        assert!(url
            .contains("redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback"));
        // Scope spaces MUST be `+` (form-encoding), never `%20`.
        assert!(url.contains("scope=user%3Aprofile+user%3Ainference"));
        assert!(
            !url.contains("scope=user%3Aprofile%20"),
            "scope spaces must form-encode to `+`, not `%20`: {url}"
        );
        assert!(url.contains("state=state-bar"));
        assert!(url.contains("code_challenge_method=S256"));
        let expected_challenge = s256_challenge("verifier-foo");
        assert!(url.contains(&format!("code_challenge={expected_challenge}")));
    }

    #[test]
    fn form_encode_spaces_to_plus_keep_plus_as_percent() {
        // Spaces → `+`; a literal `+` → `%2B`; `:` (reserved) → `%3A`.
        assert_eq!(percent_encode_form("a b"), "a+b");
        assert_eq!(percent_encode_form("a+b"), "a%2Bb");
        assert_eq!(percent_encode_form("user:profile"), "user%3Aprofile");
        // Percent-encode keeps space as `%20` (every other param relies on it).
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    /// Byte-for-byte parity guard against the official `claude auth login`
    /// CLI. The expected string below was captured from the real CLI on
    /// 2026-05-20 (state + code_challenge held at fixed values for a stable
    /// structural diff). Only `scope` is form-encoded (`+`); `redirect_uri`
    /// keeps `%2F`. If this assertion ever fails, our URL has drifted from
    /// the client the Anthropic endpoint accepts — see smithy ADR-0017 §11.
    ///
    /// **Length assertions (ADR-0017 §12, erratum #2).** Holding state and
    /// challenge at fixed values makes the structural diff readable, but it
    /// *masks the length of the real generated values* — exactly the gap
    /// that let a 16-byte (22-char) state ship undetected through the ca4c
    /// byte-for-byte check. claude.com validates the state LENGTH, so this
    /// test now also asserts that the genuinely-generated `state` and
    /// `code_challenge` are each 43 chars — the official CLI's length.
    /// Lesson: byte-for-byte parity includes the LENGTH of random values,
    /// not just the structure of the params.
    #[test]
    fn authorize_url_byte_for_byte_matches_official_cli() {
        use crate::auth_claude::config::{
            DEFAULT_AUTHORIZE_URL, DEFAULT_CLAUDE_CLI_CLIENT_ID, DEFAULT_REDIRECT_URI,
            DEFAULT_SCOPES,
        };

        // The official `claude auth login` CLI emits a 43-char state
        // (32 bytes) and a 43-char S256 challenge; claude.com rejects
        // anything else with `Invalid request format`. See ADR-0017 §12.
        const OFFICIAL_CLI_STATE_LEN: usize = 43;
        const S256_CHALLENGE_LEN: usize = 43;

        let url = build_authorize_url(
            DEFAULT_AUTHORIZE_URL,
            DEFAULT_CLAUDE_CLI_CLIENT_ID,
            DEFAULT_REDIRECT_URI,
            DEFAULT_SCOPES,
            "fixed-verifier-for-test",
            "FIXED_STATE",
        );
        let challenge = s256_challenge("fixed-verifier-for-test");

        let expected = format!(
            "https://claude.com/cai/oauth/authorize\
             ?code=true\
             &client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e\
             &response_type=code\
             &redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback\
             &scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference\
             +user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload\
             &code_challenge={challenge}\
             &code_challenge_method=S256\
             &state=FIXED_STATE"
        );

        assert_eq!(url, expected, "generated URL drifted from official CLI");

        // Length parity — the dimension the fixed-value structural diff
        // cannot see, and exactly the gap that let a 16-byte state ship.
        let real_state = new_oauth_state();
        assert_eq!(
            real_state.len(),
            OFFICIAL_CLI_STATE_LEN,
            "generated state length must match the official CLI (43 chars / 32 bytes), \
             got {} ({real_state:?})",
            real_state.len()
        );

        let real_challenge = s256_challenge(&new_code_verifier());
        assert_eq!(
            real_challenge.len(),
            S256_CHALLENGE_LEN,
            "generated code_challenge must be 43 chars (S256 → 32 bytes), got {}",
            real_challenge.len()
        );
    }
}
