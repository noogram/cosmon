// SPDX-License-Identifier: AGPL-3.0-only

//! The `cs-oidc-mock` HTTP surface, as a library router.
//!
//! This module holds **the whole `IdP`** — the JWKS endpoint, the
//! side-channel `POST /issue` minter, and (since issue #53) the three
//! endpoints an OAuth 2.0 authorization-code client actually walks:
//! `/.well-known/openid-configuration`, `/authorize` and `/token`. The
//! `cs-oidc-mock` binary is a thin `main` around [`router`]; it adds a
//! `--bind` and a `--write-jwks-out`, and nothing else.
//!
//! # Why the router lives in the library
//!
//! Issue #53 needs an end-to-end login test in `cosmon-remote`, driven by
//! the *same* `IdP` the container smoke drives. Cargo only exports
//! `CARGO_BIN_EXE_<name>` to the **defining** crate's tests, so a
//! `cosmon-remote` test cannot locate the `cs-oidc-mock` executable by
//! path. Publishing the router instead gives every consumer the real
//! surface — same handlers, same claims, same PKCE check — served
//! in-process on a random port. A second copy of these handlers written
//! for the test would be exactly the fixture that passes while the
//! shipped binary drifts.
//!
//! # Auto-approval, and why there is no consent screen
//!
//! `GET /authorize` approves every well-formed request and redirects
//! immediately. A consent screen is a human gesture; this `IdP` exists to be
//! driven by `curl -L` from a shell script and by a test's spawned opener.
//! What is *not* relaxed is the security-relevant half of the flow: the
//! `code` is single-use, short-lived, bound to its `client_id` and
//! `redirect_uri`, and only redeemable by presenting the PKCE verifier
//! whose S256 digest matches the challenge presented at `/authorize`.
//!
//! **DEMO ONLY.** The signing key is the plaintext RSA test key committed
//! in this crate. Every token this module mints is trivially forgeable by
//! anyone with the source.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine as _;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    DEFAULT_AUDIENCE, DEFAULT_ISSUER, DEFAULT_KID, TEST_RSA_E_B64URL, TEST_RSA_N_B64URL,
    TEST_RSA_PRIVATE_PEM,
};

/// Default lifetime for JWTs when the caller does not ask for one.
pub const DEFAULT_LIFETIME_SEC: u64 = 600;

/// Default subject minted by `/authorize` when the request carries no
/// `login_hint`. A stand-in for "whoever sat at the consent screen".
pub const DEFAULT_SUBJECT: &str = "cs-oidc-mock-user";

/// How long an issued authorization code stays redeemable. Deliberately
/// short: a code is a one-shot bearer of the whole login, and a test that
/// leaks one must not be able to replay it a minute later.
pub const AUTH_CODE_TTL_SECS: i64 = 60;

/// Default scope set assigned to the issued JWT when neither `?scope=`
/// nor `?scopes=` is passed to `POST /issue`. Matches the V0 read-only
/// surface so a no-arg `POST /issue` produces a token sufficient for
/// `GET /v1/molecules/:id` without further configuration.
pub const DEFAULT_SCOPES: &str = "cosmon:molecule:read";

/// Everything the mock `IdP` needs to know before it serves a request.
#[derive(Clone, Debug)]
pub struct IdpConfig {
    /// `iss` claim emitted in every JWT, echoed by the discovery document,
    /// and pinned in the JWKS file. In an authorization-code deployment
    /// this MUST be the base URL the client can actually reach (that is
    /// where the client fetches `/.well-known/openid-configuration`).
    pub issuer: String,
    /// Audiences this `IdP` will mint for. `/issue` refuses an `aud` outside
    /// the list; `/authorize` refuses a `client_id` outside it, because
    /// cosmon pins `aud == client_id`.
    pub audiences: Vec<String>,
    /// `kid` advertised in the JWKS and stamped into every JWT header.
    pub kid: String,
    /// Token lifetime, in seconds, when the caller asks for none.
    pub default_lifetime_secs: u64,
    /// The `sub` `/authorize` mints for, unless the request carries a
    /// `login_hint`.
    pub subject: String,
    /// **Test-only sabotage seam.** When set, `/authorize` stores a
    /// deliberately wrong `code_challenge`, so the matching verifier
    /// presented at `/token` cannot possibly validate.
    ///
    /// It exists so a client-side test can show its PKCE check RED: a
    /// login that fails *only* because the challenge was corrupted proves
    /// the verifier is load-bearing, which a passing login alone never
    /// does. Never reachable from the `cs-oidc-mock` command line — a
    /// consumer must opt in from Rust, in a test.
    pub corrupt_code_challenge: bool,
}

impl Default for IdpConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_owned(),
            audiences: vec![DEFAULT_AUDIENCE.to_owned()],
            kid: DEFAULT_KID.to_owned(),
            default_lifetime_secs: DEFAULT_LIFETIME_SEC,
            subject: DEFAULT_SUBJECT.to_owned(),
            corrupt_code_challenge: false,
        }
    }
}

/// One issued, not-yet-redeemed authorization code.
#[derive(Clone, Debug)]
struct AuthCode {
    /// The client the code was issued to — `/token` refuses a redemption
    /// presenting a different one.
    client_id: String,
    /// The `redirect_uri` presented at `/authorize`, matched byte-for-byte
    /// at redemption (RFC 6749 §4.1.3).
    redirect_uri: String,
    /// The base64url-nopad S256 digest of the client's PKCE verifier.
    code_challenge: String,
    /// The scopes requested, carried onto the minted tokens.
    scopes: Vec<String>,
    /// The subject the code was minted for.
    sub: String,
    /// Unix seconds after which the code is dead (see [`AUTH_CODE_TTL_SECS`]).
    expires_at: i64,
}

/// The router's shared state: immutable configuration, the signing key,
/// and the live authorization-code table.
#[derive(Clone)]
struct AppState {
    config: IdpConfig,
    encoding_key: Arc<EncodingKey>,
    /// Codes issued by `/authorize` and not yet redeemed. A redemption
    /// *removes* the entry, which is what makes the code single-use: a
    /// replay finds nothing and is refused `invalid_grant`.
    codes: Arc<Mutex<HashMap<String, AuthCode>>>,
}

impl AppState {
    fn default_audience(&self) -> &str {
        self.config
            .audiences
            .first()
            .map_or(DEFAULT_AUDIENCE, String::as_str)
    }

    /// The issuer with any trailing slash removed, so URL concatenation
    /// never produces a double slash a byte-comparing client would reject.
    fn issuer_base(&self) -> &str {
        self.config.issuer.trim_end_matches('/')
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .field("encoding_key", &"<opaque>")
            // Codes are live bearer material for a whole login; count them
            // rather than print them, so a `?state` in a trace cannot leak
            // one into a log the way the key would.
            .field(
                "codes",
                &self.codes.lock().map(|c| c.len()).unwrap_or_default(),
            )
            .finish()
    }
}

/// Build the mock `IdP`'s axum router.
///
/// Routes, in the order a client meets them:
///
/// - `GET /.well-known/openid-configuration` — RFC 8414 discovery, S256 only.
/// - `GET /authorize` — auto-approves and 302s to `redirect_uri` with
///   `code` + the caller's `state`.
/// - `POST /token` — redeems the code against its PKCE verifier.
/// - `GET /jwks.json`, `GET /jwks` — the public key, RFC 7517 shape.
/// - `POST /issue` — the side-channel minter (no OAuth dance).
/// - `GET /healthz` — liveness.
///
/// # Errors
///
/// Fails if the embedded RSA test key cannot be parsed as a PEM private
/// key, which would mean the crate's own asset is corrupt.
pub fn router(config: IdpConfig) -> anyhow::Result<Router> {
    let encoding_key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes())
        .map_err(|e| anyhow::anyhow!("embedded RSA test key is malformed: {e}"))?;
    let state = AppState {
        config,
        encoding_key: Arc::new(encoding_key),
        codes: Arc::new(Mutex::new(HashMap::new())),
    };
    Ok(Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(get_openid_configuration),
        )
        .route("/authorize", get(get_authorize))
        .route("/token", post(post_token))
        .route("/jwks.json", get(get_jwks))
        .route("/jwks", get(get_jwks))
        .route("/issue", post(post_issue))
        .route("/healthz", get(get_healthz))
        .with_state(state))
}

/// A mock `IdP` running on a random loopback port, for a consumer's
/// end-to-end test.
///
/// Dropping it aborts the server task and releases the port.
#[derive(Debug)]
pub struct MockIdp {
    base_url: String,
    server: tokio::task::JoinHandle<()>,
}

impl MockIdp {
    /// Bind `127.0.0.1:0`, then build the configuration **from the address
    /// that bind produced** and serve [`router`] on it.
    ///
    /// The callback shape is not ceremony. A login client fetches
    /// `<issuer>/.well-known/openid-configuration`, so the issuer must be
    /// the URL it can actually reach — which is unknown until the socket
    /// is bound. Handing the base URL to the caller is the only way to
    /// build a coherent config without a second, racy "guess a port" step.
    ///
    /// # Errors
    ///
    /// Fails if the loopback socket cannot be bound or the router cannot
    /// be built.
    pub async fn spawn(make_config: impl FnOnce(&str) -> IdpConfig) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let app = router(make_config(&base_url))?;
        let server = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                tracing::trace!(?err, "MockIdp axum::serve exited");
            }
        });
        Ok(Self { base_url, server })
    }

    /// The base URL the `IdP` is reachable at — the value that was handed to
    /// `make_config`, and therefore the issuer of every token it mints.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for MockIdp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

// --- liveness + key material --------------------------------------------

async fn get_healthz() -> &'static str {
    "ok"
}

async fn get_jwks(State(state): State<AppState>) -> Json<Value> {
    Json(jwks_body(&state.config.kid))
}

// --- discovery -----------------------------------------------------------

/// The RFC 8414 / OIDC Discovery document.
///
/// Only `S256` appears in `code_challenge_methods_supported`: `plain` is a
/// PKCE downgrade and this `IdP` has no reason to offer one.
fn discovery_document(state: &AppState) -> Value {
    let base = state.issuer_base();
    json!({
        "issuer": state.config.issuer,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/jwks.json"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": ["openid", DEFAULT_SCOPES],
    })
}

async fn get_openid_configuration(State(state): State<AppState>) -> Json<Value> {
    Json(discovery_document(&state))
}

// --- authorize -----------------------------------------------------------

/// The `GET /authorize` query, RFC 6749 §4.1.1 + RFC 7636 §4.3.
#[derive(Debug, Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    scope: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    /// OIDC Core §3.1.2.1 — the subject the caller asks to be signed in
    /// as. Honoured verbatim: this `IdP` has no user database to check it
    /// against, and a test that wants a specific `sub` should get it.
    login_hint: Option<String>,
}

/// An `/authorize` refusal. Kept as a plain `400` with a readable body
/// rather than an error redirect: a malformed request has no trustworthy
/// `redirect_uri` to send the operator back to, and a mock that redirects
/// a bad request hides the mistake in a browser history.
fn authorize_refusal(reason: String) -> Response {
    (StatusCode::BAD_REQUEST, reason).into_response()
}

async fn get_authorize(State(state): State<AppState>, Query(q): Query<AuthorizeQuery>) -> Response {
    let Some(client_id) = q.client_id.filter(|s| !s.is_empty()) else {
        return authorize_refusal("`client_id` is required".to_owned());
    };
    let Some(redirect_uri) = q.redirect_uri.filter(|s| !s.is_empty()) else {
        return authorize_refusal("`redirect_uri` is required".to_owned());
    };
    if q.response_type.as_deref() != Some("code") {
        return authorize_refusal(format!(
            "unsupported response_type {:?} — this IdP serves the authorization-code flow only",
            q.response_type.unwrap_or_default()
        ));
    }
    // cosmon pins `aud == client_id`, so an unknown client_id is an
    // unknown audience: refuse rather than mint a token nobody accepts.
    if !state.config.audiences.iter().any(|a| a == &client_id) {
        return authorize_refusal(format!(
            "client_id `{client_id}` is not in the configured audience allow-list \
             — pass --audience to cs-oidc-mock"
        ));
    }
    if q.code_challenge_method.as_deref() != Some("S256") {
        return authorize_refusal(format!(
            "code_challenge_method must be S256, got {:?}",
            q.code_challenge_method.unwrap_or_default()
        ));
    }
    let Some(code_challenge) = q.code_challenge.filter(|s| !s.is_empty()) else {
        return authorize_refusal("`code_challenge` is required (PKCE is mandatory)".to_owned());
    };

    let code = random_token("code");
    let entry = AuthCode {
        client_id,
        redirect_uri: redirect_uri.clone(),
        code_challenge: if state.config.corrupt_code_challenge {
            format!("{code_challenge}-corrupted")
        } else {
            code_challenge
        },
        scopes: split_space_separated(q.scope.as_deref()),
        sub: q
            .login_hint
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| state.config.subject.clone()),
        expires_at: chrono::Utc::now().timestamp() + AUTH_CODE_TTL_SECS,
    };
    match state.codes.lock() {
        Ok(mut codes) => {
            codes.insert(code.clone(), entry);
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "authorization-code table is poisoned",
            )
                .into_response()
        }
    }

    // Auto-approve: no consent screen, straight back to the client. The
    // `state` is echoed verbatim (RFC 6749 §4.1.2) — the client's CSRF
    // check is the client's, and swallowing it here would silently
    // disarm it.
    let location = match append_query(&redirect_uri, &[("code", &code)], q.state.as_deref()) {
        Ok(url) => url,
        Err(reason) => return authorize_refusal(reason),
    };
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

/// Append `pairs` (and `state`, when present) to `base`'s query string.
///
/// Built by hand rather than with a URL crate: the testkit has no `url`
/// dependency, and the only inputs are our own generated code plus the
/// client's `state`, both percent-encoded here.
fn append_query(
    base: &str,
    pairs: &[(&str, &str)],
    state: Option<&str>,
) -> std::result::Result<String, String> {
    if base.contains('#') {
        return Err(format!(
            "redirect_uri {base:?} carries a fragment, which RFC 6749 §3.1.2 forbids"
        ));
    }
    let mut out = String::from(base);
    let mut sep = if base.contains('?') { '&' } else { '?' };
    for (k, v) in pairs {
        out.push(sep);
        out.push_str(k);
        out.push('=');
        out.push_str(&percent_encode(v));
        sep = '&';
    }
    if let Some(state) = state {
        out.push(sep);
        out.push_str("state=");
        out.push_str(&percent_encode(state));
    }
    Ok(out)
}

/// Percent-encode everything outside the RFC 3986 unreserved set. Blunt on
/// purpose — over-encoding is always valid, under-encoding is not.
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            // `write!` into a String is infallible; the Result is discarded
            // rather than unwrapped (no `unwrap` in library code).
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A random, URL-safe opaque token with a readable prefix, so a code seen
/// in a log is recognisable as one.
fn random_token(prefix: &str) -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!(
        "{prefix}-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// Split an OIDC space-separated `scope` parameter, dropping empties.
fn split_space_separated(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(' ')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

// --- token ---------------------------------------------------------------

/// The `POST /token` form body (RFC 6749 §4.1.3 + RFC 7636 §4.5).
#[derive(Debug, Deserialize)]
///
/// Every field is `Option` on the wire so a missing one is a named
/// `invalid_request` refusal rather than a bare 422 from the form
/// extractor; `post_token` then requires all five.
struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    code_verifier: Option<String>,
    /// REQUIRED (RFC 6749 §4.1.3) — `/authorize` never issues a code
    /// without one, so redemption always has one to present.
    redirect_uri: Option<String>,
    /// REQUIRED — the code is bound to the client it was issued to.
    client_id: Option<String>,
}

/// An RFC 6749 §5.2 error body. The `error` code is what a client
/// switches on (cosmon-remote reads `invalid_grant` to tell a dead code
/// from a broken endpoint), so it is never folded into the description.
fn token_error(status: StatusCode, code: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": code, "error_description": description })),
    )
        .into_response()
}

/// The four parameters a redemption must carry, all present and
/// non-empty. Constructing one is the entire required-parameter
/// contract of `POST /token`; nothing downstream re-checks presence.
struct Redemption {
    code: String,
    verifier: String,
    client_id: String,
    redirect_uri: String,
}

/// A named RFC 6749 §5.2 refusal, before it becomes a `Response`.
/// Small by construction — an `axum` `Response` in an `Err` variant is
/// 128+ bytes and pays that on every success too.
struct TokenRefusal {
    error: &'static str,
    description: &'static str,
}

impl TokenRefusal {
    /// Render the refusal as the wire body a client switches on.
    fn into_response(self) -> Response {
        token_error(StatusCode::BAD_REQUEST, self.error, self.description)
    }
}

/// Validate a [`TokenForm`] into a [`Redemption`], or the refusal naming
/// the first parameter that is missing.
///
/// `redirect_uri` and `client_id` are REQUIRED, not checked-when-present.
/// RFC 6749 §4.1.3 makes `redirect_uri` required at redemption whenever
/// one was sent at authorization — and `/authorize` here refuses a
/// request without it, so it always was; `client_id` is required for the
/// same reason, since the code is bound to one. Checking them only when
/// present made that binding opt-in: a client that simply omitted the
/// field skipped the check, and this module's claim that a code is
/// "bound to its `client_id` and `redirect_uri`" held only for
/// well-behaved clients. An absent parameter is `invalid_request`
/// (malformed), which is a different failure from the present-but-wrong
/// mismatches `post_token` refuses afterwards.
fn require_redemption(form: TokenForm) -> Result<Redemption, TokenRefusal> {
    if form.grant_type.as_deref() != Some("authorization_code") {
        return Err(TokenRefusal {
            error: "unsupported_grant_type",
            description: "this IdP serves `authorization_code` only",
        });
    }
    let required = |value: Option<String>, description: &'static str| {
        value.filter(|s| !s.is_empty()).ok_or(TokenRefusal {
            error: "invalid_request",
            description,
        })
    };
    Ok(Redemption {
        code: required(form.code, "`code` is required")?,
        verifier: required(
            form.code_verifier,
            "`code_verifier` is required (PKCE is mandatory)",
        )?,
        client_id: required(form.client_id, "`client_id` is required")?,
        redirect_uri: required(
            form.redirect_uri,
            "`redirect_uri` is required (RFC 6749 §4.1.3: it was sent at /authorize)",
        )?,
    })
}

async fn post_token(State(state): State<AppState>, Form(form): Form<TokenForm>) -> Response {
    let Redemption {
        code,
        verifier,
        client_id,
        redirect_uri,
    } = match require_redemption(form) {
        Ok(r) => r,
        Err(refusal) => return refusal.into_response(),
    };

    // Take the entry out under the lock. Removal *is* the single-use
    // guarantee: every later outcome — mismatch, expiry, success — leaves
    // the table without this code, so a replay is refused identically to
    // a forged one.
    let entry = match state.codes.lock() {
        Ok(mut codes) => codes.remove(&code),
        Err(_) => {
            return token_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "authorization-code table is poisoned",
            )
        }
    };
    let Some(entry) = entry else {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "authorization code is unknown, already redeemed, or expired",
        );
    };
    if chrono::Utc::now().timestamp() > entry.expires_at {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "authorization code has expired",
        );
    }
    if client_id != entry.client_id {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "client_id does not match the one the code was issued to",
        );
    }
    if redirect_uri != entry.redirect_uri {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "redirect_uri does not match the one presented at /authorize",
        );
    }
    if s256_challenge(&verifier) != entry.code_challenge {
        return token_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "PKCE verification failed: S256(code_verifier) != code_challenge",
        );
    }

    let lifetime = state.config.default_lifetime_secs;
    let now_s = chrono::Utc::now().timestamp();
    let exp = now_s + i64::try_from(lifetime).unwrap_or(0);
    let claims = json!({
        "iss": state.config.issuer,
        "sub": entry.sub,
        "aud": entry.client_id,
        "iat": now_s,
        "exp": exp,
        "jti": random_token("jti"),
        "scopes": entry.scopes,
    });
    let token = match sign(&state, &claims) {
        Ok(t) => t,
        Err(e) => return token_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e),
    };
    // Both tokens carry the same full claim set. A real Forgejo mints a
    // claim-less bookkeeping `access_token` and puts the identity in the
    // `id_token`; cosmon-remote selects whichever token actually carries
    // the claims, so an IdP that puts them in both exercises the
    // id_token-preferred path without the client having to guess.
    Json(json!({
        "access_token": token,
        "id_token": token,
        "token_type": "Bearer",
        "expires_in": lifetime,
        "scope": entry.scopes.join(" "),
    }))
    .into_response()
}

/// The RFC 7636 §4.2 S256 challenge: base64url-nopad(SHA-256(verifier)).
fn s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn sign(state: &AppState, claims: &Value) -> std::result::Result<String, String> {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(state.config.kid.clone());
    encode(&header, claims, &state.encoding_key).map_err(|e| format!("encode: {e}"))
}

// --- issue (side channel) ------------------------------------------------

/// The `POST /issue` query — the side-channel minter, unchanged since V0.
#[derive(Debug, Deserialize)]
struct IssueQuery {
    /// Subject — the `sub` claim. Required.
    sub: Option<String>,
    /// Optional audience override (defaults to the configured `--audience`).
    aud: Option<String>,
    /// Cosmon historical plural — comma-separated scopes.
    scopes: Option<String>,
    /// RFC 8693 / OIDC singular — space-separated scopes. Wins over
    /// `scopes` when both are present.
    scope: Option<String>,
    /// Token lifetime in seconds.
    lifetime: Option<u64>,
    /// Optional `jti` override.
    jti: Option<String>,
}

/// Parse the scope set from the two accepted spellings.
///
/// Precedence: `scope` (OIDC-spec singular, space-separated) wins over
/// `scopes` (cosmon plural, comma-separated). When both are absent, fall
/// back to [`DEFAULT_SCOPES`]. Empty / whitespace-only segments are
/// dropped so callers can pass `"a, b , ,c"` without issuing the empty
/// scope.
fn parse_scopes(scope: Option<&str>, scopes: Option<&str>) -> Vec<String> {
    if let Some(s) = scope {
        return split_space_separated(Some(s));
    }
    let raw = scopes.unwrap_or(DEFAULT_SCOPES);
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

async fn post_issue(
    State(state): State<AppState>,
    Query(q): Query<IssueQuery>,
) -> std::result::Result<Json<Value>, (StatusCode, String)> {
    let sub = q.sub.ok_or((
        StatusCode::BAD_REQUEST,
        "`sub` query parameter is required".to_owned(),
    ))?;
    let aud = q.aud.unwrap_or_else(|| state.default_audience().to_owned());
    if !state.config.audiences.iter().any(|a| a == &aud) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "audience `{aud}` is not in the configured allow-list — pass --audience to cs-oidc-mock"
            ),
        ));
    }
    let scopes = parse_scopes(q.scope.as_deref(), q.scopes.as_deref());
    let lifetime = q.lifetime.unwrap_or(state.config.default_lifetime_secs);
    let now_s = chrono::Utc::now().timestamp();
    let jti = q.jti.unwrap_or_else(|| format!("jti-{now_s}-{sub}"));

    let claims = json!({
        "iss": state.config.issuer,
        "sub": sub,
        "aud": aud,
        "iat": now_s,
        "exp": now_s + i64::try_from(lifetime).unwrap_or(0),
        "jti": jti,
        "scopes": scopes,
    });
    let token = sign(&state, &claims).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": lifetime,
        "jti": jti,
        "iss": state.config.issuer,
        "aud": aud,
        "scopes": scopes,
    })))
}

// --- key material projections -------------------------------------------

/// The single JWK record for the embedded RSA-2048 test key, in RFC 7517
/// shape.
///
/// **This is the one place the key fields live.** Both the live `/jwks`
/// response ([`jwks_body`]) and the disk projection ([`write_jwks_file`])
/// wrap this record; they differ *only* in their envelope (the disk file
/// adds `iss` / `audiences`), never in the key itself. Before this
/// collapse the two builders had already drifted — the wire emitted
/// `"kty": "RSA"` and the disk file emitted `"kty": "rsa"`. RFC 7518 §6.1
/// is case-sensitive, so a strict relying party accepts the former and
/// rejects the latter. Writing `"RSA"` exactly once makes that drift
/// unrepresentable.
#[must_use]
pub fn jwk_record(kid: &str) -> Value {
    json!({
        "kid": kid,
        "alg": "RS256",
        // RFC 7518 §6.1 — case-sensitive. MUST be uppercase `RSA`.
        "kty": "RSA",
        "use": "sig",
        "n": TEST_RSA_N_B64URL,
        "e": TEST_RSA_E_B64URL,
    })
}

/// The body served at `GET /jwks` and `GET /jwks.json`.
#[must_use]
pub fn jwks_body(kid: &str) -> Value {
    json!({ "keys": [jwk_record(kid)] })
}

/// Project the key material to disk in the format
/// `cosmon-rpp-adapter::JwksStore::load` expects: the same key record as
/// the wire, wrapped in an envelope that also pins `iss` and the accepted
/// audiences.
///
/// # Errors
///
/// Propagates any filesystem failure creating the parent directory or
/// writing the file.
pub fn write_jwks_file(
    path: &Path,
    issuer: &str,
    audiences: &[String],
    kid: &str,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = json!({
        "iss": issuer,
        "audiences": audiences,
        "keys": [jwk_record(kid)],
    });
    std::fs::write(path, serde_json::to_vec_pretty(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    const TEST_CLIENT: &str = "client-under-test";

    fn test_state() -> IdpConfig {
        IdpConfig {
            issuer: "http://idp.test".to_owned(),
            audiences: vec![TEST_CLIENT.to_owned()],
            ..IdpConfig::default()
        }
    }

    async fn call(router: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>, Option<String>) {
        let resp = router
            .clone()
            .oneshot(req)
            .await
            .expect("router is infallible");
        let status = resp.status();
        let location = resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec();
        (status, bytes, location)
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("build GET")
    }

    fn post_form(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .expect("build POST")
    }

    /// The verifier / challenge pair every authorize→token test drives.
    fn pkce_pair() -> (String, String) {
        let verifier = "a".repeat(64);
        let challenge = s256_challenge(&verifier);
        (verifier, challenge)
    }

    fn authorize_uri(challenge: &str, state: &str) -> String {
        format!(
            "/authorize?response_type=code&client_id={TEST_CLIENT}\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A8123%2Fcallback\
             &scope=openid&state={state}&code_challenge={challenge}\
             &code_challenge_method=S256"
        )
    }

    /// The `redirect_uri` [`authorize_uri`] asks for, form-encoded — the
    /// same bytes `/token` matches against.
    const TEST_REDIRECT: &str = "http%3A%2F%2F127.0.0.1%3A8123%2Fcallback";

    /// A complete, well-formed `POST /token` body. Every required field
    /// is present, so a test that wants to probe ONE of them removes or
    /// corrupts exactly that field and nothing else refuses first.
    fn token_body(code: &str, verifier: &str) -> String {
        format!(
            "grant_type=authorization_code&code={code}&code_verifier={verifier}\
             &client_id={TEST_CLIENT}&redirect_uri={TEST_REDIRECT}"
        )
    }

    /// Drive `/authorize` and return the issued code.
    async fn issue_code(router: &axum::Router, challenge: &str) -> String {
        let (_, _, location) = call(router, get(&authorize_uri(challenge, "s"))).await;
        code_from(&location.expect("Location"))
    }

    /// Pull `code=` out of an `/authorize` `Location` header.
    fn code_from(location: &str) -> String {
        location
            .split(['?', '&'])
            .find_map(|p| p.strip_prefix("code="))
            .expect("Location carries a code")
            .to_owned()
    }

    #[tokio::test]
    async fn the_discovery_document_advertises_the_three_endpoints_and_s256_only() {
        let router = router(test_state()).expect("router");
        let (status, body, _) = call(&router, get("/.well-known/openid-configuration")).await;
        assert_eq!(status, StatusCode::OK);
        let doc: Value = serde_json::from_slice(&body).expect("JSON discovery document");
        assert_eq!(doc["issuer"], "http://idp.test");
        assert_eq!(doc["authorization_endpoint"], "http://idp.test/authorize");
        assert_eq!(doc["token_endpoint"], "http://idp.test/token");
        assert_eq!(doc["jwks_uri"], "http://idp.test/jwks.json");
        // `plain` is a PKCE downgrade; advertising it would let a client
        // negotiate the check away.
        assert_eq!(doc["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(doc["response_types_supported"], json!(["code"]));
    }

    #[tokio::test]
    async fn a_trailing_slash_on_the_issuer_never_doubles_in_an_endpoint_url() {
        // A client matches `redirect_uri` and endpoints byte-for-byte; a
        // `//authorize` would be a different URL than the one it dials.
        let cfg = IdpConfig {
            issuer: "http://idp.test/".to_owned(),
            ..test_state()
        };
        let router = router(cfg).expect("router");
        let (_, body, _) = call(&router, get("/.well-known/openid-configuration")).await;
        let doc: Value = serde_json::from_slice(&body).expect("JSON");
        assert_eq!(doc["authorization_endpoint"], "http://idp.test/authorize");
    }

    #[tokio::test]
    async fn authorize_auto_approves_and_echoes_the_state_verbatim() {
        let router = router(test_state()).expect("router");
        let (_, challenge) = pkce_pair();
        let (status, _, location) =
            call(&router, get(&authorize_uri(&challenge, "xyz-state-123"))).await;
        assert_eq!(status, StatusCode::FOUND, "auto-approval is a 302");
        let location = location.expect("302 carries a Location");
        assert!(
            location.starts_with("http://127.0.0.1:8123/callback?"),
            "the redirect goes to the requested redirect_uri, got {location}"
        );
        assert!(
            location.contains("state=xyz-state-123"),
            "the caller's CSRF state must come back verbatim, got {location}"
        );
        assert!(location.contains("code="), "got {location}");
    }

    #[tokio::test]
    async fn a_code_is_single_use() {
        let router = router(test_state()).expect("router");
        let (verifier, challenge) = pkce_pair();
        let (_, _, location) = call(&router, get(&authorize_uri(&challenge, "s"))).await;
        let code = code_from(&location.expect("Location"));
        let body = token_body(&code, &verifier);

        let (first, _, _) = call(&router, post_form("/token", &body)).await;
        assert_eq!(first, StatusCode::OK, "the first redemption succeeds");

        let (second, second_body, _) = call(&router, post_form("/token", &body)).await;
        assert_eq!(
            second,
            StatusCode::BAD_REQUEST,
            "replaying a redeemed code must fail"
        );
        let err: Value = serde_json::from_slice(&second_body).expect("RFC 6749 error body");
        assert_eq!(err["error"], "invalid_grant");
    }

    #[tokio::test]
    async fn a_mismatched_pkce_verifier_is_refused_and_mints_nothing() {
        let router = router(test_state()).expect("router");
        let (_, challenge) = pkce_pair();
        let (_, _, location) = call(&router, get(&authorize_uri(&challenge, "s"))).await;
        let code = code_from(&location.expect("Location"));
        let wrong = "b".repeat(64);
        let (status, body, _) =
            call(&router, post_form("/token", &token_body(&code, &wrong))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: Value = serde_json::from_slice(&body).expect("RFC 6749 error body");
        assert_eq!(err["error"], "invalid_grant");
        assert!(
            err.get("access_token").is_none() && err.get("id_token").is_none(),
            "a refused exchange must mint no token, got {err}"
        );
    }

    #[tokio::test]
    async fn a_redeemed_code_yields_signed_tokens_carrying_the_requested_identity() {
        let router = router(test_state()).expect("router");
        let (verifier, challenge) = pkce_pair();
        let uri = format!("{}&login_hint=operator-7", authorize_uri(&challenge, "s"));
        let (_, _, location) = call(&router, get(&uri)).await;
        let code = code_from(&location.expect("Location"));
        let (status, body, _) = call(
            &router,
            post_form(
                "/token",
                &format!(
                    "grant_type=authorization_code&code={code}&code_verifier={verifier}\
                     &client_id={TEST_CLIENT}&redirect_uri=http%3A%2F%2F127.0.0.1%3A8123%2Fcallback"
                ),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body)
        );
        let tokens: Value = serde_json::from_slice(&body).expect("token response");
        assert_eq!(tokens["token_type"], "Bearer");
        let id_token = tokens["id_token"].as_str().expect("id_token is a string");
        let claims = crate::verify_signed_claims(id_token, "http://idp.test", TEST_CLIENT)
            .expect("the id_token verifies against the embedded public key");
        assert_eq!(claims["sub"], "operator-7", "login_hint is honoured");
        assert_eq!(claims["scopes"], json!(["openid"]));
    }

    #[tokio::test]
    async fn a_redirect_uri_that_disagrees_with_authorize_is_refused() {
        let router = router(test_state()).expect("router");
        let (verifier, challenge) = pkce_pair();
        let (_, _, location) = call(&router, get(&authorize_uri(&challenge, "s"))).await;
        let code = code_from(&location.expect("Location"));
        let (status, body, _) = call(
            &router,
            post_form(
                "/token",
                &format!(
                    "grant_type=authorization_code&code={code}&code_verifier={verifier}\
                     &client_id={TEST_CLIENT}&redirect_uri=http%3A%2F%2Fevil.test%2Fcallback"
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: Value = serde_json::from_slice(&body).expect("error body");
        assert_eq!(err["error"], "invalid_grant");
    }

    #[tokio::test]
    async fn a_token_request_without_redirect_uri_is_refused() {
        // RFC 6749 §4.1.3: `redirect_uri` is REQUIRED at redemption when
        // one was sent at authorization — and `/authorize` refuses a
        // request that omits it, so it always was. Checking the field
        // only when present made the byte-for-byte binding skippable by
        // simply not sending it.
        let router = router(test_state()).expect("router");
        let (verifier, challenge) = pkce_pair();
        let code = issue_code(&router, &challenge).await;
        let body =
            token_body(&code, &verifier).replace(&format!("&redirect_uri={TEST_REDIRECT}"), "");
        let (status, body, _) = call(&router, post_form("/token", &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: Value = serde_json::from_slice(&body).expect("RFC 6749 error body");
        assert_eq!(
            err["error"], "invalid_request",
            "an absent required parameter is `invalid_request`, not a mismatch"
        );
    }

    #[tokio::test]
    async fn a_token_request_without_client_id_is_refused() {
        // The code is bound to the client it was issued to; a redemption
        // that names no client cannot be checked against that binding,
        // so it is malformed rather than merely unlucky.
        let router = router(test_state()).expect("router");
        let (verifier, challenge) = pkce_pair();
        let code = issue_code(&router, &challenge).await;
        let body = token_body(&code, &verifier).replace(&format!("&client_id={TEST_CLIENT}"), "");
        let (status, body, _) = call(&router, post_form("/token", &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let err: Value = serde_json::from_slice(&body).expect("RFC 6749 error body");
        assert_eq!(err["error"], "invalid_request");
    }

    #[tokio::test]
    async fn authorize_refuses_plain_pkce_and_an_unknown_client() {
        let router = router(test_state()).expect("router");
        let (_, challenge) = pkce_pair();
        let plain = authorize_uri(&challenge, "s").replace("S256", "plain");
        let (status, _, _) = call(&router, get(&plain)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "plain PKCE is a downgrade");

        let stranger = authorize_uri(&challenge, "s").replace(TEST_CLIENT, "not-provisioned");
        let (status, _, _) = call(&router, get(&stranger)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_corrupt_challenge_seam_makes_every_verifier_wrong() {
        // The sabotage knob the client-side falsifier depends on: with it
        // set, the *correct* verifier is refused. If this ever passed, a
        // green login would prove nothing about the PKCE check.
        let cfg = IdpConfig {
            corrupt_code_challenge: true,
            ..test_state()
        };
        let router = router(cfg).expect("router");
        let (verifier, challenge) = pkce_pair();
        let (_, _, location) = call(&router, get(&authorize_uri(&challenge, "s"))).await;
        let code = code_from(&location.expect("Location"));
        let (status, _, _) =
            call(&router, post_form("/token", &token_body(&code, &verifier))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_v0_endpoints_are_unchanged() {
        // Issue #53 added endpoints; it must not have moved any.
        let router = router(test_state()).expect("router");
        let (status, body, _) = call(&router, get("/healthz")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"ok");

        for path in ["/jwks", "/jwks.json"] {
            let (status, body, _) = call(&router, get(path)).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            let doc: Value = serde_json::from_slice(&body).expect("JWKS");
            assert_eq!(doc, jwks_body(DEFAULT_KID), "{path}");
        }

        let (status, body, _) = call(
            &router,
            Request::builder()
                .method("POST")
                .uri(format!("/issue?sub=n1&aud={TEST_CLIENT}"))
                .body(Body::empty())
                .expect("build POST /issue"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let doc: Value = serde_json::from_slice(&body).expect("issue response");
        assert_eq!(doc["token_type"], "Bearer");
        assert_eq!(doc["aud"], TEST_CLIENT);
        assert_eq!(doc["scopes"], json!(["cosmon:molecule:read"]));
    }

    #[test]
    fn parse_scopes_plural_csv_matches_singular_space_separated() {
        let plural = parse_scopes(None, Some("cosmon:molecule:read,cosmon:molecule:write"));
        let singular = parse_scopes(Some("cosmon:molecule:read cosmon:molecule:write"), None);
        assert_eq!(plural, singular);
        assert_eq!(
            plural,
            vec![
                "cosmon:molecule:read".to_owned(),
                "cosmon:molecule:write".to_owned()
            ]
        );
    }

    #[test]
    fn parse_scopes_singular_wins_over_plural_when_both_present() {
        // OIDC-spec spelling is canonical when both are given.
        let scopes = parse_scopes(Some("a b"), Some("c,d"));
        assert_eq!(scopes, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn parse_scopes_default_when_neither_param_present() {
        let scopes = parse_scopes(None, None);
        assert_eq!(scopes, vec!["cosmon:molecule:read".to_owned()]);
    }

    #[test]
    fn jwk_record_emits_uppercase_rsa_kty() {
        // RFC 7518 §6.1 is case-sensitive; a strict relying party
        // rejects `"rsa"`. The single constructor must always say `RSA`.
        assert_eq!(jwk_record("kid-1")["kty"], "RSA");
    }

    #[test]
    fn wire_and_disk_jwks_carry_an_identical_key_record() {
        // The whole point of the collapse: the live `/jwks` response and
        // the disk projection differ only in their envelope, never in the
        // key record. Drift between `RSA`/`rsa` is now unrepresentable.
        let wire = jwks_body("kid-1");
        let dir = std::env::temp_dir().join(format!("cs-oidc-mock-test-{}", std::process::id()));
        let path = dir.join("oidc-mock.json");
        write_jwks_file(&path, "http://idp.test", &["aud-1".to_owned()], "kid-1")
            .expect("write jwks file");
        let disk: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read jwks file")).expect("parse");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(wire["keys"][0], disk["keys"][0]);
        // …and the disk envelope carries the extra fields the wire omits.
        assert_eq!(disk["iss"], "http://idp.test");
        assert_eq!(disk["audiences"][0], "aud-1");
        assert!(wire.get("iss").is_none());
    }

    #[test]
    fn parse_scopes_drops_empty_segments_and_trims_whitespace() {
        // Plural CSV: stray comma + spaces.
        let plural = parse_scopes(None, Some("a, b , ,c"));
        assert_eq!(plural, vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        // Singular SSV: double space.
        let singular = parse_scopes(Some("a  b   c"), None);
        assert_eq!(
            singular,
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn s256_challenge_matches_the_rfc_7636_appendix_b_vector() {
        // RFC 7636 Appendix B pins verifier → challenge. Getting this
        // wrong would make the mock refuse every real client while its
        // own round-trip test still passed.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
