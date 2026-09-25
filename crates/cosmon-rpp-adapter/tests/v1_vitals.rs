// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end route contract for `GET /v1/vitals`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_core::agent::AgentRole;
use cosmon_core::id::AgentId;
use cosmon_core::transport::{AgentDefinition, RuntimeConfig, TransportBackend};
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
    backend: cosmon_transport::MockBackend,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    let bindings = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);

    AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(Arc::new(backend)),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(bindings),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(
            DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0)),
        ),
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

fn process(worker: &str) -> Value {
    json!({
        "status": "running",
        "process": {
            "worker_id": worker,
            "tmux_session": worker,
            "started_at": "2026-09-25T08:00:00Z",
            "status": "active",
            "adapter_name": "claude"
        }
    })
}

#[tokio::test]
async fn route_distinguishes_live_worker_orphan_and_unassigned_molecule() {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    tenant
        .insert_molecule("task-20260925-aaaa", &process("live-worker"))
        .unwrap();
    tenant
        .insert_molecule("task-20260925-bbbb", &process("dead-worker"))
        .unwrap();
    tenant
        .insert_molecule(
            "task-20260925-cccc",
            &json!({"status": "pending", "tags": ["temp:awaiting-op"]}),
        )
        .unwrap();
    tenant
        .insert_molecule("task-20260925-dddd", &json!({"status": "completed"}))
        .unwrap();

    let backend = cosmon_transport::MockBackend::new();
    backend
        .spawn(
            &AgentDefinition {
                id: AgentId::new("live-worker").unwrap(),
                role: AgentRole::Implementation,
                command: "worker".to_owned(),
                args: vec![],
                cwd: None,
            },
            &RuntimeConfig::default(),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path(), backend));
    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-vitals"),
    });

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/vitals")
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 32_768).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let vitals = &body["vitals"];
    assert_eq!(vitals["counts"]["molecules"], 3);
    assert_eq!(vitals["counts"]["live"], 1);
    assert_eq!(vitals["counts"]["orphaned"], 1);
    assert_eq!(vitals["counts"]["unassigned"], 1);
    assert_eq!(vitals["counts"]["awaiting_operator"], 1);
    let rows = vitals["molecules"].as_array().unwrap();
    let health = |id: &str| {
        rows.iter()
            .find(|row| row["id"] == id)
            .and_then(|row| row["health"].as_str())
    };
    assert_eq!(health("task-20260925-aaaa"), Some("live"));
    assert_eq!(health("task-20260925-bbbb"), Some("orphaned"));
    assert_eq!(health("task-20260925-cccc"), Some("unassigned"));
    assert!(rows.iter().all(|row| row["id"] != "task-20260925-dddd"));
}
