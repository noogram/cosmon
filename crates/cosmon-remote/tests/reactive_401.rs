// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for the reactive-`401` refresh seam
//! (delib-20260710-33b7 C2, kahneman-F7).
//!
//! The *proactive* refresh (on the 15-minute expiry boundary) is covered by
//! `oidc_flow.rs`. This file exercises the residual path that proactive refresh
//! structurally cannot see: the server rejects a bearer the client still
//! believes fresh — its clock ahead of ours past the leeway — with a `401`. A
//! [`Client`] carrying a [`ReactiveRefresh`] binding must then force exactly one
//! silent refresh, swap the bearer, and retry the request once, transparently to
//! the caller.
//!
//! CI (which sets `COSMON_REMOTE_TOKEN`) never carries a reauth binding, so this
//! is precisely the path CI's shape cannot generate by accident — the same
//! observation kahneman made about silent refresh being the hottest path in the
//! binary and the coldest in the test suite.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use cosmon_remote::client::{Client, ListFilters, ReactiveRefresh};
use cosmon_remote::config::Profile;
use cosmon_remote::credential::{CredentialKey, CredentialStore, SecretToken, StoredCredential};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile(host: &str, oidc_url: &str) -> Profile {
    Profile {
        host: host.to_owned(),
        sub: "operator".into(),
        aud: "client-A".into(),
        oidc_url: oidc_url.to_owned(),
        issuer: Some("https://forge.example".into()),
        client_id: Some("client-A".into()),
        noyau: None,
        scopes: vec!["cosmon:molecule:read".into()],
        artifacts_dir: None,
        timeout_secs: 5,
        // Off: keep the passive remontée out of the way of the header assertions.
        phone_home: false,
    }
}

fn key() -> CredentialKey {
    CredentialKey::new("https://forge.example", "operator", "client-A")
}

/// The full round-trip: a `401` on a locally-valid bearer triggers one silent
/// refresh and a single retry that succeeds, with the caller none the wiser.
#[tokio::test]
async fn reactive_401_forces_one_refresh_and_retries_transparently() {
    let server = MockServer::start().await;

    // OIDC discovery — the reauth binding resolves the token endpoint lazily,
    // only when the 401 actually fires.
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": "https://forge.example",
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        })))
        .mount(&server)
        .await;

    // The token endpoint mints the fresh pair. One refresh is all this path needs;
    // a counter proves it fired exactly once (single retry, no storm).
    let refreshes = Arc::new(AtomicUsize::new(0));
    let refreshes_probe = Arc::clone(&refreshes);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(move |_: &wiremock::Request| {
            refreshes_probe.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh-at",
                "refresh_token": "rt-2",
                "expires_in": 900,
                "token_type": "bearer",
            }))
        })
        .mount(&server)
        .await;

    // The protected route: the stale bearer is rejected; the fresh one is served.
    Mock::given(method("GET"))
        .and(path("/v1/molecules"))
        .and(header("authorization", "Bearer stale-at"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "token_expired",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules"))
        .and(header("authorization", "Bearer fresh-at"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "request_id": "req-1",
            "ensemble": { "molecules": [] },
        })))
        .mount(&server)
        .await;

    // The persisted credential: the store's access token matches the stale bearer
    // the server rejects (no peer rotated), so the reactive path performs a real
    // refresh rather than adopting.
    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    store
        .store(
            &key(),
            &StoredCredential::new(
                SecretToken::new("stale-at"),
                SecretToken::new("rt-seed"),
                Utc::now() + ChronoDuration::hours(1),
            ),
        )
        .unwrap();

    let http = reqwest::Client::new();
    let reauth = ReactiveRefresh::new(http, store, key(), server.uri(), "client-A");
    let client = Client::new(
        &profile(&server.uri(), &server.uri()),
        Some("stale-at".into()),
    )
    .unwrap()
    .with_reauth(reauth);

    // The caller just lists molecules — it never sees the 401 or the refresh.
    let env = client
        .list_molecules(&ListFilters::default())
        .await
        .unwrap();
    assert_eq!(env.request_id, "req-1");
    assert!(env.molecules().is_empty());
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "the reactive path must refresh exactly once"
    );
}

/// Without a reauth binding (env / mock / operator-token profiles), a `401` is
/// surfaced verbatim as [`cosmon_remote::Error::Api`] — never a silent hang, and
/// never a refresh attempt the profile has no credential for.
#[tokio::test]
async fn a_401_without_a_reauth_binding_surfaces_as_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "token_expired",
        })))
        .mount(&server)
        .await;

    let client = Client::new(
        &profile(&server.uri(), &server.uri()),
        Some("stale-at".into()),
    )
    .unwrap();
    let err = client
        .list_molecules(&ListFilters::default())
        .await
        .unwrap_err();
    match err {
        cosmon_remote::Error::Api { status, .. } => assert_eq!(status, 401),
        other => panic!("expected Api{{status:401}}, got {other:?}"),
    }
}
