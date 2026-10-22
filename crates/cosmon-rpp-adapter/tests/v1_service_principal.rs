// SPDX-License-Identifier: AGPL-3.0-only

//! A read-only service principal for a non-interactive API consumer
//! (issue #189, ADR-080 §6.8).
//!
//! The consumer, a dashboard daemon for instance, gets its own subject at
//! the trusted issuer (a bot account: one `cosmon-remote login` on its own
//! profile, then `cosmon-remote token`). The host binds that subject with a
//! handoff binding that selects `preset = "observer"`, which renders a
//! `[scopes]` ceiling of `molecule:read`, `artifact:read`, `worker:read`
//! and `events:subscribe`.
//!
//! These tests provision from the handoff file exactly as boot does, then
//! drive the real router:
//!
//! - a token for the service principal that carries write or spawn scopes
//!   is refused on every write route class, because the binding is the
//!   ceiling;
//! - the four read routes answer even for an `openid`-only token;
//! - removing the binding, or a deny entry scoped to the service subject,
//!   revokes it on the host while the human operator's binding keeps
//!   working;
//! - the authz event names the service principal, not the human.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::auth::scopes::{
    ALL, ARTIFACT_READ, EVENTS_SUBSCRIBE, GRANT_SOURCE_BINDING, MOLECULE_READ, MOLECULE_WRITE,
    WORKER_READ, WORKER_SPAWN,
};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::rate_limit::{hash_principal, hash_sub, IngressRateLimiter};
use cosmon_rpp_adapter::trust_bootstrap::{
    converge_with, TrustBootstrapSection, OBSERVER_BINDING_SCOPES,
};
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::instrumentation::{
    read_authz_ndjson, AuthzDecision, AuthzDecisionEvaluated, AUTHZ_NDJSON_RELATIVE_PATH,
};
use tower::ServiceExt;

/// The dashboard daemon's own subject at the issuer.
const SERVICE: &str = "svc-dashboard";
/// The human operator, bound with the default (write-capable) scopes.
const HUMAN: &str = "operator-alice";
const AUD: &str = "cosmon-rpp-a";
const MOLECULE: &str = "task-20261014-0001";

/// One route the observer must not reach, with the request that exercises it.
struct WriteRoute {
    method: &'static str,
    uri: String,
    body: Option<&'static str>,
}

/// Every route class that mutates state, spawns, harvests, writes an
/// artifact, or reaches the world/avatar surface. `GET .../logs` is listed
/// too: the raw pane needs `logs:subscribe`, which the preset omits.
/// Admin routes are not here: they sit behind the admin seal, not scopes.
fn write_routes() -> Vec<WriteRoute> {
    let m = |method, path: &str, body| WriteRoute {
        method,
        uri: format!("/v1/molecules/{MOLECULE}{path}"),
        body,
    };
    vec![
        WriteRoute {
            method: "POST",
            uri: "/v1/molecules".to_owned(),
            body: Some(r#"{"formula":"task-work"}"#),
        },
        m("POST", "/tags", Some(r#"{"add":["seen"]}"#)),
        m("POST", "/collapse", Some(r#"{"reason":"stale"}"#)),
        m("POST", "/freeze", Some("{}")),
        m("POST", "/stuck", Some(r#"{"reason":"stuck"}"#)),
        m("POST", "/tackle", Some("{}")),
        m("POST", "/run", Some("{}")),
        m("POST", "/done", Some(r#"{"reason":"close"}"#)),
        m("PUT", "/artifacts/result.md", Some(r#""body""#)),
        m("GET", "/logs", None),
        WriteRoute {
            method: "POST",
            uri: "/v1/avatar/converse".to_owned(),
            body: Some(r#"{"message":"hi"}"#),
        },
        WriteRoute {
            method: "POST",
            uri: "/v1/avatar/perceive".to_owned(),
            body: Some("{}"),
        },
    ]
}

/// The four routes the observer preset exists for, with their authz verb.
const READ_ROUTES: &[(&str, &str)] = &[
    ("/v1/molecules", "ensemble"),
    (
        "/v1/molecules/task-20261014-0001/artifacts",
        "list_artifacts",
    ),
    ("/v1/workers", "list_workers"),
    ("/v1/events", "events_subscribe"),
];

struct Harness {
    oidc: OidcMock,
    security_dir: tempfile::TempDir,
    _tenants: TenantWorkspaces,
    _artifact_root: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    /// Provision the service principal and the human from one handoff file,
    /// through `trust_bootstrap::converge_with`, then load what it rendered.
    async fn provisioned() -> Self {
        let mut tenants = TenantWorkspaces::new();
        let tenant = tenants.add("a");
        tenant
            .install_task_work_formula()
            .expect("install task-work formula");
        // A harvest policy, so `done` reaches its scope check instead of
        // answering that remote harvest is not configured.
        let config_dir = tenant.root.join(".cosmon");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[harvest_authority]\nrequired = true\n",
        )
        .unwrap();
        let oidc = OidcMock::start_with(OidcMockConfig {
            audiences: vec![AUD.to_owned()],
            ..OidcMockConfig::default()
        })
        .await;
        let security_dir = tempfile::tempdir().unwrap();
        let artifact_root = tempfile::tempdir().unwrap();

        let handoff_dir = security_dir.path().join("handoff");
        std::fs::create_dir_all(&handoff_dir).unwrap();
        std::fs::write(
            handoff_dir.join("issuer.toml"),
            format!(
                r#"schema = "cosmon-issuer-handoff/v1"
[issuer]
iss = "{iss}"
audiences = ["{AUD}"]

# The dashboard daemon: its own subject, read-only by construction.
[[bindings]]
noyau = "a"
sub = "{SERVICE}"
nucleon_id = "svc-dashboard"
preset = "observer"

# The human operator keeps a separate, write-capable binding.
[[bindings]]
noyau = "a"
sub = "{HUMAN}"
nucleon_id = "operator-alice"
scopes = ["{MOLECULE_READ}", "{MOLECULE_WRITE}", "{WORKER_SPAWN}"]
"#,
                iss = oidc.issuer(),
            ),
        )
        .unwrap();
        let section = TrustBootstrapSection {
            handoff_dir: Some(handoff_dir),
            ..TrustBootstrapSection::default()
        };
        let report = converge_with(security_dir.path(), &section, None, false).unwrap();
        assert_eq!(report.bindings_written, ["svc-dashboard", "operator-alice"]);

        let state = make_state(&oidc, &tenants, security_dir.path(), artifact_root.path());
        Self {
            oidc,
            security_dir,
            _tenants: tenants,
            _artifact_root: artifact_root,
            state,
        }
    }

    fn reload_bindings(&self) {
        self.state
            .nucleon_map
            .store(HabilitationMap::load(self.security_dir.path()).unwrap());
    }

    fn token(&self, sub: &str, scopes: &[&str], jti: &str) -> String {
        self.oidc.issue(&IssueJwt {
            subject: sub,
            audience: Some(AUD),
            scopes,
            lifetime_secs: Some(60),
            jti: Some(jti),
        })
    }

    async fn send(&self, method: &str, uri: &str, jwt: &str, body: Option<&str>) -> StatusCode {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {jwt}"));
        let request = match body {
            Some(body) => builder
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }

    fn audit(&self) -> Vec<AuthzDecisionEvaluated> {
        read_authz_ndjson(&self.security_dir.path().join(AUTHZ_NDJSON_RELATIVE_PATH))
            .unwrap_or_default()
    }

    /// Write `security/oidc-policy.toml`; the harness's deny cache has a
    /// zero TTL, so the next request reads it.
    fn deny(&self, body: &str) {
        let dir = self.security_dir.path().join("security");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oidc-policy.toml"), body).unwrap();
    }
}

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &Path,
    artifact_root: &Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(
            HabilitationMap::load(security_dir).unwrap(),
        ),
        rate_limiter: Arc::new(IngressRateLimiter::new(
            security_dir.join("oidc-rate-limit"),
            64.0,
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

/// The binding ceiling wins over the token: a service token carrying
/// `molecule:write`, `worker:spawn`, or every scope in the catalog is
/// refused on every write route class, and no audit event allows a scope
/// outside the preset.
#[tokio::test]
async fn service_token_with_write_or_spawn_scopes_is_refused_on_every_write_route() {
    let h = Harness::provisioned().await;
    let tokens = [
        ("molecule:write", vec![MOLECULE_WRITE]),
        ("worker:spawn", vec![MOLECULE_WRITE, WORKER_SPAWN]),
        ("every catalog scope", ALL.to_vec()),
    ];
    for (label, scopes) in &tokens {
        for (i, route) in write_routes().iter().enumerate() {
            let jwt = h.token(SERVICE, scopes, &format!("jti-{label}-{i}"));
            assert_eq!(
                h.send(route.method, &route.uri, &jwt, route.body).await,
                StatusCode::FORBIDDEN,
                "{} {} with a service token carrying {label}",
                route.method,
                route.uri,
            );
        }
    }

    // Control: the same route answers for the human's write-capable
    // binding, so the refusals above come from the service binding.
    let human = h.token(HUMAN, &[MOLECULE_WRITE], "jti-human-write");
    assert_eq!(
        h.send(
            "POST",
            "/v1/molecules",
            &human,
            Some(r#"{"formula":"task-work"}"#)
        )
        .await,
        StatusCode::CREATED
    );

    let service = format!("jwt:{}", hash_principal(h.oidc.issuer(), SERVICE));
    let allowed_outside_preset: Vec<_> = h
        .audit()
        .into_iter()
        .filter(|e| e.subject_kind == service && e.decision == AuthzDecision::Allow)
        .filter(|e| {
            e.scope_required
                .as_deref()
                .is_some_and(|s| !OBSERVER_BINDING_SCOPES.contains(&s))
        })
        .collect();
    assert!(
        allowed_outside_preset.is_empty(),
        "{allowed_outside_preset:?}"
    );
}

/// The preset's purpose: the four read routes answer, granted by the
/// binding, even when the token carries no cosmon scope at all.
#[tokio::test]
async fn service_token_reads_the_four_preset_routes() {
    let h = Harness::provisioned().await;
    let jwt = h.token(SERVICE, &["openid"], "jti-read");
    for (uri, verb) in READ_ROUTES {
        // `/v1/events` answers with an open SSE stream; only the status
        // is read and dropping the response closes it.
        assert_eq!(
            h.send("GET", uri, &jwt, None).await,
            StatusCode::OK,
            "{uri}"
        );
        let event = h
            .audit()
            .into_iter()
            .rev()
            .find(|e| e.verb == *verb)
            .unwrap_or_else(|| panic!("{verb} audit event"));
        assert_eq!(event.decision, AuthzDecision::Allow, "{uri}");
        assert_eq!(event.grant_source.as_deref(), Some(GRANT_SOURCE_BINDING));
    }
    assert_eq!(
        OBSERVER_BINDING_SCOPES,
        [MOLECULE_READ, ARTIFACT_READ, WORKER_READ, EVENTS_SUBSCRIBE]
    );
}

/// The authz event records the service principal, so the host's audit
/// trail tells the daemon's reads apart from the human's.
#[tokio::test]
async fn authz_event_names_the_service_principal() {
    let h = Harness::provisioned().await;
    let jwt = h.token(SERVICE, &["openid"], "jti-audit");
    assert_eq!(
        h.send("GET", "/v1/events", &jwt, None).await,
        StatusCode::OK
    );

    let event = h
        .audit()
        .into_iter()
        .find(|e| e.verb == "events_subscribe")
        .expect("events_subscribe audit event");
    assert_eq!(
        event.subject_kind,
        format!("jwt:{}", hash_principal(h.oidc.issuer(), SERVICE))
    );
    assert_ne!(
        event.subject_kind,
        format!("jwt:{}", hash_principal(h.oidc.issuer(), HUMAN))
    );
}

/// Revocation is on the host: deleting the service binding refuses the
/// daemon at the next reload, and the human is unaffected.
#[tokio::test]
async fn removing_the_service_binding_revokes_it_on_the_host() {
    let h = Harness::provisioned().await;
    let service = h.token(SERVICE, &["openid"], "jti-svc");
    let human = h.token(HUMAN, &["openid"], "jti-human");
    assert_eq!(
        h.send("GET", "/v1/molecules", &service, None).await,
        StatusCode::OK
    );

    std::fs::remove_dir_all(h.security_dir.path().join("nucleons/svc-dashboard")).unwrap();
    h.reload_bindings();

    let refused = h.send("GET", "/v1/molecules", &service, None).await;
    assert!(
        refused.is_client_error() && refused != StatusCode::NOT_FOUND,
        "removed binding must refuse the service token, got {refused}"
    );
    assert_eq!(
        h.send("GET", "/v1/molecules", &human, None).await,
        StatusCode::OK
    );
}

/// A deny entry scoped to the issuer and the service subject revokes the
/// daemon without a reload or binding edit; the human keeps working.
#[tokio::test]
async fn scoped_deny_entry_revokes_the_service_principal_only() {
    let h = Harness::provisioned().await;
    let service = h.token(SERVICE, &["openid"], "jti-svc-deny");
    let human = h.token(HUMAN, &["openid"], "jti-human-deny");
    assert_eq!(
        h.send("GET", "/v1/molecules", &service, None).await,
        StatusCode::OK
    );

    h.deny(&format!(
        r#"[[deny.sub]]
issuer = "{}"
sub_hash = "{}"
reason = "dashboard host decommissioned"
"#,
        h.oidc.issuer(),
        hash_sub(SERVICE),
    ));

    let refused = h.send("GET", "/v1/molecules", &service, None).await;
    assert!(
        refused.is_client_error() && refused != StatusCode::NOT_FOUND,
        "deny entry must refuse the service token, got {refused}"
    );
    assert_eq!(
        h.send("GET", "/v1/molecules", &human, None).await,
        StatusCode::OK
    );
}
