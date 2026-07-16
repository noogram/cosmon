// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for `GET /v1/molecules/{id}/result`.
//!
//! These exercise the tackled-output restitution contract end-to-end:
//! seed a molecule in the *persistent* tenant store, optionally drop a
//! canonical deliverable in the molecule dir or the ephemeral artifact
//! dir, then prove the tenant retrieves the output through the API.
//!
//! The motivating failure (Tenant-Demo v1.7, 2026-06-05): a tackled
//! `task-work` molecule completes but `GET .../artifacts` returns `[]`
//! and the GET molecule carries no result. `result` closes that by
//! reading the persistent molecule dir (where panel formulas already
//! write `synthesis.md`) with the artifact dir as fallback.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::fake_cs_path;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantPath, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    router, AppState, BackendHealthRegistry, JwksStore, Posture, SharedHabilitationMap,
};
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    oidc: OidcMock,
    tenant: TenantPath,
    _tenants: TenantWorkspaces,
    _security_dir: tempfile::TempDir,
    artifact_root: tempfile::TempDir,
    app: axum::Router,
}

async fn fixture() -> Fixture {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let artifact_root = tempfile::tempdir().unwrap();
    let _ = oidc.write_jwks_file(security_dir.path()).unwrap();
    let jwks = JwksStore::load(security_dir.path()).unwrap();

    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();
    let rate_limiter =
        IngressRateLimiter::new(security_dir.path().join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list =
        DenyList::new(security_dir.path().to_path_buf()).with_ttl(Duration::from_secs(0));

    let state = AppState {
        cs_path: fake_cs_path(),
        state_dir: security_dir.path().to_path_buf(),
        inbox_root: security_dir.path().join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: artifact_root.path().to_path_buf(),
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: std::sync::Arc::new(
            cosmon_rpp_adapter::config::InstallTemplating::default(),
        ),
        events: std::sync::Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
    };
    let app = router(state);
    Fixture {
        oidc,
        tenant,
        _tenants: tenants,
        _security_dir: security_dir,
        artifact_root,
        app,
    }
}

fn jwt_with_scopes(oidc: &OidcMock, scopes: &[&str], jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes,
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

/// Absolute path of the persistent molecule dir for a `default`-fleet
/// molecule under tenant `a`.
fn molecule_dir(tenant: &TenantPath, id: &str) -> std::path::PathBuf {
    tenant
        .state_dir
        .join("fleets")
        .join("default")
        .join("molecules")
        .join(id)
}

async fn get_result(fx: &Fixture, id: &str, jwt: &str) -> axum::http::Response<Body> {
    fx.app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/molecules/{id}/result"))
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn result_reads_synthesis_md_from_persistent_dir() {
    // The deep-think / panel case: synthesis.md is written to the
    // persistent molecule dir by the formula, so result works with
    // ZERO worker change.
    let fx = fixture().await;
    let id = "task-20260605-synt";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({"status": "completed"}))
        .unwrap();
    let dir = molecule_dir(&fx.tenant, id);
    std::fs::write(dir.join("synthesis.md"), b"# Panel verdict\nship it\n").unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-synt");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["molecule_id"], id);
    assert_eq!(body["status"], "completed");
    assert_eq!(body["result"]["source"], "synthesis.md");
    assert_eq!(body["result"]["encoding"], "utf8");
    assert_eq!(body["result"]["content"], "# Panel verdict\nship it\n");
    assert_eq!(body["result"]["integrity"]["algo"], "blake3");
}

#[tokio::test]
async fn result_reads_haiku_from_artifact_dir() {
    // The onboarding case (Dave's haiku): a task-work worker that
    // honoured the formula contract dropped result.md in
    // COSMON_ARTIFACT_DIR. The molecule dir holds no canonical file.
    let fx = fixture().await;
    let id = "task-20260605-haik";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({"status": "completed"}))
        .unwrap();
    let art_dir = fx.artifact_root.path().join("a").join(id);
    std::fs::create_dir_all(&art_dir).unwrap();
    let haiku = "quarks in the deep night\ngluon strings hum and confine\ncolor never seen\n";
    std::fs::write(art_dir.join("result.md"), haiku.as_bytes()).unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-haik");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result"]["source"], "artifact:result.md");
    assert_eq!(body["result"]["content"], haiku);
}

#[tokio::test]
async fn result_single_artifact_file_is_returned() {
    // A worker that wrote exactly one file (any name) under the
    // artifact dir — unambiguous, so it is the result.
    let fx = fixture().await;
    let id = "task-20260605-1fil";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({}))
        .unwrap();
    let art_dir = fx.artifact_root.path().join("a").join(id);
    std::fs::create_dir_all(&art_dir).unwrap();
    std::fs::write(art_dir.join("answer.txt"), b"42").unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-1fil");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result"]["source"], "artifact:answer.txt");
    assert_eq!(body["result"]["content"], "42");
}

#[tokio::test]
async fn result_persistent_dir_wins_over_artifact_dir() {
    // Precedence: result.md in the persistent molecule dir beats both
    // synthesis.md and anything in the ephemeral artifact dir.
    let fx = fixture().await;
    let id = "task-20260605-prec";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({}))
        .unwrap();
    let dir = molecule_dir(&fx.tenant, id);
    std::fs::write(dir.join("result.md"), b"canonical").unwrap();
    std::fs::write(dir.join("synthesis.md"), b"panel").unwrap();
    let art_dir = fx.artifact_root.path().join("a").join(id);
    std::fs::create_dir_all(&art_dir).unwrap();
    std::fs::write(art_dir.join("result.md"), b"ephemeral").unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-prec");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result"]["source"], "result.md");
    assert_eq!(body["result"]["content"], "canonical");
}

#[tokio::test]
async fn result_pending_when_no_canonical_output() {
    // Molecule exists (default `pending`, never tackled) and produced no
    // canonical deliverable. The old contract answered a silent 404;
    // C1 answers 200 with `result_status: pending` and `result: null`
    // so the client knows to *wait*, not relaunch.
    let fx = fixture().await;
    let id = "task-20260605-none";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({}))
        .unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-none");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result_status"], "pending");
    assert!(body["result"].is_null());
    // Liveness block is always present (refus d'opacité F4).
    assert!(body["liveness"]["stale_after_s"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn result_404_when_molecule_absent() {
    // No such molecule — STILL 404, no existence oracle distinction. The
    // always-200 contract applies only to molecules that exist; an
    // absent (or cross-tenant) molecule preserves the turing invariant.
    let fx = fixture().await;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-absent");
    let resp = get_result(&fx, "task-20260605-gone", &jwt).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn result_status_stalled_when_worker_killed_before_finish() {
    // THE gate scenario: tackle a molecule, kill the worker before it
    // finishes. The persisted shape that leaves behind is `running` with
    // a stale `tackled_at` and no deliverable on disk. Asserts the
    // endpoint says `stalled` — not 404, not `ready`.
    let fx = fixture().await;
    let id = "task-20260614-kill";
    fx.tenant
        .insert_molecule(
            id,
            &serde_json::json!({
                "status": "running",
                // Tackled far in the past — unambiguously past the
                // 15-min default decree for any plausible run time, so
                // no global env override (and its parallel-test race) is
                // needed.
                "tackled_at": "2020-01-01T00:00:00Z",
                "process": {
                    "worker_id": "w-kill",
                    "tmux_session": "sess-kill",
                    "started_at": "2020-01-01T00:00:00Z",
                    "status": "active",
                },
            }),
        )
        .unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-kill");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result_status"], "stalled");
    assert!(body["result"].is_null());
    assert_eq!(body["liveness"]["tackled_at"], "2020-01-01T00:00:00Z");
}

#[tokio::test]
async fn result_status_done_no_deliverable_never_ready() {
    // GARDE-FOU: `completed` with no file on disk is
    // `done-no-deliverable`, never `ready`. A worker can write
    // `completed` without depositing anything.
    let fx = fixture().await;
    let id = "task-20260614-empty";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({ "status": "completed" }))
        .unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-empty");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result_status"], "done-no-deliverable");
    assert!(body["result"].is_null());
}

#[tokio::test]
async fn result_status_ready_proven_by_disk() {
    // `completed` AND a file on disk → `ready`, body carries it.
    let fx = fixture().await;
    let id = "task-20260614-ready";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({ "status": "completed" }))
        .unwrap();
    let dir = molecule_dir(&fx.tenant, id);
    std::fs::write(dir.join("synthesis.md"), b"verdict\n").unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-ready");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result_status"], "ready");
    assert_eq!(body["result"]["content"], "verdict\n");
}

#[tokio::test]
async fn result_status_failed_when_collapsed() {
    // A collapsed run → `failed` (relaunch), not 404.
    let fx = fixture().await;
    let id = "task-20260614-fail";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({ "status": "collapsed" }))
        .unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-fail");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["result_status"], "failed");
}

#[tokio::test]
async fn result_without_scope_returns_403() {
    let fx = fixture().await;
    let id = "task-20260605-scop";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({}))
        .unwrap();
    let dir = molecule_dir(&fx.tenant, id);
    std::fs::write(dir.join("synthesis.md"), b"secret").unwrap();

    // artifact:read alone must NOT unlock the molecule's result.
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:read"], "jti-scop");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn result_write_scope_implies_read() {
    let fx = fixture().await;
    let id = "task-20260605-wrt";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({}))
        .unwrap();
    let dir = molecule_dir(&fx.tenant, id);
    std::fs::write(dir.join("synthesis.md"), b"ok").unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "jti-wrt");
    let resp = get_result(&fx, id, &jwt).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn result_requires_jwt() {
    let fx = fixture().await;
    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-x/result")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
