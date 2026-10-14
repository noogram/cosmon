// SPDX-License-Identifier: AGPL-3.0-only

//! A binding that declares `[scopes]` is the scope ceiling.
//!
//! The binding's `[scopes].allowed` list is the complete grant for every
//! scope in the catalog: scopes the JWT carries are ignored, so a token can
//! never widen what the operator wrote. A binding with no `[scopes]`
//! section keeps legacy admission on the token's own scopes. Harvest stays
//! binding-only in both cases (ADR-176).
//!
//! The rule lives in one pure function,
//! [`cosmon_rpp_adapter::auth::scopes::scope_decision`]; these tests pin it
//! at the routes that reach it (nucleate, tackle, avatar, logs, done, the
//! observer read routes) and at the live stream re-check.
//!
//! "Observer binding" below is a binding that declares exactly
//! `molecule:read`, `artifact:read`, `worker:read` and `events:subscribe`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::auth::scopes::{
    scope_decision, ALL, ARTIFACT_READ, EVENTS_SUBSCRIBE, GRANT_SOURCE_BINDING, GRANT_SOURCE_JWT,
    LOGS_SUBSCRIBE, MOLECULE_HARVEST, MOLECULE_READ, MOLECULE_WRITE, WORKER_READ, WORKER_SPAWN,
    WORLD_OBSERVE,
};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::instrumentation::{
    read_authz_ndjson, AuthzDecision, AuthzDecisionEvaluated, AUTHZ_NDJSON_RELATIVE_PATH,
};
use tower::ServiceExt;

const SUB: &str = "service-a";
const AUD: &str = "cosmon-rpp-a";
const OBSERVER: &[&str] = &[MOLECULE_READ, ARTIFACT_READ, WORKER_READ, EVENTS_SUBSCRIBE];

/// Build the binding map: `Some(list)` declares `[scopes]`, `None` has no
/// such section.
fn binding_map(issuer: &str, scopes: Option<&[&str]>) -> HabilitationMap {
    let builder = HabilitationMap::builder();
    match scopes {
        Some(scopes) => builder.insert_with_scopes(
            issuer,
            SUB,
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            AUD,
            scopes.iter().map(|s| (*s).to_owned()).collect(),
        ),
        None => builder.insert(
            issuer,
            SUB,
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            AUD,
        ),
    }
    .build()
}

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    scopes: Option<&[&str]>,
    security_dir: &Path,
    artifact_root: &Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(binding_map(
            oidc.issuer(),
            scopes,
        )),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: artifact_root.to_path_buf(),
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

/// One tenant `a`, one binding for [`SUB`], and the directories the
/// adapter writes to. Holds every temp dir alive for the test's duration.
struct Harness {
    oidc: OidcMock,
    tenants: TenantWorkspaces,
    security_dir: tempfile::TempDir,
    _artifact_root: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    async fn new(scopes: Option<&[&str]>) -> Self {
        let mut tenants = TenantWorkspaces::new();
        let tenant = tenants.add("a");
        tenant
            .install_task_work_formula()
            .expect("install task-work formula");
        let oidc = OidcMock::start_with(OidcMockConfig {
            audiences: vec![AUD.to_owned()],
            ..OidcMockConfig::default()
        })
        .await;
        let security_dir = tempfile::tempdir().unwrap();
        let artifact_root = tempfile::tempdir().unwrap();
        let state = make_state(
            &oidc,
            &tenants,
            scopes,
            security_dir.path(),
            artifact_root.path(),
        );
        Self {
            oidc,
            tenants,
            security_dir,
            _artifact_root: artifact_root,
            state,
        }
    }

    fn app(&self) -> Router {
        router(self.state.clone())
    }

    fn token(&self, scopes: &[&str], jti: &str) -> String {
        self.oidc.issue(&IssueJwt {
            subject: SUB,
            audience: Some(AUD),
            scopes,
            lifetime_secs: Some(60),
            jti: Some(jti),
        })
    }

    /// Write the tenant's tracked config (harvest policy).
    fn tenant_config(&self, body: &str) {
        let dir = self.tenants.tenants()[0].root.join(".cosmon");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }

    fn audit(&self) -> Vec<AuthzDecisionEvaluated> {
        read_authz_ndjson(&self.security_dir.path().join(AUTHZ_NDJSON_RELATIVE_PATH))
            .expect("authz ndjson readable")
    }

    /// The single audit event for `verb`.
    fn audit_for(&self, verb: &str) -> AuthzDecisionEvaluated {
        let events: Vec<_> = self
            .audit()
            .into_iter()
            .filter(|e| e.verb == verb)
            .collect();
        assert_eq!(
            events.len(),
            1,
            "exactly one {verb} audit event: {events:?}"
        );
        events.into_iter().next().unwrap()
    }

    async fn send(&self, method: &str, uri: &str, jwt: &str, body: Option<&str>) -> StatusCode {
        self.app()
            .oneshot(request(method, uri, jwt, body))
            .await
            .unwrap()
            .status()
    }
}

fn request(method: &str, uri: &str, jwt: &str, body: Option<&str>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {jwt}"));
    match body {
        Some(body) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

fn assert_refused(audit: &AuthzDecisionEvaluated) {
    assert_eq!(audit.decision, AuthzDecision::Absent, "{audit:?}");
    assert_eq!(audit.grant_source, None, "{audit:?}");
}

#[tokio::test]
async fn observer_binding_refuses_token_carrying_molecule_write() {
    let h = Harness::new(Some(OBSERVER)).await;
    let jwt = h.token(&[MOLECULE_WRITE], "jti-ceiling-write");

    let status = h
        .send(
            "POST",
            "/v1/molecules",
            &jwt,
            Some(r#"{"formula":"task-work"}"#),
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_refused(&h.audit_for("nucleate"));
}

#[tokio::test]
async fn observer_binding_refuses_token_carrying_worker_spawn() {
    let h = Harness::new(Some(OBSERVER)).await;
    let jwt = h.token(&[MOLECULE_WRITE, WORKER_SPAWN], "jti-ceiling-spawn");

    let status = h
        .send(
            "POST",
            "/v1/molecules/task-20261014-0001/tackle",
            &jwt,
            Some("{}"),
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    let tackle: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|e| e.verb.starts_with("tackle"))
        .collect();
    assert!(!tackle.is_empty(), "tackle must record its scope decision");
    for event in &tackle {
        assert_refused(event);
    }
}

#[tokio::test]
async fn observer_binding_refuses_token_carrying_world_observe() {
    let h = Harness::new(Some(OBSERVER)).await;
    let jwt = h.token(&[WORLD_OBSERVE], "jti-ceiling-world");

    let perceive = h
        .send("POST", "/v1/avatar/perceive", &jwt, Some("{}"))
        .await;
    let status = h
        .send("GET", "/v1/avatar/avatar-1/status", &jwt, None)
        .await;

    assert_eq!(perceive, StatusCode::FORBIDDEN);
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_refused(&h.audit_for("perceive_avatar"));
    assert_refused(&h.audit_for("avatar_status"));
}

#[tokio::test]
async fn observer_binding_refuses_token_carrying_logs_subscribe() {
    let h = Harness::new(Some(OBSERVER)).await;
    let jwt = h.token(&[LOGS_SUBSCRIBE], "jti-ceiling-logs");

    let status = h
        .send("GET", "/v1/molecules/task-20261014-0001/logs", &jwt, None)
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_refused(&h.audit_for("logs_subscribe"));
}

/// Under a legacy harvest policy a token carrying `molecule:write` used to
/// admit `done` as `LegacyTokenWrite`. A declared `[scopes]` closes that at
/// the route and again at the effect port.
#[tokio::test]
async fn declared_scopes_binding_refuses_legacy_token_write_done() {
    use cosmon_core::config::HarvestAuthorityConfig;
    use cosmon_core::harvest_door::HarvestOptions;
    use cosmon_core::id::MoleculeId;
    use cosmon_core::remote_harvest::{
        resolve_remote_policy, RemoteAdmissionValidator, RemoteAuthoritySource,
        RemoteHarvestAdmission,
    };
    use cosmon_rpp_adapter::harvest_effect::AdapterRemoteValidator;

    let h = Harness::new(Some(OBSERVER)).await;
    h.tenant_config("[harvest_authority]\nrequired = true\n");
    let jwt = h.token(&[MOLECULE_WRITE], "jti-ceiling-done");

    let status = h
        .send(
            "POST",
            "/v1/molecules/task-20261014-0001/done",
            &jwt,
            Some(r#"{"reason":"close"}"#),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_refused(&h.audit_for("done"));

    // The effect port re-checks the authority source against the live
    // binding: `LegacyTokenWrite` is not valid for a declared `[scopes]`.
    let policy = resolve_remote_policy(&HarvestAuthorityConfig {
        required: true,
        ..HarvestAuthorityConfig::default()
    })
    .unwrap();
    let admission = |authority_source| RemoteHarvestAdmission {
        issuer: h.oidc.issuer().to_owned(),
        subject: SUB.to_owned(),
        audience: AUD.to_owned(),
        tenant: "a".to_owned(),
        molecule: MoleculeId::new("task-20261014-0001").unwrap(),
        options: HarvestOptions::new("close"),
        expires_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60,
        token_id: "jti-ceiling-done".to_owned(),
        authority_source,
        policy,
    };
    let declared = AdapterRemoteValidator {
        state: Arc::new(h.state.clone()),
    };
    assert_eq!(
        declared
            .validate(&admission(RemoteAuthoritySource::LegacyTokenWrite))
            .expect_err("declared [scopes] refuses token-write authority")
            .reason(),
        "harvest_binding_changed"
    );

    // Control: the same admission is valid for a binding with no section.
    let legacy = h.state.clone();
    legacy.nucleon_map.store(binding_map(h.oidc.issuer(), None));
    let legacy = AdapterRemoteValidator {
        state: Arc::new(legacy),
    };
    assert!(legacy
        .validate(&admission(RemoteAuthoritySource::LegacyTokenWrite))
        .is_ok());
}

#[tokio::test]
async fn observer_binding_admits_each_preset_read_route_with_openid_only_token() {
    let h = Harness::new(Some(OBSERVER)).await;
    let jwt = h.token(&["openid"], "jti-ceiling-read");

    for (uri, verb) in [
        ("/v1/molecules", "ensemble"),
        (
            "/v1/molecules/task-20261014-0001/artifacts",
            "list_artifacts",
        ),
        ("/v1/workers", "list_workers"),
        ("/v1/events", "events_subscribe"),
    ] {
        // The events route answers with an open SSE stream; only its
        // status is read, and dropping the response closes it.
        let status = h.send("GET", uri, &jwt, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        let audit = h.audit_for(verb);
        assert_eq!(audit.decision, AuthzDecision::Allow, "{uri}");
        assert_eq!(
            audit.grant_source.as_deref(),
            Some(GRANT_SOURCE_BINDING),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn declared_empty_scopes_grants_nothing() {
    let h = Harness::new(Some(&[])).await;
    let jwt = h.token(&[MOLECULE_READ], "jti-ceiling-empty");

    let status = h.send("GET", "/v1/molecules", &jwt, None).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_refused(&h.audit_for("ensemble"));
}

#[tokio::test]
async fn binding_without_scopes_section_keeps_token_scopes() {
    let h = Harness::new(None).await;
    let jwt = h.token(&[MOLECULE_WRITE], "jti-ceiling-legacy");

    let status = h
        .send(
            "POST",
            "/v1/molecules",
            &jwt,
            Some(r#"{"formula":"task-work"}"#),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED);
    let audit = h.audit_for("nucleate");
    assert_eq!(audit.decision, AuthzDecision::Allow);
    assert_eq!(audit.grant_source.as_deref(), Some(GRANT_SOURCE_JWT));
}

/// For every scope in the catalog: a declared binding decides alone, a
/// declared-empty binding grants nothing, and an `openid`-only token keeps
/// what the binding lists.
#[test]
fn scope_decision_with_declared_scopes_ignores_every_token_scope() {
    let owned =
        |scopes: &[&str]| -> Vec<String> { scopes.iter().map(|s| (*s).to_owned()).collect() };
    let every_token_scope = owned(ALL);
    let openid = owned(&["openid"]);
    let observer = owned(OBSERVER);
    for wanted in ALL {
        let listed = OBSERVER.contains(wanted).then_some(GRANT_SOURCE_BINDING);
        assert_eq!(
            scope_decision(&every_token_scope, Some(&observer), &[wanted]),
            listed,
            "{wanted}: a declared binding must decide alone",
        );
        assert_eq!(
            scope_decision(&every_token_scope, Some(&[]), &[wanted]),
            None,
            "{wanted}: a declared-empty binding grants nothing",
        );
        assert_eq!(
            scope_decision(&openid, Some(&observer), &[wanted]),
            listed,
            "{wanted}: an openid-only token keeps the binding grant",
        );
    }
}

/// Narrowing a declared binding closes an open stream whose token still
/// carries the dropped scope: the 1 s stream re-check uses the same rule.
#[tokio::test]
async fn open_events_stream_closes_when_binding_drops_events_subscribe() {
    let h = Harness::new(Some(OBSERVER)).await;
    let jwt = h.token(&[EVENTS_SUBSCRIBE], "jti-ceiling-stream");

    let response = h
        .app()
        .oneshot(request("GET", "/v1/events", &jwt, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(h.state.events.receiver_count(), 1);

    h.state
        .nucleon_map
        .store(binding_map(h.oidc.issuer(), Some(&[MOLECULE_READ])));

    tokio::time::timeout(Duration::from_secs(3), to_bytes(response.into_body(), 1024))
        .await
        .expect("stream stayed open after the binding dropped events:subscribe")
        .expect("SSE body failed");
}

/// ADR-176: a token carrying `molecule:harvest` never grants harvest, with
/// or without a `[scopes]` section.
#[tokio::test]
async fn harvest_remains_binding_only() {
    let token_harvest = vec![MOLECULE_HARVEST.to_owned()];
    assert_eq!(
        scope_decision(&token_harvest, None, &[MOLECULE_HARVEST]),
        None
    );
    assert_eq!(
        scope_decision(&token_harvest, Some(&[]), &[MOLECULE_HARVEST]),
        None
    );
    assert_eq!(
        scope_decision(&[], Some(&token_harvest), &[MOLECULE_HARVEST]),
        Some(GRANT_SOURCE_BINDING)
    );

    let h = Harness::new(None).await;
    h.tenant_config("[harvest_authority]\nremote = \"scoped\"\n");
    let jwt = h.token(&[MOLECULE_HARVEST], "jti-ceiling-harvest");

    let status = h
        .send(
            "POST",
            "/v1/molecules/task-20261014-0001/done",
            &jwt,
            Some(r#"{"reason":"close"}"#),
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_refused(&h.audit_for("done"));
}
