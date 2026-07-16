// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests for the operator-sealed admin provisioning surface.
//!
//! Drives the real `axum` router so the [`AdminSeal`] extractor, the
//! single-writer [`Provisioner`], and the in-process reload are
//! exercised end-to-end over HTTP. The load-bearing assertion (the B2
//! headline + definition-of-done) is
//! `nominal_provision_resolves_without_restart`: a binding sent via the
//! API resolves on the SAME live map the admission path reads — no
//! reboot, no SIGHUP.
//!
//! Scenarios (mirror the design's §8 T1/T4/T5/T6):
//! - T5 fail-closed: no seal at boot ⇒ `403 admin_disabled`.
//! - T4 tenant-can't-reach: seal enabled, missing/wrong header ⇒ `401`.
//! - T1 nominal: correct seal ⇒ `201`, binding resolves immediately.
//! - T6 idempotence: identical re-POST ⇒ `200`.
//! - reload route: re-read host-staged binding ⇒ `200`, resolves.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_rpp_adapter::admin_seal::AdminSeal;
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::image_init::ImageInit;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::provisioner::Provisioner;
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    router, AppState, BackendHealthRegistry, JwksStore, Posture, SharedHabilitationMap,
};
use serde_json::{json, Value};
use tower::ServiceExt;

const ADMIN_TOKEN: &str = "s3cret-operator-token";

/// Build an `AppState` whose admin seal is `seal` and whose provisioner
/// shares the returned [`SharedHabilitationMap`] handle with the admission
/// path — so a provision request mutates the very map `resolve` reads.
fn make_state(state_dir: &std::path::Path, seal: AdminSeal) -> (AppState, SharedHabilitationMap) {
    let security_dir = state_dir.join("security");
    std::fs::create_dir_all(&security_dir).unwrap();
    // Empty JWKS store — these routes never touch the JWT path. `load`
    // returns the default (empty) store when `security/jwks/` is absent.
    let jwks = JwksStore::load(state_dir).unwrap();

    let map = SharedHabilitationMap::new(HabilitationMap::default());
    let galaxies_root = state_dir.join("galaxies");
    std::fs::create_dir_all(&galaxies_root).unwrap();

    let image_init = ImageInit {
        inbox_root: state_dir.join("whispers/inbox"),
        galaxies_root: galaxies_root.clone(),
        cs_path: state_dir.join("nonexistent-cs"),
        claude_home: state_dir.join("home"),
        formulas_seed_dir: None,
    };
    let provisioner = Arc::new(Provisioner::new(
        state_dir.to_path_buf(),
        galaxies_root.clone(),
        map.clone(),
        image_init,
    ));
    let portee_provisioner = Arc::new(cosmon_rpp_adapter::portee::PorteeProvisioner::new(
        state_dir.to_path_buf(),
        provisioner.clone(),
    ));

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(state_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    let state = AppState {
        cs_path: state_dir.join("nonexistent-cs"),
        state_dir: state_dir.to_path_buf(),
        inbox_root: state_dir.join("whispers/inbox"),
        galaxies_root,
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: map.clone(),
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
        admin_seal: Arc::new(seal),
        provisioner,
        portee_provisioner,
    };
    (state, map)
}

fn provision_body() -> Value {
    json!({
        "noyau": "jordan-research",
        "oidc": {
            "issuer": "http://oidc-mock:8444",
            "sub": "jordan",
            "audience": "cosmon-rpp-jordan"
        },
        "scopes": ["cosmon:molecule:read", "cosmon:molecule:write"],
        "create_noyau": true
    })
}

async fn read_json(resp: axum::response::Response) -> Value {
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn fail_closed_returns_403_when_no_seal_configured() {
    let td = tempfile::tempdir().unwrap();
    let (state, _map) = make_state(td.path(), AdminSeal::disabled());
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/habilitations")
                .header("content-type", "application/json")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::from(provision_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(resp).await["error"], "admin_disabled");
}

#[tokio::test]
async fn missing_admin_token_is_401() {
    let td = tempfile::tempdir().unwrap();
    let (state, _map) = make_state(td.path(), AdminSeal::from_token(ADMIN_TOKEN));
    let app = router(state);

    // A tenant Bearer JWT must NOT open the door — only the seal header.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/habilitations")
                .header("content-type", "application/json")
                .header("authorization", "Bearer some.tenant.jwt")
                .body(Body::from(provision_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(resp).await["error"], "admin_token_missing");
}

#[tokio::test]
async fn wrong_admin_token_is_401() {
    let td = tempfile::tempdir().unwrap();
    let (state, _map) = make_state(td.path(), AdminSeal::from_token(ADMIN_TOKEN));
    let app = router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/habilitations")
                .header("content-type", "application/json")
                .header("x-cosmon-admin-token", "wrong-token")
                .body(Body::from(provision_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(resp).await["error"], "admin_token_invalid");
}

#[tokio::test]
async fn nominal_provision_resolves_without_restart() {
    let td = tempfile::tempdir().unwrap();
    let (state, map) = make_state(td.path(), AdminSeal::from_token(ADMIN_TOKEN));
    let app = router(state);

    // Before: the binding does not resolve.
    assert!(map
        .load()
        .resolve("http://oidc-mock:8444", "jordan")
        .is_none());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/habilitations")
                .header("content-type", "application/json")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::from(provision_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_json(resp).await;
    assert_eq!(body["reloaded"], true);
    assert_eq!(body["noyau"], "jordan-research");

    // After: the SAME live map the admission path reads now resolves the
    // new binding — no reboot, no SIGHUP. This is the B2 DoD.
    let live = map.load();
    let resolved = live
        .resolve("http://oidc-mock:8444", "jordan")
        .expect("binding resolves without restart");
    assert_eq!(resolved.noyau.as_str(), "jordan-research");

    // T6 idempotence: identical re-POST ⇒ 200 (not 201).
    let resp2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/habilitations")
                .header("content-type", "application/json")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::from(provision_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}

#[tokio::test]
async fn federation_one_gesture_materialises_n_habilitations_and_groups_them() {
    // ADR-0023 G5 end-to-end through the HTTP surface: one operator
    // gesture `POST /v1/admin/federations` materialises N per-galaxy
    // habilitations (capability = 1 galaxie, D4) and presents them as one
    // portée relation. Revocation works per galaxy and for the whole
    // relation.
    let td = tempfile::tempdir().unwrap();
    let (state, map) = make_state(td.path(), AdminSeal::from_token(ADMIN_TOKEN));
    let app = router(state);

    let body = json!({
        "portee_id": "casey",
        "partner": { "issuer": "https://casey.instance.peer", "sub": "casey" },
        "galaxies": ["speck", "qcd"],
        "scopes": ["cosmon:molecule:read"],
        "create_noyau": true
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/federations")
                .header("content-type", "application/json")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let out = read_json(resp).await;
    assert_eq!(out["portee_id"], "casey");
    assert_eq!(out["members"].as_array().unwrap().len(), 2);

    // Enforcement: each galaxy resolves on its own audience — one foreign
    // identity, two galaxies, no cross-tenant pivot. This is the SAME
    // live map the admission path reads.
    let live = map.load();
    assert_eq!(
        live.resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-speck")
            .unwrap()
            .noyau
            .as_str(),
        "speck"
    );
    assert_eq!(
        live.resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-qcd")
            .unwrap()
            .noyau
            .as_str(),
        "qcd"
    );

    // Presentation: GET groups the two grants as one relation.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/admin/federations")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let listing = read_json(resp).await;
    assert_eq!(listing["count"], 1);
    assert_eq!(listing["federations"][0]["portee_id"], "casey");

    // Revoke one galaxy: speck goes, qcd stays.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/v1/admin/federations/casey/galaxies/speck")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let live = map.load();
    assert!(live
        .resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-speck")
        .is_none());
    assert!(live
        .resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-qcd")
        .is_some());

    // Dissolve the whole relation: qcd goes, manifest removed.
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/v1/admin/federations/casey")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(map
        .load()
        .resolve_for_audience("https://casey.instance.peer", "casey", "cosmon-rpp-qcd")
        .is_none());
}

#[tokio::test]
async fn federation_requires_admin_seal() {
    // The federation surface is gated by the SAME host-side seal as the
    // habilitation routes — a tenant JWT never opens it.
    let td = tempfile::tempdir().unwrap();
    let (state, _map) = make_state(td.path(), AdminSeal::from_token(ADMIN_TOKEN));
    let app = router(state);
    let body = json!({
        "partner": { "issuer": "https://casey.instance.peer", "sub": "casey" },
        "galaxies": ["speck"]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/federations")
                .header("content-type", "application/json")
                .header("authorization", "Bearer some.tenant.jwt")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn reload_route_picks_up_host_staged_binding() {
    let td = tempfile::tempdir().unwrap();
    let (state, map) = make_state(td.path(), AdminSeal::from_token(ADMIN_TOKEN));
    let app = router(state);

    // Operator stages a binding host-side (no API write).
    let dir = td.path().join("nucleons").join("staged");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("oidc-identity.toml"),
        "nucleon_id = \"staged\"\nphase = \"Biological\"\nnoyau = \"staged\"\n\n\
         [oidc]\nissuer = \"https://idp\"\nsub = \"host-staged\"\naudience = \"aud\"\n",
    )
    .unwrap();
    assert!(map.load().resolve("https://idp", "host-staged").is_none());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/reload")
                .header("x-cosmon-admin-token", ADMIN_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(read_json(resp).await["reloaded"], true);
    // The host-staged binding is now live — reloaded without a reboot.
    assert!(map.load().resolve("https://idp", "host-staged").is_some());
}
