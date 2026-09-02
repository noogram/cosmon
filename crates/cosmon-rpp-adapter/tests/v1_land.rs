// SPDX-License-Identifier: AGPL-3.0-only

//! The harvest door — `POST /v1/molecules/:id/land` integration tests
//! (ADR-176, issue #51).
//!
//! Each test pins one property the deliberation made non-negotiable, and
//! each is written so that it fails for the *reason* it exists rather than
//! incidentally:
//!
//! 1. A landed harvest answers **200, never 202**. A 202 on a transaction
//!    that may integrate nothing rebuilds the defect issue #51 reports.
//! 2. The body carries **no options**. A request with any body at all is
//!    refused `unsupported_parameter`.
//! 3. Every one of the seven named refusals survives the exit-code →
//!    label mirror intact, and `base_not_fast_forward` alone answers 5xx.
//! 4. The door needs `cosmon:molecule:write` and **not** the
//!    `worker:spawn` composition that `tackle` and `run` carry — because
//!    auto-propel is disarmed here, so no agent budget is spent.
//! 5. Idempotence: a repeat request reports `already_landed`, success.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_core::harvest_door::{DoorRefusal, ALL_REFUSALS};
use cosmon_oidc_testkit::{fake_cs_path, IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::{json, Value};
use tower::ServiceExt;

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: std::path::PathBuf::from("/tmp/cosmon"),
        dist: Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: Arc::new(cosmon_rpp_adapter::config::InstallTemplating::default()),
        events: Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: Arc::new(cosmon_rpp_adapter::portee::PorteeProvisioner::inert()),
    }
}

fn land_request(jwt: &str, id: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/molecules/{id}/land"))
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Content-Type", "application/json")
        .body(body)
        .unwrap()
}

fn jwt_with(oidc: &OidcMock, scopes: &[&str], jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes,
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

async fn oidc_mock() -> OidcMock {
    OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await
}

/// Path of the molecule directory the fake `cs` reads its pins from.
fn molecule_dir(tenants: &TenantWorkspaces, id: &str) -> std::path::PathBuf {
    tenants
        .tenant("a")
        .expect("tenant a")
        .state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(id)
}

/// The load-bearing status assertion of this route.
///
/// `/run` answers 202 because a drain is hours-shaped and the HTTP boundary
/// is a request door, not a progress cockpit. The harvest door must not
/// borrow that shape: a 202 here would tell the tenant "accepted" for a
/// transaction that may integrate nothing — which is precisely what issue
/// #51 reports happening today, through `run` step 9, with the failure
/// swallowed.
#[tokio::test]
async fn never_202_on_a_transaction_that_may_integrate_nothing() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260901-land", &json!({}))
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-1");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-land", Body::empty()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_ne!(
        resp.status(),
        StatusCode::ACCEPTED,
        "a 202 here rebuilds the silent failure of issue #51",
    );

    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.get("request_id").is_some());
    assert_eq!(body["harvest"]["molecule"], "task-20260901-land");
    assert_eq!(body["harvest"]["outcome"], "landed");
}

/// ADR-176 D4 on the wire: the body is inert.
///
/// The refusal is on the *channel*, not on a known field set — a struct
/// with `deny_unknown_fields` would accept whatever fields it knew about
/// the day someone added one.
#[tokio::test]
async fn a_request_body_is_refused_as_an_unsupported_parameter() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260901-body", &json!({}))
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-body");

    for payload in [
        r#"{"strategy":"ff-only"}"#,
        r#"{"force":true}"#,
        r#"{"skip_pre_done_hook":true}"#,
        r#"{"base":"main"}"#,
    ] {
        let resp = app
            .clone()
            .oneshot(land_request(
                &jwt,
                "task-20260901-body",
                Body::from(payload.to_owned()),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "payload {payload} must not reach the door",
        );
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "unsupported_parameter");
    }
}

/// An empty JSON object is the one non-empty body a well-behaved client
/// may send, and it must be accepted — refusing it would make the door
/// unusable from any HTTP library that always serialises a body.
#[tokio::test]
async fn an_empty_json_object_is_still_no_parameter() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260901-empty", &json!({}))
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-empty");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-empty", Body::from("{}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// The exit-code → label mirror, walked over the whole closed set.
///
/// A refusal that lost its name between the CLI and the wire would reach
/// the tenant as an anonymous 500, which is the unnamed refusal ADR-110 I4
/// forbids. Walking `ALL_REFUSALS` rather than a hand-written list means a
/// new variant joins this test by existing.
#[tokio::test]
async fn every_named_refusal_survives_the_exit_code_mirror() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260901-refuse", &json!({}))
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-refusals");
    let dir = molecule_dir(&tenants, "task-20260901-refuse");

    for refusal in ALL_REFUSALS {
        std::fs::write(dir.join("land-exit"), refusal.exit_code().to_string()).unwrap();

        let resp = app
            .clone()
            .oneshot(land_request(&jwt, "task-20260901-refuse", Body::empty()))
            .await
            .unwrap();

        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["error"],
            refusal.as_str(),
            "{refusal:?} lost its name on the wire",
        );
        assert!(
            status.is_client_error() || status.is_server_error(),
            "{refusal:?} must not answer a success status",
        );

        // ADR-176 D7: `base_not_fast_forward` is an operator configuration
        // fault, decidable at arming time. It is the ONLY refusal the
        // requester is not charged for.
        assert_eq!(
            status.is_server_error(),
            refusal.is_operator_configuration_fault(),
            "{refusal:?} is on the wrong side of the 4xx/5xx line",
        );
    }
}

/// An exit code the closed set does not own must not acquire a name.
///
/// Inventing one would be the unnamed refusal wearing a label — worse than
/// an honest 500, because a client would branch on it.
#[tokio::test]
async fn an_unknown_exit_code_stays_anonymous() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260901-weird", &json!({}))
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-weird");
    let dir = molecule_dir(&tenants, "task-20260901-weird");
    std::fs::write(dir.join("land-exit"), "42").unwrap();

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-weird", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "harvest_failed");
    assert!(
        DoorRefusal::from_exit_code(42).is_none(),
        "42 must stay outside the closed set for this test to mean anything",
    );
}

/// The door does not require `cosmon:worker:spawn`.
///
/// That composition exists on `tackle` and `run` because those verbs burn
/// Anthropic credit. The harvest door does not: auto-propel — the one path
/// that would have spent agent budget — is disarmed by construction
/// (ADR-176 D6). Requiring the spawn scope here would assert a spend that
/// must never happen, and would quietly become true if escalation were
/// re-armed.
#[tokio::test]
async fn write_scope_alone_opens_the_door() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260901-scope", &json!({}))
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));

    let write_only = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-write");
    let resp = app
        .clone()
        .oneshot(land_request(
            &write_only,
            "task-20260901-scope",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // And read alone does not: landing is a write on the molecule and on
    // the trunk it resolves against.
    let read_only = jwt_with(&oidc, &["cosmon:molecule:read"], "jti-land-read");
    let resp = app
        .oneshot(land_request(
            &read_only,
            "task-20260901-scope",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Idempotence, which is what makes a retry over a lossy network safe.
///
/// The second request must report the same success as the first and mutate
/// nothing — `already_landed` rather than a second merge or a refusal.
#[tokio::test]
async fn a_repeat_request_reports_already_landed() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260901-again", &json!({}))
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-again");
    let dir = molecule_dir(&tenants, "task-20260901-again");
    std::fs::write(dir.join("land-outcome"), "already_landed").unwrap();

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-again", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["harvest"]["outcome"], "already_landed");
}

/// A molecule id that does not resolve answers 404 and says nothing more.
///
/// The §8p boundary offers no existence oracle: "no such molecule" and
/// "not yours" must be indistinguishable, or the route becomes an
/// enumeration channel across tenants.
#[tokio::test]
async fn an_unknown_molecule_is_not_an_existence_oracle() {
    let mut tenants = TenantWorkspaces::new();
    tenants.add("a");

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-404");

    let resp = app
        .oneshot(land_request(&jwt, "not-a-molecule-id", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "not_found");
}
