// SPDX-License-Identifier: AGPL-3.0-only

//! Collection semantics for `GET /v1/molecules` (issue #185).

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
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
    let map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();

    AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(map),
        rate_limiter: Arc::new(IngressRateLimiter::new(
            security_dir.join("oidc-rate-limit"),
            128.0,
            0.0,
        )),
        deny_list: Arc::new(
            DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0)),
        ),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: security_dir.join("artifacts"),
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

fn token(oidc: &OidcMock, scopes: &[&str], jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes,
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

async fn get(app: &axum::Router, uri: &str, token: &str, etag: Option<&str>) -> Response {
    let mut request = Request::builder()
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"));
    if let Some(value) = etag {
        request = request.header(header::IF_NONE_MATCH, value);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn json_body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 256 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn list_is_ordered_paged_conditional_and_carries_a_ledger_watermark() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    tenant
        .insert_molecule(
            "task-20261006-cccc",
            &json!({
                "created_at": "2026-10-06T03:00:00Z",
                "updated_at": "2026-10-06T03:01:00Z",
            }),
        )
        .unwrap();
    tenant
        .insert_molecule(
            "task-20261006-aaaa",
            &json!({
                "created_at": "2026-10-06T01:00:00Z",
                "updated_at": "2026-10-06T01:01:00Z",
                "status": "running",
                "fleet_id": "default",
                "kind": "task",
                "typed_links": [{"rel": "blocks", "target": "task-20261006-cccc"}],
                "merged_at": "2026-10-06T01:02:00Z",
                "last_progress_at": "2026-10-06T01:03:00Z",
                "base_branch": "main",
            }),
        )
        .unwrap();
    tenant
        .insert_molecule(
            "task-20261006-bbbb",
            &json!({
                "created_at": "2026-10-06T02:00:00Z",
                "updated_at": "2026-10-06T02:01:00Z",
            }),
        )
        .unwrap();
    std::fs::write(
        tenant.state_dir.join("events.jsonl"),
        b"{\"type\":\"molecule_nucleated\",\"molecule_id\":\"task-20261006-aaaa\"}\n",
    )
    .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = token(&oidc, &["cosmon:molecule:read"], "list-contract");

    let first = get(&app, "/v1/molecules?limit=2", &jwt, None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first
        .headers()
        .get(header::ETAG)
        .expect("collection ETag")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(etag.starts_with("W/\""));
    let body = json_body(first).await;
    let rows = body["ensemble"]["molecules"].as_array().unwrap();
    assert_eq!(
        rows.iter().map(|v| &v["id"]).collect::<Vec<_>>(),
        [&json!("task-20261006-aaaa"), &json!("task-20261006-bbbb")]
    );
    assert_eq!(body["ensemble"]["total"], 3);
    assert_eq!(body["ensemble"]["next_cursor"], "task-20261006-bbbb");
    let ledger_len = std::fs::metadata(tenant.state_dir.join("events.jsonl"))
        .unwrap()
        .len();
    assert!(body["ensemble"]["ledger_cursor"]
        .as_str()
        .unwrap()
        .ends_with(&format!(".{ledger_len}")));
    assert_eq!(rows[0]["phase"], "live");
    assert_eq!(rows[0]["updated_at"], "2026-10-06T01:01:00+00:00");
    assert_eq!(rows[0]["kind"], "task");
    assert_eq!(rows[0]["fleet"], "default");
    assert_eq!(rows[0]["typed_links"][0]["rel"], "blocks");
    assert_eq!(rows[0]["merged_at"], "2026-10-06T01:02:00+00:00");
    assert_eq!(rows[0]["last_progress_at"], "2026-10-06T01:03:00+00:00");
    assert_eq!(rows[0]["base_branch"], "main");

    let second = get(
        &app,
        "/v1/molecules?limit=2&cursor=task-20261006-bbbb",
        &jwt,
        None,
    )
    .await;
    let second_body = json_body(second).await;
    assert_eq!(
        second_body["ensemble"]["molecules"][0]["id"],
        "task-20261006-cccc"
    );
    assert!(second_body["ensemble"].get("next_cursor").is_none());

    let unchanged = get(&app, "/v1/molecules?limit=2", &jwt, Some(&etag)).await;
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(to_bytes(unchanged.into_body(), 1).await.unwrap().len(), 0);

    let state_path = tenant
        .state_dir
        .join("fleets/default/molecules/task-20261006-aaaa/state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["tags"] = json!(["changed"]);
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let changed = get(&app, "/v1/molecules?limit=2", &jwt, Some(&etag)).await;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers().get(header::ETAG).unwrap(), etag.as_str());
}

#[tokio::test]
async fn five_hundred_molecules_need_only_three_pages() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    for index in 0..500 {
        tenant
            .insert_molecule(
                &format!("task-20261006-{index:04}"),
                &json!({"created_at": "2026-10-06T00:00:00Z"}),
            )
            .unwrap();
    }
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = token(&oidc, &["cosmon:molecule:read"], "five-hundred");

    let mut cursor: Option<String> = None;
    let mut calls = 0;
    let mut ids = Vec::new();
    loop {
        calls += 1;
        let uri = cursor.as_ref().map_or_else(
            || "/v1/molecules?limit=200".to_owned(),
            |cursor| format!("/v1/molecules?limit=200&cursor={cursor}"),
        );
        let body = json_body(get(&app, &uri, &jwt, None).await).await;
        ids.extend(
            body["ensemble"]["molecules"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned()),
        );
        cursor = body["ensemble"]["next_cursor"]
            .as_str()
            .map(ToOwned::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(calls, 3);
    assert_eq!(ids.len(), 500);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
}

#[tokio::test]
async fn observer_preset_reaches_every_read_surface_without_world_observe() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    let id = "task-20261006-preset";
    tenant.insert_molecule(id, &json!({})).unwrap();
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let scopes = [
        "cosmon:molecule:read",
        "cosmon:artifact:read",
        "cosmon:worker:read",
        "cosmon:events:subscribe",
    ];
    assert!(!scopes.contains(&"cosmon:world:observe"));
    let jwt = token(&oidc, &scopes, "observer-preset");

    for uri in [
        "/v1/molecules".to_owned(),
        format!("/v1/molecules/{id}"),
        format!("/v1/molecules/{id}/status"),
        format!("/v1/molecules/{id}/artifacts"),
        "/v1/ledger".to_owned(),
    ] {
        let response = get(&app, &uri, &jwt, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
}
