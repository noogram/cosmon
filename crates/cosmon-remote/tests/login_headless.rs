// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end `login` against the real `cs-oidc-mock` `IdP`, with no browser
//! (GitHub issue #53, U2).
//!
//! Every other login test in this crate stubs the provider with `wiremock`
//! and injects a closure that fabricates the callback. This one does
//! neither. It serves the **shipped** `IdP` router
//! ([`cosmon_oidc_testkit::idp`] — the same handlers `cs-oidc-mock` serves
//! in a container), walks the client's own discovery, and hands the
//! authorize URL to the **shipped** opener seam
//! ([`cosmon_remote::oidc::Opener`]) configured exactly as the container
//! smoke configures it: `curl -sS -L`. Nothing between the two ends is
//! written for the test.
//!
//! What it therefore proves, and a stub cannot:
//!
//! - the mock's discovery document parses with the client's own
//!   [`ProviderMetadata`] parser, not with a JSON shape the test asserted;
//! - `curl -L` alone completes the flow — auto-approve, 302, loopback
//!   catch — so a shell script can drive `login` with no display;
//! - the persisted bearer **verifies** against the `IdP`'s signing key with
//!   the right `iss` / `aud` / `exp`: the four checks
//!   `cosmon-rpp-adapter::JwtVerifier` runs before `GET /v1/auth/me`
//!   answers. A bearer that merely decodes is the bearer that 401s.
//!
//! The falsifier is [`a_corrupted_pkce_challenge_fails_the_login_and_persists_nothing`]:
//! the `IdP` stores a deliberately wrong `code_challenge`, so the verifier
//! the client mints cannot match. Login must fail and the store must stay
//! empty. Without it, a green login would say nothing about whether PKCE
//! is checked at all.

use std::time::Duration;

use cosmon_oidc_testkit::{verify_signed_claims, IdpConfig, MockIdp};
use cosmon_remote::credential::CredentialStore;
use cosmon_remote::oidc::{self, Opener, ProviderMetadata};

/// The audience/client_id the mock provisions. cosmon pins `aud ==
/// client_id`, so this one string is both.
const CLIENT_ID: &str = "cs-rpp-adapter-e2e";

/// The subject the `IdP` signs in as (it auto-approves, so there is no
/// consent screen to choose one at).
const SUBJECT: &str = "operator-e2e";

/// How long the loopback catcher waits. Far below the production
/// five-minute timeout on purpose: if the opener never fires, this test
/// must fail in seconds with a legible message, not idle for five minutes
/// and read to the fleet as a healthy worker.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The opener the container smoke uses, verbatim.
const CURL_OPENER: &str = "curl -sS -L -o /dev/null";

/// Fail loudly if `curl` is missing rather than skipping: a test that
/// silently opts out of running is a test that hides the drift it exists
/// to catch.
fn require_curl() {
    let ok = std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok,
        "`curl` is required: it is the opener the headless login seam is \
         documented and smoke-tested with"
    );
}

/// Reserve a free loopback port by binding and immediately releasing it.
///
/// The redirect port cannot be the crate default here — these tests run
/// concurrently with each other and with `oidc_flow.rs`, and two logins on
/// one port would collide. The window between release and re-bind is the
/// standard ephemeral-port race; on a loopback interface in a test process
/// it is not a practical hazard, and a collision surfaces as a bind error,
/// never as a silently wrong assertion.
fn free_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Start the mock `IdP` and resolve the client's endpoints through the
/// client's **own** discovery parser — the half of the contract issue #53
/// added to the mock.
async fn endpoints_via_discovery(idp: &MockIdp, http: &reqwest::Client) -> oidc::OidcEndpoints {
    let meta = ProviderMetadata::fetch(http, idp.base_url())
        .await
        .expect("the mock serves a discovery document this client can parse");
    assert_eq!(meta.issuer, idp.base_url());
    oidc::OidcEndpoints::new(
        meta.issuer,
        meta.authorization_endpoint,
        meta.token_endpoint,
        CLIENT_ID,
        oidc::redirect_uri(free_loopback_port()),
        vec!["openid".to_owned()],
    )
}

fn idp_config(base: &str, corrupt_code_challenge: bool) -> IdpConfig {
    IdpConfig {
        issuer: base.to_owned(),
        audiences: vec![CLIENT_ID.to_owned()],
        subject: SUBJECT.to_owned(),
        corrupt_code_challenge,
        ..IdpConfig::default()
    }
}

#[tokio::test]
async fn a_headless_login_through_the_opener_seam_persists_a_verifiable_bearer() {
    require_curl();
    let idp = MockIdp::spawn(|base| idp_config(base, false))
        .await
        .expect("start the mock IdP");
    let http = reqwest::Client::new();
    let endpoints = endpoints_via_discovery(&idp, &http).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let store = CredentialStore::file_at(tmp.path());

    // The seam under test, built the way the CLI builds it from the
    // environment — no test-only closure anywhere in the flow.
    let opener = Opener::from_raw(Some(CURL_OPENER)).expect("a well-formed opener command");
    assert!(matches!(opener, Opener::Spawn(_)));

    let outcome = oidc::login(&http, &store, &endpoints, SUBJECT, TIMEOUT, |url| {
        opener.open(url);
    })
    .await
    .expect("a full browser-less login against the real mock IdP");

    // The identity comes out of the token, never out of what we asked for.
    let identity = outcome
        .identity
        .expect("the bearer carries identity claims");
    assert_eq!(identity.sub, SUBJECT);
    assert_eq!(identity.iss, idp.base_url());

    // Persisted — the half of `login` a hand-driven exchange never reaches.
    let cred = store
        .load(&outcome.key)
        .expect("read the store back")
        .expect("login persisted a credential under its own key");

    // …and the persisted bearer is one the resource server would accept:
    // signature, expiry, issuer and audience all check out.
    let claims = verify_signed_claims(cred.access_token().expose(), idp.base_url(), CLIENT_ID)
        .expect("the persisted bearer verifies against the IdP's signing key");
    assert_eq!(claims["sub"], SUBJECT);
    assert_eq!(
        claims["scopes"],
        serde_json::json!(["openid"]),
        "the granted scopes ride on the bearer"
    );
}

#[tokio::test]
async fn a_corrupted_pkce_challenge_fails_the_login_and_persists_nothing() {
    require_curl();
    // Same flow, one thing changed: the IdP stores a challenge that no
    // verifier can satisfy. If this test ever goes green-with-a-token, the
    // PKCE check is decorative and the test above proves nothing.
    let idp = MockIdp::spawn(|base| idp_config(base, true))
        .await
        .expect("start the sabotaged mock IdP");
    let http = reqwest::Client::new();
    let endpoints = endpoints_via_discovery(&idp, &http).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let store = CredentialStore::file_at(tmp.path());
    let opener = Opener::from_raw(Some(CURL_OPENER)).expect("a well-formed opener command");

    let err = oidc::login(&http, &store, &endpoints, SUBJECT, TIMEOUT, |url| {
        opener.open(url);
    })
    .await
    .expect_err("a login whose PKCE verifier cannot match must fail");

    let rendered = err.to_string();
    assert!(
        rendered.contains("invalid_grant"),
        "the failure must be the token endpoint's PKCE refusal, got: {rendered}"
    );

    let key = endpoints.credential_key(SUBJECT);
    assert!(
        store.load(&key).expect("read the store back").is_none(),
        "a refused login must leave nothing behind"
    );
}
