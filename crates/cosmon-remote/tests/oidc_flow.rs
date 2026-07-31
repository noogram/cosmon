// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for the OAuth2-PKCE login + silent-refresh flow
//! (delib-20260710-33b7 C9, Child 2).
//!
//! These exercise the seams that cannot live in a `#[cfg(test)]` unit block:
//! the HTTP round-trips (discovery, code exchange, refresh) against a
//! `wiremock` `OidcMock` that **enforces single-use refresh rotation**, and the
//! two invariants CI's single-process / single-audience shape structurally
//! cannot generate by accident:
//!
//! - **concurrent-refresh single-flight** — N parallel refreshers → exactly one
//!   network refresh, all converge to the same fresh token;
//! - **negative audience** — a token minted for audience A is never returned for
//!   audience B (the isolation is proved by *absence*, not by acceptance);
//! - **the `openid` ⇄ `id_token` coupling** (issue #27) — against a provider that
//!   mints an `id_token` *only* for `openid`, the bearer carries OIDC identity
//!   claims after login **and** after refresh. This one needs an authorization
//!   endpoint that observes the requested scope, which is why it lives here and
//!   not in `cs-oidc-mock` (see [`OpenidGatingProvider`]).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use chrono::{Duration as ChronoDuration, Utc};
use cosmon_remote::credential::{CredentialKey, CredentialStore, SecretToken, StoredCredential};
use cosmon_remote::oidc::{self, TokenState};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// --- JWT helpers ---------------------------------------------------------
//
// The client now *decodes* every bearer candidate and selects on the identity
// claims it carries (round-1 finding 2), so the mocks must mint real
// (base64url) JWTs, not opaque strings — exactly the shapes a real provider
// returns.

/// Mint a syntactically valid compact JWT around `payload` (the signature is
/// never verified client-side).
fn jwt(payload: serde_json::Value) -> String {
    let eng = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = eng.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let body = eng.encode(serde_json::to_vec(&payload).unwrap());
    format!("{header}.{body}.sig")
}

/// The identity assertion a real OIDC provider mints in the `id_token`: carries
/// iss/sub/aud plus a `marker` claim so assertions can tell tokens apart.
fn identity_jwt(marker: &str) -> String {
    jwt(serde_json::json!({
        "iss": "https://forge.example",
        "sub": "operator",
        "aud": "client-A",
        "marker": marker,
    }))
}

/// Forgejo's bookkeeping `access_token` shape: a real JWT with NO identity
/// claims (`{gnt, tt, iat, exp}`).
fn bookkeeping_jwt(n: usize) -> String {
    jwt(serde_json::json!({
        "gnt": "authorization_code",
        "tt": "access",
        "iat": 1_700_000_000 + n as i64,
        "exp": 1_700_000_900 + n as i64,
    }))
}

/// Decode a bearer's `marker` claim (fails the test if the bearer is not one of
/// this file's minted JWTs).
fn marker_of(bearer: &str) -> String {
    let eng = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = bearer.split('.').nth(1).expect("bearer is a JWT");
    let claims: serde_json::Value =
        serde_json::from_slice(&eng.decode(payload).expect("base64url payload")).expect("JSON");
    claims["marker"]
        .as_str()
        .unwrap_or("<no marker>")
        .to_owned()
}

/// A stateful token endpoint that rotates refresh tokens single-use (Forgejo's
/// `InvalidateRefreshTokens: true`): each valid refresh mints a fresh
/// `{access, refresh}` and invalidates the presented one. Reusing a spent
/// refresh token yields `invalid_grant`. Counts the number of *successful*
/// network refreshes so the single-flight test can assert exactly one.
struct OidcMock {
    valid_refresh: Mutex<std::collections::HashSet<String>>,
    seq: AtomicUsize,
    refresh_count: Arc<AtomicUsize>,
}

impl OidcMock {
    fn new(initial_refresh: &str, refresh_count: Arc<AtomicUsize>) -> Self {
        let mut set = std::collections::HashSet::new();
        set.insert(initial_refresh.to_owned());
        Self {
            valid_refresh: Mutex::new(set),
            seq: AtomicUsize::new(1),
            refresh_count,
        }
    }
}

impl Respond for OidcMock {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let params: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(&request.body)
                .into_owned()
                .collect();
        let grant = params.get("grant_type").map_or("", String::as_str);
        match grant {
            "authorization_code" => {
                let n = self.seq.fetch_add(1, Ordering::SeqCst);
                let rt = format!("rt-{n}");
                self.valid_refresh.lock().unwrap().insert(rt.clone());
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": bookkeeping_jwt(n),
                    "refresh_token": rt,
                    "expires_in": 900,
                    "token_type": "bearer",
                    // A real OIDC provider (scope=openid) returns the identity
                    // assertion here; the client stores THIS as the bearer.
                    "id_token": identity_jwt(&format!("id-{n}")),
                }))
            }
            "refresh_token" => {
                let presented = params.get("refresh_token").cloned().unwrap_or_default();
                let mut valid = self.valid_refresh.lock().unwrap();
                if valid.remove(&presented) {
                    self.refresh_count.fetch_add(1, Ordering::SeqCst);
                    let n = self.seq.fetch_add(1, Ordering::SeqCst);
                    let rt = format!("rt-{n}");
                    valid.insert(rt.clone());
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "access_token": bookkeeping_jwt(n),
                        "refresh_token": rt,
                        "expires_in": 900,
                        "token_type": "bearer",
                        // Forgejo re-issues the id_token on every openid refresh.
                        "id_token": identity_jwt(&format!("id-{n}")),
                    }))
                } else {
                    ResponseTemplate::new(400).set_body_json(serde_json::json!({
                        "error": "invalid_grant",
                        "error_description": "refresh token is spent or unknown",
                    }))
                }
            }
            _ => ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "unsupported_grant_type",
            })),
        }
    }
}

fn key() -> CredentialKey {
    CredentialKey::new("https://forge.example", "operator", "client-A")
}

fn expiring_cred(access: &str, refresh: &str) -> StoredCredential {
    // Already past expiry → forces a refresh on the next ensure_token.
    StoredCredential::new(
        SecretToken::new(access),
        SecretToken::new(refresh),
        Utc::now() - ChronoDuration::seconds(10),
    )
}

// --- discovery ----------------------------------------------------------

#[tokio::test]
async fn discover_resolves_endpoints_and_client_id() {
    let server = MockServer::start().await;
    // The registry carries the issuer — validated against the pinned
    // expected_issuer before OIDC Discovery is fetched.
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "schema_version": 2,
            "issuer": server.uri(),
            "clients": [
                {"audience": "cs-rpp-adapter", "client_id": "abc-123"},
                {"audience": "claude-web", "client_id": "def-456"},
            ],
        })))
        .mount(&server)
        .await;
    // OIDC Discovery is fetched from the validated issuer.
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let ep = oidc::discover(
        &http,
        &server.uri(),
        &server.uri(),
        "cs-rpp-adapter",
        vec!["openid".into()],
    )
    .await
    .unwrap();
    assert_eq!(ep.issuer, server.uri());
    assert_eq!(ep.client_id, "abc-123");
    assert_eq!(ep.token_endpoint, format!("{}/token", server.uri()));
}

#[tokio::test]
async fn discover_fails_when_registry_issuer_mismatches_pinned_issuer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "schema_version": 2,
            "issuer": "https://wrong-idp.example",
            "clients": [{"audience": "cs-rpp-adapter", "client_id": "abc"}],
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let err = oidc::discover(
        &http,
        &server.uri(), // pinned expected_issuer
        &server.uri(),
        "cs-rpp-adapter",
        vec!["openid".into()],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::Discovery { .. })
        ),
        "expected Discovery error on issuer mismatch, got {err:?}"
    );
}

#[tokio::test]
async fn discover_fails_when_discovery_issuer_differs_from_pinned_issuer() {
    let server = MockServer::start().await;
    // Registry issuer matches the pinned issuer, so Step 2 passes.
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "schema_version": 2,
            "issuer": server.uri(),
            "clients": [{"audience": "cs-rpp-adapter", "client_id": "abc"}],
        })))
        .mount(&server)
        .await;
    // OIDC Discovery declares a *different* issuer than the pinned one — the
    // §3.3 gate (Step 4) must reject it even though the registry issuer matched.
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": "https://wrong-idp.example",
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let err = oidc::discover(
        &http,
        &server.uri(), // pinned expected_issuer
        &server.uri(),
        "cs-rpp-adapter",
        vec!["openid".into()],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::Discovery { .. })
        ),
        "expected Discovery error on discovery-issuer mismatch, got {err:?}"
    );
}

// --- single-use rotation ------------------------------------------------

#[tokio::test]
async fn refresh_rotates_and_rejects_reuse() {
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(OidcMock::new("rt-seed", count.clone()))
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let k = key();
    store
        .store(&k, &expiring_cred("at-seed", "rt-seed"))
        .unwrap();

    let http = reqwest::Client::new();
    let cfg = oidc::RefreshConfig {
        token_endpoint: format!("{}/token", server.uri()),
        client_id: "client-A".into(),
        rotation: oidc::RefreshRotation::Rotating,
    };
    let leeway = ChronoDuration::seconds(60);

    // First refresh rotates rt-seed → a fresh pair.
    let state = oidc::refresh_credential(&http, &store, &k, &cfg, leeway)
        .await
        .unwrap();
    let first = match state {
        TokenState::Valid(t) => t.expose().to_owned(),
        TokenState::NeedsLogin => panic!("expected Valid, got NeedsLogin"),
    };
    // The bearer is the id_token the mock returned (marker `id-N`), NOT the
    // claim-less access token: the refresh path stores the OIDC identity
    // assertion, the only bearer cosmon-server accepts (task-20260720-71fd).
    assert!(
        marker_of(&first).starts_with("id-"),
        "refreshed bearer must be the id_token, got {first:?}"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Manually reuse the now-spent seed token → the mock rejects it → the flow
    // re-reads and reports RefreshExpired (no fresher token on disk).
    store
        .store(&k, &expiring_cred("at-old", "rt-seed"))
        .unwrap();
    let err = oidc::refresh_credential(&http, &store, &k, &cfg, leeway)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::RefreshExpired)
        ),
        "expected RefreshExpired, got {err:?}"
    );
}

// --- rotating provider that omits the rotated refresh token (F6) --------

/// A token endpoint that accepts one refresh but returns an **empty**
/// `refresh_token` on the grant — the ambiguous shape RFC 6749 §5.1 permits.
/// A rotating provider that does this has still invalidated the presented
/// token, so the client has nothing live to fall back on. Its `access_token`
/// is a full-claim identity JWT (the non-OIDC-deployment shape), so the
/// bearer-suitability check is satisfied and the tests isolate the
/// empty-refresh reconciliation.
struct EmptyRefreshMock;

impl Respond for EmptyRefreshMock {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": identity_jwt("at-new"),
            "refresh_token": "",
            "expires_in": 900,
            "token_type": "bearer",
        }))
    }
}

#[tokio::test]
async fn rotating_provider_empty_refresh_surfaces_refresh_expired() {
    // Regression for F6 (task-20260710-a6ae): on a rotating provider an omitted
    // refresh_token must NOT resurrect the spent one — it must surface
    // RefreshExpired so the caller re-logs in cleanly.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(EmptyRefreshMock)
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let k = key();
    store
        .store(&k, &expiring_cred("at-seed", "rt-spent"))
        .unwrap();

    let http = reqwest::Client::new();
    let cfg = oidc::RefreshConfig {
        token_endpoint: format!("{}/token", server.uri()),
        client_id: "client-A".into(),
        rotation: oidc::RefreshRotation::Rotating,
    };
    let err = oidc::refresh_credential(&http, &store, &k, &cfg, ChronoDuration::seconds(60))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::RefreshExpired)
        ),
        "expected RefreshExpired for an empty rotated refresh token, got {err:?}"
    );
}

#[tokio::test]
async fn static_provider_empty_refresh_reuses_previous() {
    // The mirror case: a non-rotating provider that omits the refresh_token
    // means "keep the one you hold", so the refresh succeeds and the previous
    // refresh token is preserved.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(EmptyRefreshMock)
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let k = key();
    store
        .store(&k, &expiring_cred("at-seed", "rt-keep"))
        .unwrap();

    let http = reqwest::Client::new();
    let cfg = oidc::RefreshConfig {
        token_endpoint: format!("{}/token", server.uri()),
        client_id: "client-A".into(),
        rotation: oidc::RefreshRotation::Static,
    };
    let state = oidc::refresh_credential(&http, &store, &k, &cfg, ChronoDuration::seconds(60))
        .await
        .unwrap();
    match state {
        TokenState::Valid(t) => assert_eq!(t.expose(), identity_jwt("at-new")),
        TokenState::NeedsLogin => panic!("expected Valid, got NeedsLogin"),
    }
    let stored = store.load(&k).unwrap().unwrap();
    assert_eq!(stored.refresh_token().expose(), "rt-keep");
}

// --- concurrent single-flight (the highest-value C9 test) ---------------

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn concurrent_refresh_is_single_flight() {
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/token"))
        // A small delay widens the race window so the test actually exercises
        // contention rather than accidental serialisation.
        .respond_with(OidcMock::new("rt-seed", count.clone()).with_delay())
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let store = Arc::new(CredentialStore::file_at(tmp.path()));
    let k = key();
    store
        .store(&k, &expiring_cred("at-seed", "rt-seed"))
        .unwrap();

    let cfg = oidc::RefreshConfig {
        token_endpoint: format!("{}/token", server.uri()),
        client_id: "client-A".into(),
        rotation: oidc::RefreshRotation::Rotating,
    };
    let leeway = ChronoDuration::seconds(60);

    // N tasks all see the same expiring credential and race to refresh.
    let n = 6;
    let mut handles = Vec::new();
    for _ in 0..n {
        let store = Arc::clone(&store);
        let cfg = cfg.clone();
        let k = k.clone();
        handles.push(tokio::spawn(async move {
            let http = reqwest::Client::new();
            match oidc::refresh_credential(&http, &store, &k, &cfg, leeway)
                .await
                .unwrap()
            {
                TokenState::Valid(t) => t.expose().to_owned(),
                TokenState::NeedsLogin => panic!("expected Valid, got NeedsLogin"),
            }
        }));
    }
    let mut tokens = Vec::new();
    for h in handles {
        tokens.push(h.await.unwrap());
    }

    // Exactly one network refresh happened...
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "single-flight violated: {} network refreshes",
        count.load(Ordering::SeqCst)
    );
    // ...and every racer converged onto the same fresh access token.
    let first = &tokens[0];
    assert!(
        tokens.iter().all(|t| t == first),
        "racers diverged: {tokens:?}"
    );
    // The store holds exactly that fresh token.
    let stored = store.load(&k).unwrap().unwrap();
    assert_eq!(stored.access_token().expose(), first);
}

// --- fast path: valid cache → zero network ------------------------------

#[tokio::test]
async fn ensure_token_fast_path_makes_no_network_call() {
    // No mock server at all: if ensure_token touched the network it would error.
    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let k = key();
    let fresh = StoredCredential::new(
        SecretToken::new("at-valid"),
        SecretToken::new("rt-valid"),
        Utc::now() + ChronoDuration::hours(1),
    );
    store.store(&k, &fresh).unwrap();

    let http = reqwest::Client::new();
    let cfg = oidc::RefreshConfig {
        token_endpoint: "http://127.0.0.1:1/token".into(), // unreachable on purpose
        client_id: "client-A".into(),
        rotation: oidc::RefreshRotation::Rotating,
    };
    let state = oidc::ensure_token(
        &http,
        &store,
        &k,
        &cfg,
        Utc::now(),
        ChronoDuration::seconds(60),
    )
    .await
    .unwrap();
    match state {
        TokenState::Valid(t) => assert_eq!(t.expose(), "at-valid"),
        TokenState::NeedsLogin => panic!("expected Valid, got NeedsLogin"),
    }
}

// --- negative audience --------------------------------------------------

#[tokio::test]
async fn token_for_audience_a_is_never_returned_for_audience_b() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    // A full login for audience A would persist under client-A; here we persist
    // it directly and assert the audience-B key never retrieves it.
    let a = CredentialKey::new("https://forge.example", "operator", "client-A");
    let b = CredentialKey::new("https://forge.example", "operator", "client-B");
    store
        .store(
            &a,
            &StoredCredential::new(
                SecretToken::new("secret-A"),
                SecretToken::new("rt-A"),
                Utc::now() + ChronoDuration::hours(1),
            ),
        )
        .unwrap();

    // The B key's cache read is Cold — A's token is structurally unreachable.
    let state = oidc::cached_access(&store, &b, Utc::now(), ChronoDuration::seconds(60)).unwrap();
    assert!(matches!(state, oidc::CacheState::Cold));
    // And A still returns exactly A.
    match oidc::cached_access(&store, &a, Utc::now(), ChronoDuration::seconds(60)).unwrap() {
        oidc::CacheState::Fresh(t) => assert_eq!(t.expose(), "secret-A"),
        other => panic!("expected Fresh, got {other:?}"),
    }
}

// --- login end-to-end (fake browser drives the loopback) ----------------

#[tokio::test]
async fn login_end_to_end_persists_the_credential() {
    let server = MockServer::start().await;
    let count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(OidcMock::new("unused", count))
        .mount(&server)
        .await;

    // Grab an ephemeral free port for the loopback redirect.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let endpoints = oidc::OidcEndpoints::new(
        "https://forge.example",
        "http://unused.example/authorize",
        format!("{}/token", server.uri()),
        "client-A",
        redirect,
        vec!["openid".into()],
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());

    // The "browser": parse the authorize URL for redirect_uri + state, then fire
    // the callback the way a real browser would after consent.
    let open = |authorize_url: &str| {
        let url = url::Url::parse(authorize_url).unwrap();
        let mut redirect_uri = String::new();
        let mut state = String::new();
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "redirect_uri" => redirect_uri = v.into_owned(),
                "state" => state = v.into_owned(),
                _ => {}
            }
        }
        tokio::spawn(async move {
            // Give login a moment to reach its accept() await.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let cb = format!("{redirect_uri}?code=the-code&state={state}");
            let _ = reqwest::get(&cb).await;
        });
    };

    let http = reqwest::Client::new();
    let outcome = oidc::login(
        &http,
        &store,
        &endpoints,
        "operator",
        std::time::Duration::from_secs(10),
        open,
    )
    .await
    .unwrap();

    // The credential landed under (issuer, sub, client-A).
    assert_eq!(outcome.key.aud(), "client-A");
    let stored = store
        .load(&outcome.key)
        .unwrap()
        .expect("credential persisted");
    // The persisted bearer is the OIDC id_token (marker `id-N`), NOT Forgejo's
    // claim-less access token: the access token carries no iss/aud/sub, so
    // cosmon-server would reject it with `malformed_jwt`. This is the
    // end-to-end regression guard for task-20260720-71fd.
    let bearer = stored.access_token().expose();
    assert!(
        marker_of(bearer).starts_with("id-"),
        "login must persist the id_token as the bearer, got {bearer:?}"
    );
    assert!(stored.has_refresh());
}

// --- adversarial: no identity-bearing token in the response (finding 2) --

/// A token endpoint that answers every grant with a claim-less JWT
/// `access_token` and NO `id_token` — a provider that ignored (or was never
/// sent) the `openid` scope. Pre-fix, the blind fallback persisted that
/// claim-less token as the bearer and armed a guaranteed `401 malformed_jwt`.
struct ClaimlessMock;

impl Respond for ClaimlessMock {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": bookkeeping_jwt(1),
            "refresh_token": "rt-next",
            "expires_in": 900,
            "token_type": "bearer",
        }))
    }
}

#[tokio::test]
async fn login_fails_loud_when_no_token_carries_identity() {
    // Finding 2, login seat: a token response where neither candidate carries
    // iss ∧ sub ∧ aud must fail the login explicitly — and persist NOTHING.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ClaimlessMock)
        .mount(&server)
        .await;

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let endpoints = oidc::OidcEndpoints::new(
        "https://forge.example",
        "http://unused.example/authorize",
        format!("{}/token", server.uri()),
        "client-A",
        format!("http://127.0.0.1:{port}/callback"),
        vec!["openid".into()],
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());

    let open = |authorize_url: &str| {
        let url = url::Url::parse(authorize_url).unwrap();
        let mut redirect_uri = String::new();
        let mut state = String::new();
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "redirect_uri" => redirect_uri = v.into_owned(),
                "state" => state = v.into_owned(),
                _ => {}
            }
        }
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let cb = format!("{redirect_uri}?code=the-code&state={state}");
            let _ = reqwest::get(&cb).await;
        });
    };

    let http = reqwest::Client::new();
    let err = oidc::login(
        &http,
        &store,
        &endpoints,
        "operator",
        std::time::Duration::from_secs(10),
        open,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::NoIdentityBearer)
        ),
        "expected NoIdentityBearer at login, got {err:?}"
    );
    // Nothing was persisted: a doomed bearer must never land in the store.
    let key = endpoints.credential_key("operator");
    assert!(store.load(&key).unwrap().is_none());
}

#[tokio::test]
async fn refresh_fails_loud_when_rotation_returns_no_identity_bearer() {
    // Finding 2, refresh seat: the login-only false-green. A fix that selects
    // the id_token at login but blindly falls back on rotation would re-arm the
    // 401 at the first 15-minute refresh. The rotation must fail explicitly —
    // and must NOT clobber the stored credential with the claim-less token.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ClaimlessMock)
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let k = key();
    store
        .store(&k, &expiring_cred("at-seed", "rt-seed"))
        .unwrap();

    let http = reqwest::Client::new();
    let cfg = oidc::RefreshConfig {
        token_endpoint: format!("{}/token", server.uri()),
        client_id: "client-A".into(),
        rotation: oidc::RefreshRotation::Rotating,
    };
    let err = oidc::refresh_credential(&http, &store, &k, &cfg, ChronoDuration::seconds(60))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::NoIdentityBearer)
        ),
        "expected NoIdentityBearer at refresh, got {err:?}"
    );
    // The store still holds the seed pair — the claim-less rotation never
    // overwrote it.
    let stored = store.load(&k).unwrap().unwrap();
    assert_eq!(stored.access_token().expose(), "at-seed");
}

// --- adversarial: id_token present but unusable (round-2 finding) --------

/// A token endpoint that answers every grant with the claim-less bookkeeping
/// `access_token` AND an `id_token` minted around the given payload. With a
/// degenerate payload (`"aud": null`, `""`, `[]`, non-string `iss`/`sub`) the
/// pre-fix presence check selected and persisted the `id_token` even though the
/// server's closed `(iss, aud)` allowlist can never match it — the same 401
/// class as issue #27.
struct DegenerateIdMock(serde_json::Value);

impl Respond for DegenerateIdMock {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": bookkeeping_jwt(1),
            "refresh_token": "rt-next",
            "expires_in": 900,
            "token_type": "bearer",
            "id_token": jwt(self.0.clone()),
        }))
    }
}

/// Identity payloads where every claim is present but at least one is unusable
/// per RFC 7519 §4.1.3 — the round-2 referee finding's attack surface.
fn degenerate_identity_payloads() -> Vec<(&'static str, serde_json::Value)> {
    let base = serde_json::json!({
        "iss": "https://forge.example",
        "sub": "operator",
        "aud": "client-A",
    });
    let with = |claim: &str, value: serde_json::Value| {
        let mut p = base.clone();
        p[claim] = value;
        p
    };
    vec![
        ("null aud", with("aud", serde_json::Value::Null)),
        ("empty-string aud", with("aud", serde_json::json!(""))),
        ("empty-array aud", with("aud", serde_json::json!([]))),
        ("non-string iss", with("iss", serde_json::json!(7))),
        (
            "non-string sub",
            with("sub", serde_json::json!(["operator"])),
        ),
    ]
}

/// The fake browser used by the login-seam tests: parse the authorize URL for
/// `redirect_uri` + `state`, then fire the callback like a real browser would.
fn fake_browser(authorize_url: &str) {
    let url = url::Url::parse(authorize_url).unwrap();
    let mut redirect_uri = String::new();
    let mut state = String::new();
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "redirect_uri" => redirect_uri = v.into_owned(),
            "state" => state = v.into_owned(),
            _ => {}
        }
    }
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let cb = format!("{redirect_uri}?code=the-code&state={state}");
        let _ = reqwest::get(&cb).await;
    });
}

#[tokio::test]
async fn login_fails_loud_when_the_id_token_claims_are_unusable() {
    // Round-2 finding, login persist seam: an id_token whose identity claims
    // are present but degenerate must not be persisted as the bearer — with no
    // usable fallback the login fails loud, and NOTHING lands in the store.
    for (label, payload) in degenerate_identity_payloads() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(DegenerateIdMock(payload))
            .mount(&server)
            .await;

        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let endpoints = oidc::OidcEndpoints::new(
            "https://forge.example",
            "http://unused.example/authorize",
            format!("{}/token", server.uri()),
            "client-A",
            format!("http://127.0.0.1:{port}/callback"),
            vec!["openid".into()],
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());

        let http = reqwest::Client::new();
        let err = oidc::login(
            &http,
            &store,
            &endpoints,
            "operator",
            std::time::Duration::from_secs(10),
            fake_browser,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                cosmon_remote::Error::Oidc(cosmon_remote::OidcError::NoIdentityBearer)
            ),
            "{label}: expected NoIdentityBearer at login, got {err:?}"
        );
        let key = endpoints.credential_key("operator");
        assert!(
            store.load(&key).unwrap().is_none(),
            "{label}: an unusable bearer must never land in the store"
        );
    }
}

#[tokio::test]
async fn refresh_fails_loud_when_the_rotated_id_token_claims_are_unusable() {
    // Round-2 finding, refresh rotate seam: a rotation whose id_token carries
    // degenerate claims must fail loud and must NOT clobber the stored
    // credential with the unusable bearer.
    for (label, payload) in degenerate_identity_payloads() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(DegenerateIdMock(payload))
            .mount(&server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key();
        store
            .store(&k, &expiring_cred("at-seed", "rt-seed"))
            .unwrap();

        let http = reqwest::Client::new();
        let cfg = oidc::RefreshConfig {
            token_endpoint: format!("{}/token", server.uri()),
            client_id: "client-A".into(),
            rotation: oidc::RefreshRotation::Rotating,
        };
        let err = oidc::refresh_credential(&http, &store, &k, &cfg, ChronoDuration::seconds(60))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                cosmon_remote::Error::Oidc(cosmon_remote::OidcError::NoIdentityBearer)
            ),
            "{label}: expected NoIdentityBearer at refresh, got {err:?}"
        );
        let stored = store.load(&k).unwrap().unwrap();
        assert_eq!(
            stored.access_token().expose(),
            "at-seed",
            "{label}: the unusable rotation must not overwrite the store"
        );
    }
}

// --- adversarial: the token's `iss` names a different authority ----------

/// A token endpoint that mints a fully-formed identity `id_token` whose `iss`
/// names a DIFFERENT authority (`https://evil.example`) than the one the flow
/// authenticated against (`https://forge.example`). `sub`/`aud` are valid, so
/// the token passes identity-bearer selection — only the OIDC Core §3.1.3.7
/// issuer check can reject it. Answers both grants, driving the login and
/// refresh seats.
struct WrongIssuerMock;

impl Respond for WrongIssuerMock {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": bookkeeping_jwt(1),
            "refresh_token": "rt-next",
            "expires_in": 900,
            "token_type": "bearer",
            "id_token": jwt(serde_json::json!({
                "iss": "https://evil.example",
                "sub": "operator",
                "aud": "client-A",
            })),
        }))
    }
}

#[tokio::test]
async fn login_fails_loud_when_the_id_token_issuer_differs_from_the_authenticated_issuer() {
    // The provider serving the pinned issuer's discovery mints a token whose
    // `iss` names a different authority. Persisting it would file a bearer under
    // `https://forge.example`'s key while the resource server resolves its real
    // `iss` elsewhere — so the login must fail loud and persist NOTHING.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(WrongIssuerMock)
        .mount(&server)
        .await;

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let endpoints = oidc::OidcEndpoints::new(
        "https://forge.example",
        "http://unused.example/authorize",
        format!("{}/token", server.uri()),
        "client-A",
        format!("http://127.0.0.1:{port}/callback"),
        vec!["openid".into()],
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());

    let http = reqwest::Client::new();
    let err = oidc::login(
        &http,
        &store,
        &endpoints,
        "operator",
        std::time::Duration::from_secs(10),
        fake_browser,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::TokenIssuerMismatch { .. })
        ),
        "expected TokenIssuerMismatch at login, got {err:?}"
    );
    let key = endpoints.credential_key("operator");
    assert!(
        store.load(&key).unwrap().is_none(),
        "a token for a different issuer must never land in the store"
    );
}

#[tokio::test]
async fn refresh_fails_loud_when_the_rotated_id_token_issuer_differs() {
    // The rotation seat: a credential pinned to `https://forge.example` must not
    // be overwritten by a rotation whose id_token names a different `iss`. The
    // refresh fails loud and the seed pair stays put.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(WrongIssuerMock)
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let k = key(); // issuer == https://forge.example
    store
        .store(&k, &expiring_cred("at-seed", "rt-seed"))
        .unwrap();

    let http = reqwest::Client::new();
    let cfg = oidc::RefreshConfig {
        token_endpoint: format!("{}/token", server.uri()),
        client_id: "client-A".into(),
        rotation: oidc::RefreshRotation::Rotating,
    };
    let err = oidc::refresh_credential(&http, &store, &k, &cfg, ChronoDuration::seconds(60))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            cosmon_remote::Error::Oidc(cosmon_remote::OidcError::TokenIssuerMismatch { .. })
        ),
        "expected TokenIssuerMismatch at refresh, got {err:?}"
    );
    let stored = store.load(&k).unwrap().unwrap();
    assert_eq!(
        stored.access_token().expose(),
        "at-seed",
        "a wrong-issuer rotation must not overwrite the store"
    );
}

// --- the login outcome reports the TOKEN's identity, not the profile's ---

/// A provider that issues an `id_token` for a subject the caller did NOT ask
/// for — the live shape of the 2026-07-25 replay, where the local profile
/// recorded `sub = "1"` while Forgejo minted `sub = "2"`.
struct DisagreeingSubMock;

impl Respond for DisagreeingSubMock {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": bookkeeping_jwt(1),
            "refresh_token": "rt-next",
            "expires_in": 900,
            "token_type": "bearer",
            "id_token": jwt(serde_json::json!({
                "iss": "https://forge.example",
                "sub": "2",
                "aud": "client-A",
            })),
        }))
    }
}

#[tokio::test]
async fn login_outcome_reports_the_token_subject_not_the_requested_one() {
    // The profile says "1", the provider issues "2". The login outcome must
    // carry the token's answer — a client that echoed the requested `sub` would
    // assert an identity the server never granted.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(DisagreeingSubMock)
        .mount(&server)
        .await;

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let endpoints = oidc::OidcEndpoints::new(
        "https://forge.example",
        "http://unused.example/authorize",
        format!("{}/token", server.uri()),
        "client-A",
        format!("http://127.0.0.1:{port}/callback"),
        vec!["openid".into()],
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let http = reqwest::Client::new();
    let outcome = oidc::login(
        &http,
        &store,
        &endpoints,
        "1", // what the profile asks for
        std::time::Duration::from_secs(10),
        fake_browser,
    )
    .await
    .unwrap();

    let identity = outcome
        .identity
        .expect("the retained id_token carries identity claims");
    assert_eq!(
        identity.sub, "2",
        "the reported identity must come from the token, not the requested sub"
    );
    assert_eq!(identity.iss, "https://forge.example");
    // The credential still keys off the requested sub — the slot is a local
    // filing decision; only the *displayed identity* is the token's.
    assert_eq!(outcome.key.sub(), "1");
}

// --- #27 end-to-end: a provider that GATES the id_token on `openid` ------
//
// Everything above stipulates the provider's generosity: the mocks return an
// `id_token` unconditionally, so they prove the client *selects* the right
// bearer but say nothing about whether it *asks* for one. Issue #27's two
// defects are coupled precisely there — a client that never sends `openid` gets
// no `id_token` to select, and the hardened selection then fails loud instead of
// producing a login. The mock below is the missing half: it mints an `id_token`
// only when the authorization request carried `openid`, which is Forgejo's
// actual behaviour and the only shape that can distinguish the fix from its
// absence.
//
// `cs-oidc-mock` (crate `cosmon-oidc-testkit`) cannot host this reproduction:
// it exposes exactly one route, `GET /jwks`, and by documented design has no
// discovery document, no authorization endpoint, and no token endpoint — the
// caller signs a JWT directly via `issue_jwt`. With no authorization request to
// carry `openid` and no token response to withhold, there is nothing to gate.
// The gate therefore lives here, in the `wiremock` harness that already models
// the token endpoint's wire shapes.

/// A provider whose `id_token` issuance is gated on the `openid` scope.
///
/// `GET /authorize` records whether the authorization request's `scope`
/// parameter contained `openid`, then redirects to the loopback `redirect_uri`
/// the way a real authorization server does after consent. `POST /token` (both
/// grants) consults that record: with `openid` it returns
/// `{access_token, refresh_token, id_token}`; without it, only the claim-less
/// bookkeeping `access_token` — the exact `401 malformed_jwt` shape from #27.
///
/// Sharing one `granted_openid` flag across both grants models the real
/// coupling: the refresh grant carries no `scope` of its own and inherits the
/// consent grant's, so a login that failed to request `openid` yields no
/// `id_token` at refresh time either.
struct OpenidGatingProvider {
    granted_openid: Arc<std::sync::atomic::AtomicBool>,
    /// The issuer this provider serves discovery under — stamped into the `iss`
    /// of every `id_token` it mints, so the flow's OIDC Core §3.1.3.7 issuer
    /// check accepts the bearer it authenticated against.
    issuer: String,
    seq: AtomicUsize,
    valid_refresh: Mutex<std::collections::HashSet<String>>,
}

impl OpenidGatingProvider {
    fn new(issuer: String, granted_openid: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            granted_openid,
            issuer,
            seq: AtomicUsize::new(1),
            valid_refresh: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// The authorization-endpoint half: observe the requested scopes, then
    /// redirect to the loopback with a code (`reqwest` follows the 302, exactly
    /// as a browser would).
    fn authorize(&self, request: &Request) -> ResponseTemplate {
        let mut redirect_uri = String::new();
        let mut state = String::new();
        let mut scope = String::new();
        for (k, v) in request.url.query_pairs() {
            match k.as_ref() {
                "redirect_uri" => redirect_uri = v.into_owned(),
                "state" => state = v.into_owned(),
                "scope" => scope = v.into_owned(),
                _ => {}
            }
        }
        // Space-delimited per RFC 6749 §3.3 — match the whole token, so a scope
        // merely *containing* the substring (`openid-adjacent`) does not pass.
        let openid = scope.split(' ').any(|s| s == "openid");
        self.granted_openid.store(openid, Ordering::SeqCst);
        ResponseTemplate::new(302).insert_header(
            "location",
            format!("{redirect_uri}?code=the-code&state={state}").as_str(),
        )
    }

    /// The token-endpoint half: the response shape depends on whether the
    /// authorization request that opened this grant carried `openid`.
    fn token(&self, request: &Request) -> ResponseTemplate {
        let params: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(&request.body)
                .into_owned()
                .collect();
        // A refresh grant must present a live refresh token; the code grant
        // opens the chain.
        if params.get("grant_type").map_or("", String::as_str) == "refresh_token" {
            let presented = params.get("refresh_token").cloned().unwrap_or_default();
            if !self.valid_refresh.lock().unwrap().remove(&presented) {
                return ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": "invalid_grant",
                    "error_description": "refresh token is spent or unknown",
                }));
            }
        }
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        let rt = format!("rt-{n}");
        self.valid_refresh.lock().unwrap().insert(rt.clone());
        let mut body = serde_json::json!({
            "access_token": bookkeeping_jwt(n),
            "refresh_token": rt,
            "expires_in": 900,
            "token_type": "bearer",
        });
        if self.granted_openid.load(Ordering::SeqCst) {
            // Stamp the provider's own issuer so the flow's issuer check accepts
            // the bearer; the `marker` claim keeps candidates distinguishable.
            body["id_token"] = serde_json::json!(jwt(serde_json::json!({
                "iss": self.issuer,
                "sub": "operator",
                "aud": "client-A",
                "marker": format!("id-{n}"),
            })));
        }
        ResponseTemplate::new(200).set_body_json(body)
    }
}

/// `wiremock` dispatches by matched route, so one `Respond` serves both halves.
impl Respond for OpenidGatingProvider {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if request.url.path() == "/authorize" {
            self.authorize(request)
        } else {
            self.token(request)
        }
    }
}

/// Mount the gating provider's discovery document, client registry, authorize
/// and token endpoints. `published_scopes` is what the cosmon reverse-discovery
/// document advertises for the audience — `Some(vec![])` is #27's trigger.
async fn mount_gating_provider(
    server: &MockServer,
    published_scopes: Option<Vec<&str>>,
    granted_openid: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut client = serde_json::json!({
        "audience": "cs-rpp-adapter",
        "client_id": "client-A",
    });
    if let Some(scopes) = published_scopes {
        client["scopes"] = serde_json::json!(scopes);
    }
    // Registry is fetched first; its issuer is validated then used to fetch
    // OIDC Discovery. Both must carry the same issuer URL.
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "schema_version": 2,
            "issuer": server.uri(),
            "clients": [client],
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        })))
        .mount(server)
        .await;
    let provider = Arc::new(OpenidGatingProvider::new(server.uri(), granted_openid));
    Mock::given(method("GET"))
        .and(path("/authorize"))
        .respond_with(SharedProvider(provider.clone()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(SharedProvider(provider))
        .mount(server)
        .await;
}

/// Two mounts, one provider: the authorize and token halves must share the
/// `openid` record, so the `Respond` is registered behind an `Arc`.
struct SharedProvider(Arc<OpenidGatingProvider>);
impl Respond for SharedProvider {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.0.respond(request)
    }
}

/// A browser that simply *follows* the authorize URL, so the authorization
/// server observes the request (and its `scope`) before redirecting to the
/// loopback. `fake_browser` above short-circuits the provider by firing the
/// callback itself — which is why it cannot exercise a scope gate.
fn following_browser(authorize_url: &str) {
    let url = authorize_url.to_owned();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Redirect-following is `reqwest`'s default — the 302 lands on the
        // loopback listener exactly as a browser's would.
        let _ = reqwest::get(&url).await;
    });
}

#[tokio::test]
async fn login_and_refresh_carry_identity_against_a_provider_that_gates_on_openid() {
    // The #27 acceptance criterion, end to end and in one run: against a
    // provider that mints an `id_token` only for `openid`, and a
    // reverse-discovery document that publishes an EMPTY scope set (the exact
    // trigger — `scopes: []` is present, so it overrides the fallback), the
    // authorization request must still carry `openid`, and the bearer must carry
    // OIDC identity claims BOTH after login and after a refresh.
    //
    // This test FAILS without the fix: drop `ensure_openid` from `discover` and
    // the empty published set reaches the authorize URL verbatim, the provider
    // mints no `id_token`, and `login` fails `NoIdentityBearer`. Verified by
    // reverting the call.
    let server = MockServer::start().await;
    let granted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    mount_gating_provider(&server, Some(vec![]), granted.clone()).await;

    // The loopback redirect the provider will 302 to.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let http = reqwest::Client::new();
    let mut endpoints = oidc::discover(
        &http,
        &server.uri(),
        &server.uri(),
        "cs-rpp-adapter",
        // A profile fallback that also lacks `openid`: neither source supplies
        // it, so only `discover`'s own guarantee can.
        vec!["cosmon:molecule:read".into()],
    )
    .await
    .unwrap();
    endpoints.redirect_uri = format!("http://127.0.0.1:{port}/callback");

    // The scope the client will actually send leads with `openid`.
    assert_eq!(
        endpoints.scopes.first().map(String::as_str),
        Some("openid"),
        "discover must request openid even when the registry publishes scopes: []"
    );

    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    let outcome = oidc::login(
        &http,
        &store,
        &endpoints,
        "operator",
        std::time::Duration::from_secs(10),
        following_browser,
    )
    .await
    .expect("login must succeed against an openid-gating provider");

    // The provider saw `openid` in the authorization request — the client asked.
    assert!(
        granted.load(Ordering::SeqCst),
        "the authorization request must carry the openid scope"
    );
    // And the bearer it retained carries the identity claims the resource server
    // resolves (iss ∧ sub ∧ aud), not the bookkeeping access token.
    let after_login = store
        .load(&outcome.key)
        .unwrap()
        .expect("credential persisted");
    assert!(
        marker_of(after_login.access_token().expose()).starts_with("id-"),
        "the post-login bearer must be the id_token"
    );
    let identity = outcome
        .identity
        .expect("the post-login bearer carries identity claims");
    assert_eq!(identity.iss, server.uri());
    assert_eq!(identity.sub, "operator");

    // --- and after a refresh -------------------------------------------------
    // #27 says "after login AND after refresh". Age the credential so the next
    // ensure_token must rotate, keeping the same refresh token.
    let refresh = after_login.refresh_token().expose().to_owned();
    store
        .store(
            &outcome.key,
            &expiring_cred(after_login.access_token().expose(), &refresh),
        )
        .unwrap();
    let state = oidc::ensure_token(
        &http,
        &store,
        &outcome.key,
        &endpoints.refresh_config(),
        Utc::now(),
        ChronoDuration::seconds(60),
    )
    .await
    .expect("the refresh grant must succeed");
    let rotated = match state {
        TokenState::Valid(t) => t.expose().to_owned(),
        TokenState::NeedsLogin => panic!("expected Valid after a refresh, got NeedsLogin"),
    };
    assert!(
        marker_of(&rotated).starts_with("id-"),
        "the post-refresh bearer must be the freshly issued id_token, got {rotated:?}"
    );
    assert_eq!(
        oidc::bearer_identity(&rotated)
            .expect("the post-refresh bearer carries identity claims")
            .iss,
        server.uri()
    );
}

// --- the openid guarantee, at the discover seam --------------------------

#[tokio::test]
async fn discover_requests_openid_whatever_the_registry_publishes() {
    // The wiring guard for `ensure_openid`. Its unit tests prove the function is
    // correct; nothing proved `discover` still *calls* it — deleting the call
    // left all 234 crate tests green. Each case below pins one shape of the
    // published scope set through the real discovery round-trip.
    for (published, fallback, expected) in [
        // #27's trigger: present-but-empty, so it overrides the fallback.
        (Some(vec![]), vec!["cosmon:molecule:read"], vec!["openid"]),
        // Absent: the profile fallback applies — and is normalized too.
        (
            None,
            vec!["cosmon:molecule:read"],
            vec!["openid", "cosmon:molecule:read"],
        ),
        // Published without `openid`: prepended, order otherwise preserved.
        (
            Some(vec!["cosmon:molecule:read", "cosmon:molecule:write"]),
            vec![],
            vec!["openid", "cosmon:molecule:read", "cosmon:molecule:write"],
        ),
        // Published with `openid` trailing: moved to the front, never doubled.
        (
            Some(vec!["profile", "openid"]),
            vec![],
            vec!["openid", "profile"],
        ),
    ] {
        let server = MockServer::start().await;
        mount_gating_provider(
            &server,
            published.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;
        let http = reqwest::Client::new();
        let ep = oidc::discover(
            &http,
            &server.uri(),
            &server.uri(),
            "cs-rpp-adapter",
            fallback.iter().map(|s| (*s).to_string()).collect(),
        )
        .await
        .unwrap();
        assert_eq!(
            ep.scopes, expected,
            "published scopes {published:?} must resolve to {expected:?}"
        );
        // The scope the provider actually reads is the joined query parameter —
        // assert on that, not only on the vector behind it.
        let url = oidc::build_authorize_url(&ep, "the-state", "the-challenge").unwrap();
        let scope = url::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "scope")
            .map(|(_, v)| v.into_owned())
            .expect("the authorize URL carries a scope parameter");
        assert_eq!(scope, expected.join(" "));
    }
}

/// A tiny extension so the single-flight mock can add a response delay without a
/// second `Respond` wrapper type.
trait WithDelay {
    fn with_delay(self) -> DelayedMock;
}
impl WithDelay for OidcMock {
    fn with_delay(self) -> DelayedMock {
        DelayedMock(self)
    }
}
struct DelayedMock(OidcMock);
impl Respond for DelayedMock {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.0
            .respond(request)
            .set_delay(std::time::Duration::from_millis(80))
    }
}
