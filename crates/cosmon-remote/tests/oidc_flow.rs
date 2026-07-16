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
//!   audience B (the isolation is proved by *absence*, not by acceptance).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Duration as ChronoDuration, Utc};
use cosmon_remote::credential::{CredentialKey, CredentialStore, SecretToken, StoredCredential};
use cosmon_remote::oidc::{self, TokenState};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

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
                    "access_token": format!("at-{n}"),
                    "refresh_token": rt,
                    "expires_in": 900,
                    "token_type": "bearer",
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
                        "access_token": format!("at-{n}"),
                        "refresh_token": rt,
                        "expires_in": 900,
                        "token_type": "bearer",
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
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": "https://forge.example",
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "schema_version": 1,
            "clients": [
                {"audience": "cs-rpp-adapter", "client_id": "abc-123"},
                {"audience": "claude-web", "client_id": "def-456"},
            ],
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
    assert_eq!(ep.issuer, "https://forge.example");
    assert_eq!(ep.client_id, "abc-123");
    assert_eq!(ep.token_endpoint, format!("{}/token", server.uri()));
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
    assert!(first.starts_with("at-"));
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
/// token, so the client has nothing live to fall back on.
struct EmptyRefreshMock;

impl Respond for EmptyRefreshMock {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-new",
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
        TokenState::Valid(t) => assert_eq!(t.expose(), "at-new"),
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
    assert!(stored.access_token().expose().starts_with("at-"));
    assert!(stored.has_refresh());
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
