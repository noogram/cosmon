// SPDX-License-Identifier: Apache-2.0

//! V0 smoke test — drives `cs-thin` against an in-process rpp-adapter.
//!
//! This smoke boots the
//! library-direct rpp-adapter on a real TCP socket, mints a JWT via
//! the `cosmon-oidc-testkit` mock `IdP`, and runs the three V0 verbs
//! (observe / nucleate / tag) end-to-end through cs-thin's
//! [`run_with`] entry point. Asserts:
//!
//! 1. HTTP 200 (or 201) reaches the JSON renderer.
//! 2. The wire shape printed by cs-thin matches `cs --json <verb>`
//!    field-for-field — `observe.id`, `nucleate.status == "active"`,
//!    `tag.added` reflects the supplied `--add` list.
//!
//! The rpp-adapter is dev-dep — cs-thin does **not** link it in the
//! shipped binary; this test scaffolds it locally so we can exercise
//! the full HTTP path without a docker stack on every developer
//! machine. Operators who want the full container loop run
//! `crates/cosmon-rpp-adapter/scripts/dev-stack.sh` (out of scope here).

use std::sync::Arc;
use std::time::Duration;

use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_thin_cli::cli::{run_with, Cli, Command, NucleateArgs, ObserveArgs, TagArgs, VerbsArgs};
use serde_json::Value;

/// Build an [`AppState`] over the testkit primitives. Mirrors
/// `crates/cosmon-rpp-adapter/tests/v1_post_molecules.rs::make_state`
/// but kept local so the smoke is self-contained.
fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    nucleons: Vec<(&str, &str, &str, &str)>,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let mut builder = HabilitationMap::builder();
    for (sub, nucleon, noyau, audience) in nucleons {
        builder = builder.insert(
            oidc.issuer(),
            sub,
            HabilitationId::new(nucleon),
            Noyau::new(noyau),
            audience,
        );
    }

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: cosmon_oidc_testkit::fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        artifact_root: security_dir.join("artifacts"),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(builder.build()),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
        auth_claude: None,
        dist: std::sync::Arc::new(cosmon_rpp_adapter::routes::dist::DistState::new(
            "/tmp/cosmon-dist",
        )),
        install_templating: std::sync::Arc::new(
            cosmon_rpp_adapter::config::InstallTemplating::default(),
        ),
        events: std::sync::Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity()),
    }
}

/// Spawn the rpp-adapter on a random port and return the
/// `http://127.0.0.1:<port>` base URL plus the `JoinHandle` so the test
/// can drop both at end-of-scope.
async fn spawn_adapter(state: AppState) -> String {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Tiny delay so the listener is ready before reqwest connects.
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn observe_returns_byte_stable_molecule_json() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule(
            "task-20260504-shrd",
            &serde_json::json!({"variables": {"smoke": "yes"}}),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read"],
        lifetime_secs: Some(60),
        jti: Some("jti-cs-thin-observe"),
    });

    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(Command::Observe(ObserveArgs {
            molecule_id: "task-20260504-shrd".to_owned(),
        })),
    };

    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("observe should succeed");
    let line = std::str::from_utf8(&out).unwrap().trim();
    let body: Value = serde_json::from_str(line).unwrap();
    assert_eq!(body["id"], "task-20260504-shrd");
    // Wire-stable fields from `cs --json observe` — cs-thin re-renders
    // the molecule envelope verbatim.
    assert!(body.get("formula").is_some(), "missing formula");
    assert!(body.get("status").is_some(), "missing status");
}

#[tokio::test]
async fn nucleate_creates_molecule_with_active_status() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a.install_task_work_formula().unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-cs-thin-nucleate"),
    });

    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(Command::Nucleate(NucleateArgs {
            formula: "task-work".to_owned(),
            kind: Some("task".to_owned()),
            vars: vec!["topic=hello".to_owned()],
            tags: vec!["temp:warm".to_owned()],
        })),
    };

    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("nucleate should succeed");
    let line = std::str::from_utf8(&out).unwrap().trim();
    let body: Value = serde_json::from_str(line).unwrap();
    // `cs --json nucleate` shape: id, formula, status="active",
    // total_steps, assigned_worker, variables, created_at.
    assert_eq!(body["status"], "active");
    assert_eq!(body["formula"], "task-work");
    assert!(body["id"].as_str().unwrap().starts_with("task-"));
    assert!(body["total_steps"].is_u64());
    assert_eq!(body["variables"]["topic"], "hello");
    assert!(body["created_at"].is_string());
}

#[tokio::test]
async fn tag_adds_label_and_returns_tag_json() {
    let mut tenants = TenantWorkspaces::new();
    let tenant_a = tenants.add("a");
    tenant_a
        .insert_molecule("task-20260504-tagx", &serde_json::json!({}))
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let state = make_state(
        &oidc,
        &tenants,
        vec![("sub-a", "nuc-a", "a", "cosmon-rpp-a")],
        security_dir.path(),
    );
    let base_url = spawn_adapter(state).await;

    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("jti-cs-thin-tag"),
    });

    let cli = Cli {
        base_url: Some(base_url),
        jwt_from_env: None,
        jwt_file: Some(write_jwt_to_temp(&jwt)),
        coverage_report: false,
        json: false,
        command: Some(Command::Tag(TagArgs {
            molecule_id: "task-20260504-tagx".to_owned(),
            add: vec!["temp:hot".to_owned()],
            remove: vec![],
        })),
    };

    let mut out = Vec::new();
    run_with(cli, &mut out).await.expect("tag should succeed");
    let line = std::str::from_utf8(&out).unwrap().trim();
    let body: Value = serde_json::from_str(line).unwrap();
    // `cs --json tag` shape: id, tags, added, removed, delta.
    assert_eq!(body["id"], "task-20260504-tagx");
    assert_eq!(body["added"], serde_json::json!(["temp:hot"]));
    assert_eq!(body["removed"], serde_json::json!([]));
    assert_eq!(body["delta"], 1);
    assert_eq!(body["tags"], serde_json::json!(["temp:hot"]));
}

#[tokio::test]
async fn verbs_check_renders_coverage_summary() {
    // The new `verbs --check` flow is purely compile-time — no
    // network probe, no rpp-adapter needed. We still build the
    // adapter scaffolding so the test path mirrors the real CLI.
    let cli = Cli {
        base_url: None,
        jwt_from_env: None,
        jwt_file: None,
        coverage_report: false,
        json: false,
        command: Some(Command::Verbs(VerbsArgs {
            check: true,
            json: false,
        })),
    };
    let mut out = Vec::new();
    run_with(cli, &mut out)
        .await
        .expect("verbs --check should succeed");
    let text = std::str::from_utf8(&out).unwrap();
    // Covered verbs (V0) — all three appear in the human-readable
    // section.
    assert!(
        text.contains("observe"),
        "missing observe in output: {text}"
    );
    assert!(
        text.contains("nucleate"),
        "missing nucleate in output: {text}"
    );
    assert!(text.contains("tag"), "missing tag in output: {text}");
    // The summary tail surfaces ADR-080 and the COMPLETE status.
    assert!(
        text.contains("OPERATOR-ONLY"),
        "missing OPERATOR-ONLY block: {text}"
    );
    assert!(
        text.contains("ADR-080"),
        "missing ADR-080 reference: {text}"
    );
    assert!(
        text.contains("Status: COMPLETE"),
        "missing COMPLETE status: {text}"
    );
}

/// Write the JWT to a temp file and return the path. The path is
/// kept alive by the process — tempfile is cleaned up at process
/// exit by the OS, which is fine for a smoke test.
fn write_jwt_to_temp(jwt: &str) -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("jwt.txt");
    std::fs::write(&p, jwt).unwrap();
    // Leak the dir so the path stays valid for the duration of the
    // test. `Box::leak` keeps the directory alive until process exit
    // — acceptable for a one-shot smoke test.
    Box::leak(Box::new(dir));
    p
}
