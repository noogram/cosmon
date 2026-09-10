// SPDX-License-Identifier: AGPL-3.0-only

//! Falsifier 5 of issue #51: **no rendered surface still names `land`.**
//!
//! Asserted over what the surface *renders*, not over one file. A verb comes
//! back the way it left — through a leftover: a route still mounted, a canon
//! row still folded, a client const still generated, a golden still blessed.
//! Each of those is a different file, and checking one of them is how the
//! next reader concludes the withdrawal was complete.
//!
//! What this deliberately does **not** assert is the absence of the *word*
//! from prose. The mounting line stays in `data/surface_events.txt` (the log
//! is append-only and its lines are history), ADR-176 keeps the reversed D4
//! in full, and CHANGELOG entries are not rewritten. A withdrawal that
//! erased its own record would teach nobody why the verb existed.

use cosmon_rpp_adapter::{frozen_api_surface, surface_events::SURFACE_ROUTES};

/// The folded §8p surface mounts no `land` route, and does mount `done`.
///
/// `frozen_api_surface()` is the fold the router is built from, so this is
/// the surface a tenant can actually reach — not the canon file's raw text.
#[test]
fn the_frozen_surface_mounts_done_and_not_land() {
    let surface = frozen_api_surface();
    assert!(
        !surface.iter().any(|route| route.contains("/land")),
        "a withdrawn route is still on the frozen surface: {surface:?}",
    );
    assert!(
        surface
            .iter()
            .any(|route| *route == "POST /v1/molecules/{id}/done"),
        "the harvest door must be mounted under its own name: {surface:?}",
    );
}

/// The same claim against the compile-time projection the help renderers and
/// the bijection test read. Two folds of one log; a withdrawal that reached
/// only one of them would be a divergence, which is what this issue reverses.
#[test]
fn the_generated_route_table_carries_no_withdrawn_route() {
    assert!(
        !SURFACE_ROUTES.iter().any(|route| route.contains("/land")),
        "the generated route table still carries a withdrawn route: {SURFACE_ROUTES:?}",
    );
}

/// A request to the withdrawn path is answered by the router's own 404 —
/// there is no handler behind it, authenticated or otherwise.
///
/// The end of the chain: a route can be absent from every table and still be
/// mounted by a stray `.route(...)` line. This is the only assertion that
/// reads the router rather than a projection of it.
#[tokio::test]
async fn the_withdrawn_path_is_not_routed() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    let security_dir = tempfile::tempdir().unwrap();
    let oidc = cosmon_oidc_testkit::OidcMock::start().await;
    let _ = oidc.write_jwks_file(security_dir.path()).unwrap();
    let tenants = cosmon_oidc_testkit::TenantWorkspaces::new();
    let app = cosmon_rpp_adapter::router(cosmon_rpp_adapter::AppState {
        harvest_effect: std::sync::Arc::new(
            cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect,
        ),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(std::sync::Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.path().to_path_buf(),
        inbox_root: security_dir.path().join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(
            cosmon_rpp_adapter::JwksStore::load(security_dir.path()).unwrap(),
        ),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(
            cosmon_rpp_adapter::nucleon_map::HabilitationMap::builder().build(),
        ),
        rate_limiter: std::sync::Arc::new(cosmon_rpp_adapter::rate_limit::IngressRateLimiter::new(
            security_dir.path().join("oidc-rate-limit"),
            64.0,
            0.0,
        )),
        deny_list: std::sync::Arc::new(cosmon_rpp_adapter::deny_list::DenyList::new(
            security_dir.path().to_path_buf(),
        )),
        posture: cosmon_rpp_adapter::Posture::Prepared,
        drain_timeout: std::time::Duration::from_secs(5),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: std::sync::Arc::new(cosmon_rpp_adapter::BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: security_dir.path().join("artifacts"),
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            security_dir.path().join("dist"),
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
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules/task-20260101-abcd/land")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "the withdrawn path is still mounted somewhere in the router",
    );
}
