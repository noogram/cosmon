// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for the three artifact endpoints (e653 spec).
//!
//! Each test mounts an [`AppState`] pointed at a temp artifact root,
//! seeds files on disk, mints a scoped JWT through the OIDC mock,
//! and exercises one route end-to-end:
//!
//! - `GET /v1/molecules/{id}/artifacts` — manifest shape, sorting,
//!   missing-dir → empty.
//! - `GET /v1/molecules/{id}/artifacts/{token}` — binary stream,
//!   `ETag` = blake3, 404 on unknown token.
//! - `PUT /v1/molecules/{id}/artifacts/{name}` — write file,
//!   blake3 verification, `If-Match` precondition, scope gating.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::fake_cs_path;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    oidc: OidcMock,
    _tenants: TenantWorkspaces,
    _security_dir: tempfile::TempDir,
    artifact_root: tempfile::TempDir,
    app: axum::Router,
}

async fn fixture() -> Fixture {
    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");

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
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
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

#[tokio::test]
async fn list_artifacts_returns_empty_manifest_when_dir_missing() {
    let fx = fixture().await;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:read"], "jti-list-empty");

    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-empty/artifacts")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 8 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["molecule_id"], "task-empty");
    assert!(body["artifacts"].as_array().unwrap().is_empty());
    assert!(body.get("request_id").is_some());
}

#[tokio::test]
async fn list_artifacts_projects_disk_files_as_manifest() {
    let fx = fixture().await;
    let dir = fx.artifact_root.path().join("a").join("task-with-art");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("haiku.txt"), b"line one\nline two\n").unwrap();
    std::fs::write(dir.join("report.md"), b"# title\n").unwrap();
    std::fs::write(dir.join(".hidden"), b"skipped").unwrap();
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:read"], "jti-list-2");

    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-with-art/artifacts")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 8 * 1024).await.unwrap()).unwrap();
    let arr = body["artifacts"].as_array().unwrap();
    assert_eq!(arr.len(), 2, "dotfiles must be skipped");
    assert_eq!(arr[0]["name"], "haiku.txt");
    assert_eq!(arr[1]["name"], "report.md");
    assert_eq!(arr[0]["content_type"], "text/plain");
    assert_eq!(arr[0]["size_bytes"], 18);
    assert_eq!(arr[0]["integrity"]["algo"], "blake3");
    assert!(arr[0]["token"].as_str().unwrap().starts_with("art_"));
}

#[tokio::test]
async fn list_artifacts_without_scope_returns_403() {
    let fx = fixture().await;
    // Only the molecule scope — not artifact:read.
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "jti-list-403");

    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-403/artifacts")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn fetch_artifact_streams_binary_with_etag() {
    let fx = fixture().await;
    let dir = fx.artifact_root.path().join("a").join("task-fetch");
    std::fs::create_dir_all(&dir).unwrap();
    let content = b"the body";
    std::fs::write(dir.join("payload.bin"), content).unwrap();
    let expected_hex = blake3::hash(content).to_hex().to_string();

    // First list to get the token.
    let list_jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:read"], "jti-fetch-list");
    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-fetch/artifacts")
                .header("Authorization", format!("Bearer {list_jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 8 * 1024).await.unwrap()).unwrap();
    let token = body["artifacts"][0]["token"].as_str().unwrap().to_owned();

    // Then fetch.
    let fetch_jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:read"], "jti-fetch-get");
    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/molecules/task-fetch/artifacts/{token}"))
                .header("Authorization", format!("Bearer {fetch_jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some(expected_hex.as_str())
    );
    let body_bytes = to_bytes(resp.into_body(), 8 * 1024).await.unwrap();
    assert_eq!(body_bytes.as_ref(), content);
}

#[tokio::test]
async fn fetch_artifact_unknown_token_returns_404() {
    let fx = fixture().await;
    let dir = fx.artifact_root.path().join("a").join("task-fetch-404");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("anything.txt"), b"x").unwrap();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:read"], "jti-fetch-404");
    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/molecules/task-fetch-404/artifacts/art_doesnotexistdoesnotexist")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn push_artifact_writes_file_and_returns_envelope() {
    let fx = fixture().await;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:write"], "jti-push-1");
    let payload = b"haiku\n";
    let hex = blake3::hash(payload).to_hex().to_string();

    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/molecules/task-push/artifacts/haiku.txt")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "text/plain")
                .header("Digest", format!("blake3={hex}"))
                .body(Body::from(payload.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 8 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["artifact"]["name"], "haiku.txt");
    assert_eq!(body["artifact"]["content_type"], "text/plain");
    assert_eq!(body["artifact"]["size_bytes"], payload.len() as u64);
    assert_eq!(body["artifact"]["integrity"]["algo"], "blake3");
    assert_eq!(body["artifact"]["integrity"]["hex"], hex);

    // File should now exist on disk under the per-molecule dir.
    let on_disk = fx
        .artifact_root
        .path()
        .join("a")
        .join("task-push")
        .join("haiku.txt");
    assert!(on_disk.is_file(), "PUT must materialise the file");
    assert_eq!(std::fs::read(&on_disk).unwrap(), payload);
}

#[tokio::test]
async fn push_artifact_rejects_digest_mismatch() {
    let fx = fixture().await;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:write"], "jti-push-bad");
    let payload = b"real body";
    let wrong_hex = blake3::hash(b"different body").to_hex().to_string();

    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/molecules/task-push-bad/artifacts/x.bin")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/octet-stream")
                .header("Digest", format!("blake3={wrong_hex}"))
                .body(Body::from(payload.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // The file must NOT have been written.
    let on_disk = fx
        .artifact_root
        .path()
        .join("a")
        .join("task-push-bad")
        .join("x.bin");
    assert!(
        !on_disk.exists(),
        "digest_mismatch must not have written the file"
    );
}

#[tokio::test]
async fn push_artifact_without_scope_returns_403() {
    let fx = fixture().await;
    // Has artifact:read but not :write.
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:read"], "jti-push-403");

    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/molecules/task-push-403/artifacts/whatever.txt")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::from(b"x".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn push_artifact_if_match_succeeds_on_correct_digest() {
    let fx = fixture().await;
    let dir = fx.artifact_root.path().join("a").join("task-ifmatch");
    std::fs::create_dir_all(&dir).unwrap();
    let original = b"v1 contents";
    std::fs::write(dir.join("doc.txt"), original).unwrap();
    let original_hex = blake3::hash(original).to_hex().to_string();
    let new_payload = b"v2 contents";
    let new_hex = blake3::hash(new_payload).to_hex().to_string();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:write"], "jti-ifmatch-ok");
    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/molecules/task-ifmatch/artifacts/doc.txt")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "text/plain")
                .header("Digest", format!("blake3={new_hex}"))
                .header("If-Match", format!("\"{original_hex}\""))
                .body(Body::from(new_payload.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(std::fs::read(dir.join("doc.txt")).unwrap(), new_payload);
}

#[tokio::test]
async fn push_artifact_if_match_fails_on_stale_digest() {
    let fx = fixture().await;
    let dir = fx.artifact_root.path().join("a").join("task-ifmatch-bad");
    std::fs::create_dir_all(&dir).unwrap();
    let original = b"on disk";
    std::fs::write(dir.join("doc.txt"), original).unwrap();
    let new_payload = b"would overwrite";
    let new_hex = blake3::hash(new_payload).to_hex().to_string();

    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:artifact:write"], "jti-ifmatch-fail");
    let resp = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/molecules/task-ifmatch-bad/artifacts/doc.txt")
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "text/plain")
                .header("Digest", format!("blake3={new_hex}"))
                .header("If-Match", "\"stale-hex-that-never-matches\"")
                .body(Body::from(new_payload.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);
    // File contents must be unchanged.
    assert_eq!(std::fs::read(dir.join("doc.txt")).unwrap(), original);
}

#[tokio::test]
async fn artifact_routes_require_jwt() {
    let fx = fixture().await;
    for (method, path) in [
        ("GET", "/v1/molecules/task-x/artifacts"),
        ("GET", "/v1/molecules/task-x/artifacts/art_anything"),
        ("PUT", "/v1/molecules/task-x/artifacts/anything.txt"),
    ] {
        let resp = fx
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} must require JWT"
        );
    }
}
