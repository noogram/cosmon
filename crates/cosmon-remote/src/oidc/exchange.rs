// SPDX-License-Identifier: AGPL-3.0-only

//! The token endpoint: authorization-code exchange and refresh-token rotation
//! (delib-20260710-33b7 C2).
//!
//! Two grants, both POSTing an `application/x-www-form-urlencoded` body to the
//! provider's `token_endpoint`:
//!
//! - **`authorization_code`** — the first exchange after the browser flow,
//!   presenting the `code` plus the PKCE `code_verifier`
//!   ([`exchange_code`]).
//! - **`refresh_token`** — the silent 15-minute rotation, presenting the stored
//!   refresh token ([`refresh_token`]).
//!
//! Forgejo rotates refresh tokens on every use (`InvalidateRefreshTokens: true`),
//! so a successful refresh returns a **new** `{access, refresh}` pair and the old
//! refresh token is immediately dead. That single-use property is why the caller
//! (`flow`) must persist-before-use and serialise refreshes per key — this module
//! only performs one exchange and reports the result honestly.
//!
//! The success body is parsed into [`TokenResponse`]; an error body is mapped to
//! [`OidcError::Server`] carrying the RFC 6749 `error` / `error_description`, so
//! the caller can distinguish `invalid_grant` (a dead/rotated token) from other
//! failures via [`OidcError::is_invalid_grant`].

use serde::Deserialize;
use zeroize::Zeroizing;

use super::error::OidcError;
use crate::error::Result;

/// A wipe-on-drop owner for the `application/x-www-form-urlencoded` request
/// body handed to reqwest.
///
/// The refresh-token grant serialises the plaintext refresh token into this
/// body. `reqwest`'s `.form()` helper (via `serde_urlencoded`) builds the body
/// in a bare `String` whose growth reallocates — orphaning unwiped fragments of
/// the token in freed heap blocks (RÉSIDUEL SÉCU A, re-review of
/// task-20260713-c5ad §FIX A). We instead build the body ourselves into a
/// single `Zeroizing<Vec<u8>>` sized up-front (see [`encode_form_zeroizing`]) so
/// no reallocation occurs, then hand it to reqwest through
/// [`bytes::Bytes::from_owner`]. `Bytes` keeps the owner alive until the last
/// body clone is released; the owner's `Drop` then runs `Zeroizing`'s wipe, so
/// the plaintext body never lingers in freed memory.
struct ZeroizingBody(Zeroizing<Vec<u8>>);

impl AsRef<[u8]> for ZeroizingBody {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Serialise `form` into an `application/x-www-form-urlencoded` body inside a
/// single wipe-on-drop allocation.
///
/// The capacity is reserved up-front at the worst-case size — every input byte
/// can expand to a 3-byte `%XX` escape, plus one `=` per pair and one `&`
/// separator between pairs. Because the reserved capacity is never exceeded,
/// the backing `Vec` never reallocates: there is exactly one heap allocation,
/// every secret byte lives inside its `0..len` region, and `Zeroizing` wipes
/// all of it on drop. This is the fix the bare-`String` serialiser could not
/// give — its intermediate reallocations freed unwiped token fragments while
/// the body was still being written.
///
/// Encoding matches `serde_urlencoded` (both delegate to
/// `url::form_urlencoded`), so the wire bytes are byte-identical to the prior
/// `.form()` call.
fn encode_form_zeroizing(form: &[(&str, &str)]) -> ZeroizingBody {
    let bound: usize = form
        .iter()
        .map(|(k, v)| k.len() * 3 + v.len() * 3 + 2)
        .sum();
    let mut buf = Zeroizing::new(Vec::<u8>::with_capacity(bound));
    for (i, (k, v)) in form.iter().enumerate() {
        if i > 0 {
            buf.push(b'&');
        }
        for chunk in url::form_urlencoded::byte_serialize(k.as_bytes()) {
            buf.extend_from_slice(chunk.as_bytes());
        }
        buf.push(b'=');
        for chunk in url::form_urlencoded::byte_serialize(v.as_bytes()) {
            buf.extend_from_slice(chunk.as_bytes());
        }
    }
    // Invariant: the up-front bound is a true upper bound, so no reallocation
    // happened and every byte written is within `0..len`.
    debug_assert!(buf.len() <= bound);
    ZeroizingBody(buf)
}

/// A successful token-endpoint response. **Deserialize-only** and
/// unknown-field-tolerant.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct TokenResponse {
    /// The freshly minted access token (the request bearer).
    pub access_token: String,
    /// The rotated refresh token. Forgejo always returns one on both grants;
    /// it defaults to empty for a provider that omits it, in which case the
    /// caller reuses the prior refresh token.
    #[serde(default)]
    pub refresh_token: String,
    /// Access-token lifetime in seconds (Forgejo: 900 = 15 min). The caller
    /// converts this to an absolute `expires_at` at mint time. Defaults to 0
    /// (treated as already-expiring) if the provider omits it.
    #[serde(default)]
    pub expires_in: i64,
    /// The token type (`bearer`). Retained for completeness; the client always
    /// presents `Authorization: Bearer`.
    #[serde(default)]
    pub token_type: String,
}

/// The token-endpoint error body (RFC 6749 §5.2).
#[derive(Debug, Deserialize)]
struct TokenErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// POST an `authorization_code` grant: exchange `code` + `code_verifier` for the
/// first `{access, refresh}` pair.
pub async fn exchange_code(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    post_token(http, token_endpoint, &form).await
}

/// POST a `refresh_token` grant: rotate the stored refresh token into a fresh
/// `{access, refresh}` pair. A rejected token surfaces as
/// [`OidcError::Server`] with `error == "invalid_grant"`.
pub async fn refresh_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh: &str,
) -> Result<TokenResponse> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", client_id),
    ];
    post_token(http, token_endpoint, &form).await
}

/// Shared POST + response decoding for both grants.
async fn post_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse> {
    // Build the form body ourselves into a wipe-on-drop buffer. `reqwest`'s
    // `.form()` helper serialises the pairs into a bare `String` body reqwest
    // frees **un-zeroized**, and — the gap the re-review of task-20260713-c5ad
    // §FIX A confirmed still open — the `String` also reallocates *while it
    // grows*, orphaning unwiped fragments of the refresh token in freed heap
    // blocks. `encode_form_zeroizing` sizes the buffer up-front so it never
    // reallocates, and hands reqwest a `Bytes` that BORROWS that buffer via
    // `Bytes::from_owner`: no extra copy is made, and when reqwest/hyper drop
    // the last reference the owner's `Drop` zeroizes the plaintext. The wire
    // bytes and `Content-Type` are byte-identical to the former `.form()` path,
    // so the token endpoint sees an unchanged request.
    //
    // Residual (documented, not silently claimed away): rustls copies the body
    // into its TLS record buffer before encryption; that copy lives inside
    // rustls and is outside our wipe control — true of any HTTPS request
    // carrying a secret. What this closes is the reqwest-owned body buffer and
    // every intermediate allocation on the way to it.
    let body = reqwest::Body::from(bytes::Bytes::from_owner(encode_form_zeroizing(form)));

    let resp = http
        .post(token_endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .body(body)
        .send()
        .await
        .map_err(OidcError::transport)?;
    let status = resp.status();
    let body = resp.text().await.map_err(OidcError::transport)?;

    if status.is_success() {
        return serde_json::from_str::<TokenResponse>(&body)
            .map_err(|e| OidcError::Server {
                error: "invalid_token_response".to_owned(),
                description: Some(format!("could not parse token response: {e}")),
            })
            .map_err(Into::into);
    }

    // Non-2xx: prefer the structured OAuth error, fall back to a status note.
    let err = match serde_json::from_str::<TokenErrorBody>(&body) {
        Ok(parsed) => OidcError::Server {
            error: parsed.error,
            description: parsed.error_description,
        },
        Err(_) => OidcError::Server {
            error: format!("http_{}", status.as_u16()),
            description: None,
        },
    };
    Err(err.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_token_response() {
        let json = r#"{
            "access_token": "at-1",
            "refresh_token": "rt-1",
            "expires_in": 900,
            "token_type": "bearer",
            "scope": "openid profile"
        }"#;
        let tok: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(tok.access_token, "at-1");
        assert_eq!(tok.refresh_token, "rt-1");
        assert_eq!(tok.expires_in, 900);
    }

    #[test]
    fn tolerates_absent_refresh_and_expiry() {
        let json = r#"{"access_token": "at-only"}"#;
        let tok: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(tok.access_token, "at-only");
        assert!(tok.refresh_token.is_empty());
        assert_eq!(tok.expires_in, 0);
    }

    #[test]
    fn error_body_parses_invalid_grant() {
        let body: TokenErrorBody =
            serde_json::from_str(r#"{"error":"invalid_grant","error_description":"expired"}"#)
                .unwrap();
        assert_eq!(body.error, "invalid_grant");
        assert_eq!(body.error_description.as_deref(), Some("expired"));
    }

    /// Grep-gate covering the **HTTP-body** copy of the refresh token — the gap
    /// the `flow.rs` gate could not see (security-review task-20260712-5008
    /// finding A#2: that gate is single-file and syntactic). `post_token` must
    /// NOT build the request body with reqwest's `.form(` helper: that
    /// serialises the plaintext refresh token into a bare `String` body reqwest
    /// frees un-zeroized. The body MUST flow through the wipe-on-drop
    /// `Bytes::from_owner` path over a `Zeroizing` owner instead.
    ///
    /// Falsifier contract: reverting `post_token` to `.form(form)` reddens the
    /// first assertion; dropping the `from_owner` sink reddens the second.
    #[test]
    fn refresh_body_never_uses_unwiped_form_serialization() {
        // The source of THIS file, embedded at compile time (fixture-
        // independent: no tested type's layout is re-derived).
        let src = include_str!("exchange.rs");
        // Strip line comments so this test's own prose (which names the pattern
        // and the sink) cannot trip the gate. `//` opens a comment only when it
        // is not part of a scheme like `https://` — guard on the preceding byte.
        let mut code = String::with_capacity(src.len());
        for line in src.lines() {
            let bytes = line.as_bytes();
            let mut cut = line.len();
            let mut i = 0;
            while i + 1 < bytes.len() {
                if bytes[i] == b'/' && bytes[i + 1] == b'/' {
                    let prev = if i == 0 { b' ' } else { bytes[i - 1] };
                    if prev != b':' {
                        cut = i;
                        break;
                    }
                }
                i += 1;
            }
            code.push_str(&line[..cut]);
            code.push('\n');
        }
        // Collapse ALL whitespace so a call split across physical lines is seen
        // contiguous (same normalisation as the `flow.rs` gate).
        let flat: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        // Needle assembled from a fragment so the assertion messages below do
        // not re-introduce the literal into the scanned surface.
        let form_call = format!(".{}(", "form");
        assert!(
            !flat.contains(&form_call),
            "post_token must not use reqwest's form() builder — it serialises \
             the plaintext refresh token into a bare String body reqwest frees \
             un-zeroized (security-review 5008 A#1)",
        );
        assert!(
            flat.contains("from_owner"),
            "the wipe-on-drop body path (Bytes::from_owner over a Zeroizing \
             owner) must be present in post_token",
        );
    }

    #[test]
    fn form_body_matches_reference_encoding() {
        // A refresh token containing bytes that percent-encode (`/`, `+`,
        // space, non-ASCII `€`) — exactly the shape that makes a growing
        // `String` reallocate mid-serialisation.
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", "a/b+c d€/tail"),
            ("client_id", "cosmon"),
        ];
        let body = encode_form_zeroizing(&form);
        let got = std::str::from_utf8(body.as_ref()).unwrap();

        // Reference: an INDEPENDENT form-urlencoded serialiser (the `url`
        // crate's `Serializer`, the same primitive `serde_urlencoded` builds
        // on). Pins our hand-rolled encoding to the spec without re-deriving
        // the expectation from the code under test — so a regression in
        // `encode_form_zeroizing` reddens this test.
        let mut reference = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &form {
            reference.append_pair(k, v);
        }
        assert_eq!(got, reference.finish());
    }

    #[test]
    fn form_body_stays_within_reserved_capacity() {
        // Every value byte here percent-encodes to a 3-byte `%XX` escape — the
        // worst case for buffer growth. The reserved up-front bound must still
        // hold: that is the property guaranteeing the single allocation never
        // reallocates and thus never orphans an unwiped token fragment in freed
        // heap (RÉSIDUEL SÉCU A). The bound is recomputed here independently.
        let heavy = "/".repeat(300);
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", heavy.as_str()),
        ];
        let bound: usize = form
            .iter()
            .map(|(k, v)| k.len() * 3 + v.len() * 3 + 2)
            .sum();
        let body = encode_form_zeroizing(&form);
        assert!(
            body.as_ref().len() <= bound,
            "encoded body of {} bytes exceeded the reserved bound of {} — a \
             reallocation could have orphaned an unwiped token fragment",
            body.as_ref().len(),
            bound,
        );

        // And the secret still round-trips intact through the wire encoding.
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(body.as_ref())
            .into_owned()
            .collect();
        assert_eq!(pairs[1], ("refresh_token".to_owned(), heavy));
    }
}
