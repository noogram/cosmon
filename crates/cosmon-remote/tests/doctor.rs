// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-remote doctor` — gate « chaque check falsifiable
//! indépendamment ».
//!
//! Every named check is exercised in BOTH states: the all-green run,
//! and one provoked red per check with a single root cause — the other
//! checks must stay green or report `Skipped` (a pointer to the real
//! cause), never duplicate the red. That anti-cascade property is the
//! discipline: one cause, one red line.

use chrono::{Duration as ChronoDuration, Utc};
use cosmon_remote::config::Profile;
use cosmon_remote::credential::{CredentialStore, SecretToken, StoredCredential};
use cosmon_remote::doctor::{
    self, Outcome, CHECK_CREDENTIAL, CHECK_DISCOVERY, CHECK_HOST, CHECK_OIDC, CHECK_PROFILE,
    CHECK_REGISTRY, CHECK_TENANT_BADGE, CHECK_WORKER_GLASSES,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A **mock** profile: no `login` recorded (`issuer`/`client_id` absent), so
/// `doctor` takes the `/issue` mint branch.
fn profile_for(server: &MockServer) -> Profile {
    Profile {
        host: server.uri(),
        sub: "tenant-demo-operator".into(),
        aud: "cosmon-rpp-tenant".into(),
        oidc_url: server.uri(),
        issuer: None,
        client_id: None,
        noyau: Some("default".into()),
        scopes: vec![
            "cosmon:molecule:read".into(),
            "cosmon:molecule:write".into(),
        ],
        artifacts_dir: None,
        timeout_secs: 5,
        phone_home: true,
    }
}

/// A **real-OIDC** profile: a `login` recorded `issuer` + `client_id`, so
/// `doctor` reads the credential store instead of minting via `/issue`.
fn real_oidc_profile(server: &MockServer) -> Profile {
    Profile {
        issuer: Some(server.uri()),
        client_id: Some("cosmon-rpp-tenant".into()),
        ..profile_for(server)
    }
}

/// An empty file-backed store in a fresh temp dir — the injected credential
/// port. Returned alongside its `TempDir` guard so the caller keeps it alive.
fn empty_store() -> (tempfile::TempDir, CredentialStore) {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = CredentialStore::file_at(tmp.path());
    (tmp, store)
}

/// Persist a credential for `profile` whose access token expires `ttl` from now.
/// A positive `ttl` well past the refresh leeway is `Fresh`; a tiny/negative one
/// is `Stale`. The refresh token is what makes an expiring access token `Stale`
/// rather than a static (never-refreshed) bearer.
fn store_credential(store: &CredentialStore, profile: &Profile, ttl: ChronoDuration) {
    let key = profile.credential_key().unwrap();
    store
        .store(
            &key,
            &StoredCredential::new(
                SecretToken::new("cached-access-token"),
                SecretToken::new("cached-refresh-token"),
                Utc::now() + ttl,
            ),
        )
        .unwrap();
}

fn mount_openid_config(server: &MockServer) -> impl std::future::Future<Output = ()> + '_ {
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
        })))
        .mount(server)
}

fn mount_registry<'a>(
    server: &'a MockServer,
    aud: &str,
) -> impl std::future::Future<Output = ()> + 'a {
    // Build the body eagerly so the returned future borrows only `server`.
    let body = json!({
        "schema_version": 2,
        "issuer": server.uri(),
        "clients": [{"audience": aud, "client_id": "cosmon-rpp-tenant"}],
    });
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
}

fn mount_healthz(server: &MockServer) -> impl std::future::Future<Output = ()> + '_ {
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "service": "mock"})),
        )
        .mount(server)
}

fn mount_issue(server: &MockServer) -> impl std::future::Future<Output = ()> + '_ {
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"access_token": "tok.abc.def"})),
        )
        .mount(server)
}

fn auth_me_body(noyau: Option<&str>, glasses: Option<bool>) -> serde_json::Value {
    json!({
        "sub": "tenant-demo-operator",
        "aud": ["cosmon-rpp-tenant"],
        "scopes": ["cosmon:molecule:read"],
        "noyau": noyau,
        "expires_at": "2026-06-10T12:00:00Z",
        "issuer": "https://mock-issuer",
        "claude_credentials_present": glasses,
    })
}

fn mount_auth_me<'a>(
    server: &'a MockServer,
    body: &serde_json::Value,
) -> impl std::future::Future<Output = ()> + 'a {
    Mock::given(method("GET"))
        .and(path("/v1/auth/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
        .mount(server)
}

fn outcome_of(report: &doctor::DoctorReport, name: &str) -> Outcome {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("check {name} missing from report"))
        .outcome
}

fn fix_of(report: &doctor::DoctorReport, name: &str) -> String {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| c.fix.clone())
        .unwrap_or_default()
}

/// Whether a check appears at all — the real-OIDC path *omits* the checks that
/// are not on its current road (no `oidc-mint`; no acquisition probes when the
/// session is fresh), so presence is itself an assertion.
fn has_check(report: &doctor::DoctorReport, name: &str) -> bool {
    report.checks.iter().any(|c| c.name == name)
}

// ── Green path ──────────────────────────────────────────────────────

#[tokio::test]
async fn all_checks_green_when_everything_is_up() {
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_issue(&server).await;
    mount_auth_me(
        &server,
        &auth_me_body(Some("tenant-demo-sandbox"), Some(true)),
    )
    .await;

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile_for(&server), &store).await;
    assert!(report.healthy(), "report: {report:?}");
    for name in [
        CHECK_PROFILE,
        CHECK_HOST,
        CHECK_OIDC,
        CHECK_TENANT_BADGE,
        CHECK_WORKER_GLASSES,
    ] {
        assert_eq!(outcome_of(&report, name), Outcome::Pass, "check {name}");
    }
}

// ── One provoked red per check, anti-cascade asserted ───────────────

#[tokio::test]
async fn incomplete_profile_reds_profile_only_and_host_still_probes() {
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    let mut profile = profile_for(&server);
    profile.sub.clear(); // the single provoked cause

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile, &store).await;
    assert!(!report.healthy());
    assert_eq!(outcome_of(&report, CHECK_PROFILE), Outcome::Fail);
    // host only needs `host` — it must still be probed and green, so a
    // missing sub can never mask (or be masked by) a network wall.
    assert_eq!(outcome_of(&report, CHECK_HOST), Outcome::Pass);
    // downstream checks are skipped, not red — one cause, one red line.
    assert_eq!(outcome_of(&report, CHECK_OIDC), Outcome::Skipped);
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Skipped);
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Skipped);
}

#[tokio::test]
async fn unreachable_host_reds_host_and_skips_badges() {
    // Point the profile at a port nothing listens on: the network wall
    // (Tailscale ACL absente, VPN down) seen from the client side.
    let server = MockServer::start().await;
    mount_issue(&server).await; // oidc stays up — its check must stay green
    let mut profile = profile_for(&server);
    profile.host = "http://127.0.0.1:1".into();

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile, &store).await;
    assert!(!report.healthy());
    assert_eq!(outcome_of(&report, CHECK_PROFILE), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_HOST), Outcome::Fail);
    assert!(fix_of(&report, CHECK_HOST).contains("Tailscale"));
    assert_eq!(outcome_of(&report, CHECK_OIDC), Outcome::Pass);
    // auth/me lives on the unreachable host → skipped, not a second red.
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Skipped);
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Skipped);
}

#[tokio::test]
async fn broken_oidc_reds_mint_only() {
    // The Dave wall n°2: oidc-url resolves to a server that refuses
    // to mint (templated for another host / dead issuer).
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile_for(&server), &store).await;
    assert!(!report.healthy());
    assert_eq!(outcome_of(&report, CHECK_PROFILE), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_HOST), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_OIDC), Outcome::Fail);
    assert!(fix_of(&report, CHECK_OIDC).contains("oidc-url"));
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Skipped);
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Skipped);
}

#[tokio::test]
async fn rejected_token_reds_tenant_badge() {
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_issue(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/auth/me"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": "audience_mismatch"})),
        )
        .mount(&server)
        .await;

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile_for(&server), &store).await;
    assert!(!report.healthy());
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Fail);
    assert!(fix_of(&report, CHECK_TENANT_BADGE).contains("config show"));
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Skipped);
}

#[tokio::test]
async fn unbound_principal_reds_tenant_badge_names_operator_gesture() {
    // The JWT is accepted but no noyau is bound — provisioning is an
    // operator gesture, and the fix line must say so instead of
    // sending the tenant in circles.
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_issue(&server).await;
    mount_auth_me(&server, &auth_me_body(None, Some(true))).await;

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile_for(&server), &store).await;
    assert!(!report.healthy());
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Fail);
    assert!(fix_of(&report, CHECK_TENANT_BADGE).contains("operator"));
    // worker glasses are still readable from the same response — and
    // they were fine, so they stay green (independent falsifiability).
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Pass);
}

#[tokio::test]
async fn missing_worker_glasses_reds_with_auth_login_fix() {
    // The two-badges trap (janis marche n°1): everything is green
    // except the worker's Claude login — the exact state that turns
    // into a 503 on the first tackle if doctor does not catch it first.
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_issue(&server).await;
    mount_auth_me(
        &server,
        &auth_me_body(Some("tenant-demo-sandbox"), Some(false)),
    )
    .await;

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile_for(&server), &store).await;
    assert!(!report.healthy());
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Fail);
    assert!(fix_of(&report, CHECK_WORKER_GLASSES).contains("auth login"));
}

// Issue #48's other half — *which* `claude_credentials_status` shapes *which*
// detail and *which* fix — is asserted in `doctor::tests` instead of here. That
// mapping is a pure function of the `/v1/auth/me` payload, so a mock server adds
// nothing to it while coupling the assertion to whatever acquisition shape the
// CLI currently uses. The wiring from `doctor::run` to that function is what
// this file covers, once, in `missing_worker_glasses_reds_with_auth_login_fix`.

#[tokio::test]
async fn older_server_without_signal_reports_unknown_not_green() {
    // An adapter that predates `claude_credentials_present` (or has no
    // auth-claude surface) → honest Unknown, never coerced to green or
    // red, and the report stays healthy (exit 0).
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_issue(&server).await;
    mount_auth_me(&server, &auth_me_body(Some("tenant-demo-sandbox"), None)).await;

    let (_tmp, store) = empty_store();
    let report = doctor::run(&profile_for(&server), &store).await;
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Unknown);
    assert!(fix_of(&report, CHECK_WORKER_GLASSES).contains("auth login"));
    assert!(report.healthy(), "Unknown alone must not fail the report");
}

// ── Real-OIDC shape — read-only, credential-state-gated ──────────────

#[tokio::test]
async fn real_oidc_fresh_credential_uses_authme_and_never_mints() {
    // The regression: a logged-in real-OIDC profile must read its cached token
    // and present it to /v1/auth/me — never POST /issue (which does not exist on
    // a real IdP). No `/issue` mock is mounted, so a mint attempt would fail; a
    // green report proves the mint path was not taken.
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_auth_me(
        &server,
        &auth_me_body(Some("tenant-demo-sandbox"), Some(true)),
    )
    .await;

    let (_tmp, store) = empty_store();
    let profile = real_oidc_profile(&server);
    store_credential(&store, &profile, ChronoDuration::hours(1)); // Fresh

    let report = doctor::run(&profile, &store).await;
    assert!(report.healthy(), "report: {report:?}");
    assert_eq!(outcome_of(&report, CHECK_CREDENTIAL), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Pass);
    // The mock-only line is absent, and a fresh session does not probe the
    // acquisition path — those endpoints are not on its road.
    assert!(!has_check(&report, CHECK_OIDC), "real-OIDC must not mint");
    assert!(!has_check(&report, CHECK_DISCOVERY));
    assert!(!has_check(&report, CHECK_REGISTRY));
}

#[tokio::test]
async fn real_oidc_cold_credential_probes_acquisition_and_skips_badges() {
    // No session cached: acceptance is untestable, so doctor tests whether a
    // `login` *would* succeed — discovery + registry — and skips the badges.
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_openid_config(&server).await;
    mount_registry(&server, "cosmon-rpp-tenant").await;

    let (_tmp, store) = empty_store(); // Cold
    let profile = real_oidc_profile(&server);

    let report = doctor::run(&profile, &store).await;
    assert!(!report.healthy());
    assert_eq!(outcome_of(&report, CHECK_CREDENTIAL), Outcome::Fail);
    assert!(fix_of(&report, CHECK_CREDENTIAL).contains("login"));
    assert_eq!(outcome_of(&report, CHECK_DISCOVERY), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_REGISTRY), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Skipped);
    assert_eq!(outcome_of(&report, CHECK_WORKER_GLASSES), Outcome::Skipped);
}

#[tokio::test]
async fn real_oidc_stale_credential_reds_credential_not_badges() {
    // A token past its refresh horizon is treated as "invalid locally": doctor
    // will not refresh it (that would spend the single-use refresh token), so it
    // reds `credential` and re-runs the acquisition probe instead of the badges.
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_openid_config(&server).await;
    mount_registry(&server, "cosmon-rpp-tenant").await;

    let (_tmp, store) = empty_store();
    let profile = real_oidc_profile(&server);
    store_credential(&store, &profile, ChronoDuration::seconds(1)); // Stale

    let report = doctor::run(&profile, &store).await;
    assert_eq!(outcome_of(&report, CHECK_CREDENTIAL), Outcome::Fail);
    assert!(fix_of(&report, CHECK_CREDENTIAL).contains("refresh"));
    assert_eq!(outcome_of(&report, CHECK_DISCOVERY), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_TENANT_BADGE), Outcome::Skipped);
}

#[tokio::test]
async fn real_oidc_dead_issuer_reds_discovery_only() {
    // The IdP wall, distinct from the cosmon host: a dead issuer reds discovery
    // while the registry (on the reachable host) stays green — one cause, one red.
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_registry(&server, "cosmon-rpp-tenant").await;
    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let (_tmp, store) = empty_store(); // Cold → acquisition probed
    let profile = real_oidc_profile(&server);

    let report = doctor::run(&profile, &store).await;
    assert_eq!(outcome_of(&report, CHECK_DISCOVERY), Outcome::Fail);
    assert!(fix_of(&report, CHECK_DISCOVERY).contains("oidc-url"));
    assert_eq!(outcome_of(&report, CHECK_REGISTRY), Outcome::Pass);
}

#[tokio::test]
async fn real_oidc_unprovisioned_audience_reds_registry_only() {
    // The registry is reachable and well-formed but lists no client for this
    // profile's audience — an operator provisioning gap, not a network wall.
    let server = MockServer::start().await;
    mount_healthz(&server).await;
    mount_openid_config(&server).await;
    Mock::given(method("GET"))
        .and(path("/.well-known/cosmon-oauth-clients"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "schema_version": 2,
            "issuer": server.uri(),
            "clients": [{"audience": "some-other-audience", "client_id": "zzz"}],
        })))
        .mount(&server)
        .await;

    let (_tmp, store) = empty_store(); // Cold → acquisition probed
    let profile = real_oidc_profile(&server);

    let report = doctor::run(&profile, &store).await;
    assert_eq!(outcome_of(&report, CHECK_DISCOVERY), Outcome::Pass);
    assert_eq!(outcome_of(&report, CHECK_REGISTRY), Outcome::Fail);
    assert!(fix_of(&report, CHECK_REGISTRY).contains("operator"));
}
