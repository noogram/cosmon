// SPDX-License-Identifier: AGPL-3.0-only

//! Acceptance tests for the `/metrics` (Prometheus) and `/diagnostics`
//! (JSON) endpoints.
//!
//! Both endpoints are operational (outside `/v1/`, no JWT gate). The
//! tests verify:
//!
//! - the Prometheus text body parses well enough that a real scraper
//!   would ingest it (one `# TYPE` per family, declared families
//!   present);
//! - the JSON diagnostics body carries every named projection from the
//!   §3.7 mini-spec (`version`, `uptime_seconds`, `nucleon_map`,
//!   `jwks`, `backends`, `rate_limit`, `events`, `rejects`);
//! - `/healthz` stays minimal-plus-version — the four-key body is
//!   frozen (`ok`, `service`, `version`, `api_surface_version`); any
//!   further growth belongs on `/diagnostics`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{fake_cs_path, OidcMock, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    router, AppState, BackendHealthRegistry, BackendProbe, JwksStore, MetricsRegistry, Posture,
};
use serde_json::Value;
use tower::ServiceExt;

const HEALTHZ_PAYLOAD_KEYS: &[&str] = &["ok", "service", "version", "api_surface_version"];

async fn make_state_with_metrics(
    security_dir: &std::path::Path,
    metrics: Arc<MetricsRegistry>,
    backend_registry: Arc<BackendHealthRegistry>,
) -> AppState {
    let oidc = OidcMock::start().await;
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    let nucleon_map = HabilitationMap::builder().build();
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));
    let tenants = TenantWorkspaces::new();
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
        subprocess_timeout: Duration::from_secs(5),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: backend_registry,
        auth_claude: None,
        artifact_root: std::path::PathBuf::from("/tmp/cosmon"),
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: std::sync::Arc::new(
            cosmon_rpp_adapter::config::InstallTemplating::default(),
        ),
        events: std::sync::Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
        metrics,
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
    }
}

#[tokio::test]
async fn healthz_stays_minimal_after_d820() {
    // Acceptance contract from the gap report §3.7 / task scope,
    // amended by delib-20260610-9a0c (tolnay): `/healthz` carries the
    // minimal `{ok:true, service}` liveness body PLUS the two additive
    // version fields (`version`, `api_surface_version`) so the
    // deployed build no longer has to be deduced by behavioural
    // inference. The set is frozen at these four keys — dashboard
    // data lives in `/metrics` and `/diagnostics`, not here.
    let tmp = tempfile::tempdir().unwrap();
    let metrics = Arc::new(MetricsRegistry::new());
    let backend = Arc::new(BackendHealthRegistry::new());
    let app = router(make_state_with_metrics(tmp.path(), metrics, backend).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 1024).await.unwrap()).unwrap();
    let map = body.as_object().expect("healthz returns a JSON object");
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    for required in HEALTHZ_PAYLOAD_KEYS {
        assert!(
            keys.contains(required),
            "/healthz must carry the key `{required}`",
        );
    }
    // No keys beyond the minimal set — additive growth must land on
    // /metrics or /diagnostics, not here.
    for k in &keys {
        assert!(
            HEALTHZ_PAYLOAD_KEYS.contains(k),
            "/healthz grew an unexpected key `{k}` — extend /diagnostics instead",
        );
    }

    // Full-body snapshot. `version` is the crate version (bake tag
    // series); `api_surface_version` is the event-fold length —
    // derived, never a hand-edited literal (wheeler
    // I-ADDITIVE-COUNTERS), so appending a surface event keeps this
    // snapshot green without touching it.
    let expected = serde_json::json!({
        "ok": true,
        "service": "cosmon-rpp-adapter",
        "version": env!("CARGO_PKG_VERSION"),
        "api_surface_version": cosmon_rpp_adapter::surface_events::SURFACE_EVENTS.len(),
    });
    assert_eq!(
        body, expected,
        "/healthz body is a frozen four-key snapshot"
    );
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_text_body() {
    let tmp = tempfile::tempdir().unwrap();
    let metrics = Arc::new(MetricsRegistry::new());
    let backend = Arc::new(BackendHealthRegistry::new());
    backend.register_configured(["anthropic".to_owned()]);
    backend.record(
        "anthropic",
        BackendProbe {
            success: true,
            latency_ms: 200,
            at: chrono::Utc::now(),
        },
    );
    let app = router(make_state_with_metrics(tmp.path(), metrics, backend).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/plain"),
        "Prometheus content-type must start with text/plain, got {content_type:?}",
    );

    let body = String::from_utf8(
        to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();

    // The required families per the §3.7 gap-report scope: workers
    // (proxied via subscribers / backend probes), molecules-by-state
    // (deferred — surfaced via the per-tenant filesystem, not the
    // adapter; documented as such in OpenAPI), JWT rejects
    // (`admission_rejects_total`), rate-limit consumption (capacity
    // + leak), backend health probes (`backend_status`).
    for required in &[
        "# TYPE cosmon_adapter_uptime_seconds gauge",
        "# TYPE cosmon_adapter_build_info gauge",
        "# TYPE cosmon_adapter_http_responses_total counter",
        "# TYPE cosmon_adapter_admission_rejects_total counter",
        "# TYPE cosmon_adapter_rate_limit_capacity gauge",
        "# TYPE cosmon_adapter_rate_limit_leak_per_minute gauge",
        "# TYPE cosmon_adapter_backend_status gauge",
        "# TYPE cosmon_adapter_jwks_keys_loaded gauge",
        "# TYPE cosmon_adapter_nucleon_bindings gauge",
        "# TYPE cosmon_adapter_events_subscribers gauge",
    ] {
        assert!(
            body.contains(required),
            "/metrics body missing required family declaration `{required}`",
        );
    }

    // Build info must carry the crate version label.
    assert!(
        body.contains(&format!("version=\"{}\"", env!("CARGO_PKG_VERSION"))),
        "/metrics body must expose the crate version through build_info",
    );

    // Backend status sample for the recorded backend must be present.
    assert!(
        body.contains("cosmon_adapter_backend_status{backend=\"anthropic\"}"),
        "/metrics body must surface the registered backend",
    );
}

#[tokio::test]
async fn metrics_endpoint_records_request_status_class() {
    // Hitting /metrics itself produces a 200 — verify the response
    // counter bumps so the layer is wired end-to-end.
    let tmp = tempfile::tempdir().unwrap();
    let metrics = Arc::new(MetricsRegistry::new());
    let backend = Arc::new(BackendHealthRegistry::new());
    let app = router(make_state_with_metrics(tmp.path(), Arc::clone(&metrics), backend).await);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();

    let counts = metrics.status_class_counts();
    assert!(
        counts.responses_2xx >= 1,
        "metrics layer must record /metrics own 200",
    );
}

#[tokio::test]
async fn diagnostics_endpoint_returns_named_projections() {
    let tmp = tempfile::tempdir().unwrap();
    let metrics = Arc::new(MetricsRegistry::new());
    metrics.record_reject("expired");
    metrics.record_reject("unknown_sub");
    let backend = Arc::new(BackendHealthRegistry::new());
    backend.register_configured(["anthropic".to_owned(), "ollama".to_owned()]);
    let app = router(make_state_with_metrics(tmp.path(), metrics, backend).await);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/diagnostics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(resp.into_body(), 64 * 1024).await.unwrap()).unwrap();

    // §3.7 named projections — every key from the gap-report scope
    // MUST be present so a dashboard can render the page without
    // out-of-band knowledge of the adapter version.
    for required in &[
        "service",
        "version",
        "posture",
        "uptime_seconds",
        "nucleon_map",
        "jwks",
        "backends",
        "rate_limit",
        "events",
        "rejects",
        "http_responses",
    ] {
        assert!(
            body.get(*required).is_some(),
            "/diagnostics body must carry the key `{required}`, got {body}",
        );
    }

    assert_eq!(body["service"], "cosmon-rpp-adapter");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert!(body["uptime_seconds"].is_u64());

    let backends = &body["backends"];
    assert_eq!(backends["count"], 2);
    let snapshot = backends["snapshot"].as_array().unwrap();
    assert_eq!(snapshot.len(), 2);

    let rejects = &body["rejects"];
    assert_eq!(rejects["total"], 2);
    let by_reason = rejects["by_reason"].as_array().unwrap();
    assert_eq!(by_reason.len(), 2);
    let reasons: Vec<&str> = by_reason
        .iter()
        .filter_map(|v| v["reason"].as_str())
        .collect();
    assert!(reasons.contains(&"expired"));
    assert!(reasons.contains(&"unknown_sub"));
}

#[tokio::test]
async fn diagnostics_endpoint_is_unauthenticated() {
    // No `Authorization` header sent — the operator-facing endpoints
    // must respond 200 regardless. JWT gating would defeat the
    // purpose (an operator checking the adapter from a Tailscale
    // shell does not have a tenant JWT).
    let tmp = tempfile::tempdir().unwrap();
    let metrics = Arc::new(MetricsRegistry::new());
    let backend = Arc::new(BackendHealthRegistry::new());
    let app = router(make_state_with_metrics(tmp.path(), metrics, backend).await);

    for path in &["/metrics", "/diagnostics"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(*path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "operational endpoint {path} must be unauthenticated"
        );
    }
}
