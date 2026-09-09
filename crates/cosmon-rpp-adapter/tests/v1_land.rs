// SPDX-License-Identifier: AGPL-3.0-only

//! The harvest door — `POST /v1/molecules/:id/land` integration tests
//! (ADR-176, issue #51; library-direct decision half, issue #54 U3).
//!
//! Each test pins one property the deliberation made non-negotiable, and
//! each is written so that it fails for the *reason* it exists rather than
//! incidentally:
//!
//! 1. A landed harvest answers **200, never 202**. A 202 on a transaction
//!    that may integrate nothing rebuilds the defect issue #51 reports.
//! 2. The body carries **no options**. A request with any body at all is
//!    refused `unsupported_parameter`.
//! 3. The four **pre-effect** refusals — and `already_landed`
//!    idempotence — are decided in-process from the tenant's own state
//!    files, with **no `cs` binary involved** (issue #54 U3), each under
//!    its own label. Since U6 the subprocess effect is retired: a harvest
//!    the decision half ADMITS answers the typed refusal
//!    `501 land_effect_unavailable` until the sealed transaction grows a
//!    library implementation (ADR-176 §11).
//! 4. The door needs `cosmon:molecule:write` and **not** the
//!    `worker:spawn` composition that `tackle` and `run` carry — because
//!    auto-propel is disarmed here, so no agent budget is spent.
//! 5. Idempotence: a repeat request reports `already_landed`, success.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_core::harvest_door::ALL_REFUSALS;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantPath, TenantWorkspaces};
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
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(std::sync::Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
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

/// Arm `[harvest_authority] required` in the tenant's tracked config —
/// the second key the in-process decision half reads (ADR-176 D1).
fn arm_harvest_authority(tenant: &TenantPath) {
    let cosmon_dir = tenant.root.join(".cosmon");
    std::fs::create_dir_all(&cosmon_dir).unwrap();
    std::fs::write(
        cosmon_dir.join("config.toml"),
        "[harvest_authority]\nrequired = true\n",
    )
    .unwrap();
}

/// Plant a `Completed`, unmerged molecule — the one shape whose harvest is
/// admissible past the whole in-process decision half.
fn plant_completed(tenant: &TenantPath, id: &str) {
    tenant
        .insert_molecule(id, &json!({"status": "completed"}))
        .unwrap();
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
    arm_harvest_authority(&tenant_a);
    plant_completed(&tenant_a, "task-20260901-land");

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-1");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-land", Body::empty()))
        .await
        .unwrap();

    // U6: an ADMITTED harvest is answered with the typed effect refusal —
    // synchronously, with the truth. Anything but a 202 keeps the issue
    // #51 property; a 202 would claim acceptance of a transaction that
    // integrates nothing.
    assert_ne!(
        resp.status(),
        StatusCode::ACCEPTED,
        "a 202 here rebuilds the silent failure of issue #51",
    );
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.get("request_id").is_some());
    assert_eq!(body["error"], "land_effect_unavailable");
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
    arm_harvest_authority(&tenant_a);
    plant_completed(&tenant_a, "task-20260901-empty");

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-empty");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-empty", Body::from("{}")))
        .await
        .unwrap();
    // Past the parameter gate: the empty object reaches the effect
    // boundary (whose U6 answer is the typed refusal), never a 400.
    assert_ne!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
}

/// The four pre-effect refusals, decided **in-process** from the tenant's
/// own state files — issue #54 U3.
///
/// No `land-exit` pin is written anywhere in this test: the fake `cs`
/// would answer success if it were reached, so a refusal arriving on the
/// wire proves the library decision half produced it without a subprocess.
/// Each case plants the real state shape that owns the refusal.
#[tokio::test]
async fn the_pre_effect_refusals_are_decided_in_process_from_real_state() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_harvest_authority(&tenant_a);

    // not_completed: work still in flight.
    tenant_a
        .insert_molecule("task-20260901-flight", &json!({"status": "running"}))
        .unwrap();
    // reservation_requires_seal: a condition only a human lifts.
    tenant_a
        .insert_molecule(
            "task-20260901-held",
            &json!({"status": "completed", "tags": ["needs-review"]}),
        )
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-inproc");

    for (id, label, status) in [
        (
            "task-20260901-flight",
            "not_completed",
            StatusCode::CONFLICT,
        ),
        (
            "task-20260901-held",
            "reservation_requires_seal",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let resp = app
            .clone()
            .oneshot(land_request(&jwt, id, Body::empty()))
            .await
            .unwrap();
        assert_eq!(resp.status(), status, "{id} answered the wrong status");
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], label, "{id} lost its refusal name");
    }
}

/// `not_authorized`, fail-closed and in-process: a tenant galaxy that has
/// not armed `[harvest_authority] required` refuses every harvest — the
/// bearer token authenticates, it never authorises (ADR-176 D1).
#[tokio::test]
async fn an_unarmed_tenant_galaxy_refuses_not_authorized_in_process() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    // Deliberately NOT armed, and the molecule is otherwise landable.
    plant_completed(&tenant_a, "task-20260901-cold");

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-cold");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-cold", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "not_authorized");
}

/// `backlog_full`, in-process: past the sealed ceiling of
/// closed-but-unintegrated molecules the door refuses by name — a bounded,
/// refusing queue beats a silent block (ADR-176 D7, ADR-110 I4).
#[tokio::test]
async fn a_full_backlog_refuses_in_process() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_harvest_authority(&tenant_a);
    let ceiling = cosmon_core::config::HarvestAuthorityConfig::DEFAULT_BACKLOG_CEILING;
    for i in 0..ceiling {
        tenant_a
            .insert_molecule(
                &format!("task-20260901-b{i:03}"),
                &json!({
                    "status": "completed",
                    "non_integration": {
                        "reason": "pre-done-refused",
                        "at": chrono::Utc::now().to_rfc3339(),
                        "base_branch": "main",
                        "detail": null,
                    },
                }),
            )
            .unwrap();
    }
    plant_completed(&tenant_a, "task-20260901-ninth");

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-full");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-ninth", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "backlog_full");
}

/// The effect half is a TYPED refusal (issue #54 U6): a harvest the
/// decision half admits answers `501 land_effect_unavailable` — never a
/// silent subprocess fallback, never an invented outcome. The three
/// execution refusals (`merge_conflict`, `base_not_fast_forward`,
/// `pre_done_refused`) belong to the sealed transaction and return with
/// its library implementation (ADR-176 §11); until then this label is
/// the one honest answer past the decision half.
#[tokio::test]
async fn an_admitted_harvest_answers_the_typed_effect_refusal() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_harvest_authority(&tenant_a);
    plant_completed(&tenant_a, "task-20260901-refuse");

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-refusals");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-refuse", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "land_effect_unavailable");
    // The label is NOT one of the door's seven named refusals: the
    // closed set stays closed, and the parity gap has its own name.
    for refusal in ALL_REFUSALS {
        assert_ne!(body["error"], refusal.as_str());
    }
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
    arm_harvest_authority(&tenant_a);
    plant_completed(&tenant_a, "task-20260901-scope");

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
    // Write scope opens the door: the request reaches the effect
    // boundary (whose U6 answer is the typed refusal), never a 403.
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

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

/// Idempotence, decided **in-process** from the molecule's own record —
/// no fake-`cs` pin, no subprocess.
///
/// The second request must report the same success as the first and mutate
/// nothing — `already_landed` rather than a second merge or a refusal.
#[tokio::test]
async fn a_repeat_request_reports_already_landed_in_process() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_harvest_authority(&tenant_a);
    tenant_a
        .insert_molecule(
            "task-20260901-again",
            &json!({
                "status": "completed",
                "merged_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-again");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-again", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["harvest"]["outcome"], "already_landed");
}

/// The issue #54 claim itself: the decision half and idempotence answer
/// correctly against an image that carries **no `cs` binary at all** —
/// since U6 the adapter has no `cs` path to configure in the first
/// place, so a refusal or an `already_landed` success arriving on the
/// wire can only have been produced in-process.
#[tokio::test]
async fn the_decision_half_needs_no_cs_binary_at_all() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_harvest_authority(&tenant_a);
    tenant_a
        .insert_molecule("task-20260901-nobin", &json!({"status": "running"}))
        .unwrap();
    tenant_a
        .insert_molecule(
            "task-20260901-nobin2",
            &json!({
                "status": "completed",
                "merged_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .unwrap();

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-nobin");

    let resp = app
        .clone()
        .oneshot(land_request(&jwt, "task-20260901-nobin", Body::empty()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "not_completed");

    let resp = app
        .oneshot(land_request(&jwt, "task-20260901-nobin2", Body::empty()))
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
/// enumeration channel across tenants. Covered for both a malformed id and
/// a well-formed id the tenant's store has never seen — the latter is
/// decided in-process by the library door since issue #54 U3.
#[tokio::test]
async fn an_unknown_molecule_is_not_an_existence_oracle() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    arm_harvest_authority(&tenant_a);

    let oidc = oidc_mock().await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = jwt_with(&oidc, &["cosmon:molecule:write"], "jti-land-404");

    for id in ["not-a-molecule-id", "task-20260901-ghost"] {
        let resp = app
            .clone()
            .oneshot(land_request(&jwt, id, Body::empty()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{id}");
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "not_found", "{id}");
    }
}
