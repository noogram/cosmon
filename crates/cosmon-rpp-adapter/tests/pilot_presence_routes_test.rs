// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot presence routes (issue #212, W7a): S-ATTACH, S-BEAT, S-DETACH and
//! S-PEERS through the authenticated router against the presence store.
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::{Duration as ChronoDuration, Utc};
use cosmon_core::collaboration::{
    AdmittedIdentity, AttachmentProof, BindingSpec, CollaborationBinding, CollaborationCapability,
    CollaborationScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::remote_presence::RemotePresence;
use cosmon_filestore::PresenceStore;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::collaboration::CollaborationBindingStore;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const MISSION: &str = "task-20261120-b000";
const OTHER_MISSION: &str = "task-20261120-b999";
const SESSIONS_WRITE: &str = "cosmon:sessions:write";
const SESSIONS_READ: &str = "cosmon:sessions:read";
type Binding<'a> = (&'a str, &'a str, &'a str, &'a str, Option<&'a [&'a str]>);

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    nucleons: Vec<Binding<'_>>,
    security_dir: &std::path::Path,
    backend: cosmon_transport::MockBackend,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let mut builder = HabilitationMap::builder();
    for (sub, nucleon, noyau, audience, scopes) in nucleons {
        builder = match scopes {
            Some(scopes) => builder.insert_with_scopes(
                oidc.issuer(),
                sub,
                HabilitationId::new(nucleon),
                Noyau::new(noyau),
                audience,
                scopes.iter().map(|s| (*s).to_owned()).collect(),
            ),
            None => builder.insert(
                oidc.issuer(),
                sub,
                HabilitationId::new(nucleon),
                Noyau::new(noyau),
                audience,
            ),
        };
    }

    // Large enough for a test that crosses one 100-entry page per seat.
    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 512.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        harvest_effect: std::sync::Arc::new(
            cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect,
        ),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(std::sync::Arc::new(
            backend,
        )),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(builder.build()),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: std::path::PathBuf::from("/tmp/cosmon"),
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
    }
}

struct Client {
    token: String,
    binding: CollaborationBinding,
    proof: AttachmentProof,
}

struct Fixture {
    _oidc: OidcMock,
    _tenants: TenantWorkspaces,
    _security: tempfile::TempDir,
    app: axum::Router,
    tenant_state: std::path::PathBuf,
    clients: BTreeMap<String, Client>,
}

/// One pilot binding per name on [`MISSION`], each with its own attachment.
/// A name listed in `granted` holds exactly those token scopes instead of
/// `cosmon:sessions:write`; its binding keeps `SessionsWrite`, so the token
/// scope is the only thing a route can refuse on. A name listed in
/// `elsewhere` is bound to [`OTHER_MISSION`] instead.
async fn fixture(
    names: &[&str],
    granted: &[(&str, &'static [&'static str])],
    elsewhere: &[&str],
) -> Fixture {
    let scopes_of = |name: &str| -> &'static [&'static str] {
        granted
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(&[SESSIONS_WRITE][..], |(_, scopes)| *scopes)
    };
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    for mission in [MISSION, OTHER_MISSION] {
        tenant
            .insert_molecule(mission, &json!({"status":"running"}))
            .unwrap();
    }
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security = tempfile::tempdir().unwrap();
    let backend = cosmon_transport::MockBackend::new();
    let bindings = names
        .iter()
        .map(|name| (*name, *name, "a", "cosmon-rpp-a", Some(scopes_of(name))))
        .collect();
    let state = make_state(&oidc, &tenants, bindings, security.path(), backend);
    let store = CollaborationBindingStore::new(&tenant.state_dir);
    let clients = names
        .iter()
        .map(|name| {
            let mission = if elsewhere.contains(name) {
                OTHER_MISSION
            } else {
                MISSION
            };
            let (binding, proof) = store
                .provision(
                    BindingSpec {
                        identity: AdmittedIdentity::new(oidc.issuer(), *name, "cosmon-rpp-a", "a")
                            .unwrap(),
                        capability: CollaborationCapability::PilotMission {
                            mission: MoleculeId::new(mission).unwrap(),
                        },
                        scopes: [CollaborationScope::SessionsWrite].into_iter().collect(),
                        label: None,
                    },
                    Utc::now(),
                )
                .unwrap();
            let token = oidc.issue(&IssueJwt {
                subject: name,
                audience: Some("cosmon-rpp-a"),
                scopes: scopes_of(name),
                lifetime_secs: Some(3600),
                jti: Some(name),
            });
            (
                (*name).to_owned(),
                Client {
                    token,
                    binding,
                    proof,
                },
            )
        })
        .collect();
    Fixture {
        _oidc: oidc,
        _tenants: tenants,
        _security: security,
        app: router(state),
        tenant_state: tenant.state_dir,
        clients,
    }
}

impl Fixture {
    fn attachment(&self, name: &str) -> String {
        self.clients[name].binding.attachment.to_string()
    }

    fn store(&self) -> PresenceStore {
        PresenceStore::new(&self.tenant_state)
    }

    fn records(&self) -> Vec<RemotePresence> {
        self.store()
            .remote_presences(&MoleculeId::new(MISSION).unwrap())
            .unwrap()
    }

    /// Every file under the tenant state, with its bytes.
    fn snapshot(&self) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn walk(dir: &std::path::Path, out: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.tenant_state, &mut out);
        out
    }

    async fn request(
        &self,
        name: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let client = &self.clients[name];
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {}", client.token))
            .header("cosmon-attachment", client.binding.attachment.to_string())
            .header("cosmon-attachment-proof", client.proof.expose_secret())
            .header("Content-Type", "application/json");
        let data = body.map_or(Body::empty(), |body| {
            Body::from(serde_json::to_vec(&body).unwrap())
        });
        let response = self
            .app
            .clone()
            .oneshot(req.body(data).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)}));
        (status, value)
    }

    async fn attach(&self, name: &str, body: Value) -> (StatusCode, Value) {
        self.request(
            name,
            "POST",
            &format!("/v1/pilot/{MISSION}/attachments"),
            Some(body),
        )
        .await
    }

    async fn beat(&self, name: &str, attachment: &str) -> (StatusCode, Value) {
        self.request(
            name,
            "PUT",
            &format!("/v1/pilot/{MISSION}/attachments/{attachment}/heartbeat"),
            None,
        )
        .await
    }

    async fn detach(&self, name: &str, attachment: &str) -> (StatusCode, Value) {
        self.request(
            name,
            "DELETE",
            &format!("/v1/pilot/{MISSION}/attachments/{attachment}"),
            None,
        )
        .await
    }

    async fn peers(&self, name: &str, query: &str) -> (StatusCode, Value) {
        self.request(
            name,
            "GET",
            &format!("/v1/pilot/{MISSION}/attachments{query}"),
            None,
        )
        .await
    }

    /// Move an attachment's last heartbeat back by `secs`, as if that much
    /// server time had passed.
    fn age(&self, name: &str, secs: i64) {
        let mission = MoleculeId::new(MISSION).unwrap();
        let attachment = &self.clients[name].binding.attachment;
        let path = self.store().remote_presence_path(&mission, attachment);
        let mut record: RemotePresence =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record.heartbeat_at -= ChronoDuration::seconds(secs);
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    }
}

#[tokio::test]
async fn attach_takes_mission_and_attachment_from_the_binding_and_retries_by_key() {
    let fx = fixture(&["laptop"], &[], &[]).await;
    let (status, body) = fx
        .attach("laptop", json!({"idempotency_key":"a1","label":"laptop"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["attachment"]["attachment"], fx.attachment("laptop"));
    assert_eq!(body["attachment"]["mission"], MISSION);
    assert_eq!(body["duplicate"], false);
    let stored = fx.records();
    assert_eq!(stored.len(), 1);
    let before = fx.snapshot();

    // Same key, same body: the same attachment, nothing written.
    let (status, again) = fx
        .attach("laptop", json!({"idempotency_key":"a1","label":"laptop"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["attachment"], body["attachment"]);
    assert_eq!(fx.snapshot(), before);

    // Same key, another body: refused, first record kept.
    let (status, collision) = fx
        .attach("laptop", json!({"idempotency_key":"a1","label":"desk"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{collision}");
    assert_eq!(collision["error"], "key_collision");
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn attach_refuses_authority_fields_and_unknown_fields() {
    let fx = fixture(&["laptop"], &[], &[]).await;
    for forged in [
        json!({"idempotency_key":"a1","role":"primary"}),
        json!({"idempotency_key":"a1","lease_epoch":3}),
        json!({"idempotency_key":"a1","mission":OTHER_MISSION}),
        json!({"idempotency_key":"a1","attachment":"att-ffffffffffffffff"}),
    ] {
        let (status, body) = fx.attach("laptop", forged.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{forged} -> {body}");
    }
    assert!(fx.records().is_empty());
}

#[tokio::test]
async fn a_third_attachment_of_one_mission_is_refused() {
    let fx = fixture(&["one", "two", "three"], &[], &[]).await;
    for name in ["one", "two"] {
        let (status, body) = fx.attach(name, json!({"idempotency_key":"k"})).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
    }
    let before = fx.snapshot();
    let (status, body) = fx.attach("three", json!({"idempotency_key":"k"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "attachment_limit");
    assert_eq!(fx.snapshot(), before);

    // Detaching one frees its place.
    let (status, _) = fx.detach("one", &fx.attachment("one")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = fx.attach("three", json!({"idempotency_key":"k"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn heartbeats_are_server_timed_and_spaced_ten_seconds_apart() {
    let fx = fixture(&["laptop"], &[], &[]).await;
    let me = fx.attachment("laptop");
    let (status, body) = fx.beat("laptop", &me).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "not_attached");

    fx.attach("laptop", json!({"idempotency_key":"k"})).await;
    let before = fx.snapshot();
    let (status, body) = fx.beat("laptop", &me).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"], "heartbeat_too_soon");
    assert!(body["retry_after_seconds"].as_u64().unwrap() <= 10);
    assert_eq!(fx.snapshot(), before);

    fx.age("laptop", 11);
    let earlier = fx.records()[0].heartbeat_at;
    let start = Utc::now();
    let (status, body) = fx.beat("laptop", &me).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let beat = fx.records()[0].heartbeat_at;
    assert!(beat > earlier && beat >= start, "{earlier} -> {beat}");
    assert_eq!(body["attachment"]["heartbeat_at"], json!(beat));

    // A second beat inside the window is refused again.
    let (status, _) = fx.beat("laptop", &me).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn detach_is_safe_to_repeat_and_touches_nothing_else() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    fx.attach("laptop", json!({"idempotency_key":"k"})).await;
    fx.attach("desk", json!({"idempotency_key":"k"})).await;
    let me = fx.attachment("laptop");
    let (status, body) = fx.detach("laptop", &me).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["detached"], true);
    let after = fx.snapshot();
    let (status, body) = fx.detach("laptop", &me).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["detached"], false);
    assert_eq!(fx.snapshot(), after);
    let left = fx.records();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].attachment.to_string(), fx.attachment("desk"));
    let (status, body) = fx.beat("laptop", &me).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_path_naming_another_attachment_or_mission_is_refused_not_rewritten() {
    let fx = fixture(&["laptop", "desk", "stranger"], &[], &["stranger"]).await;
    fx.attach("laptop", json!({"idempotency_key":"k"})).await;
    fx.attach("desk", json!({"idempotency_key":"k"})).await;
    fx.age("desk", 30);
    let before = fx.snapshot();
    let desk = fx.attachment("desk");

    // Laptop's own credentials, desk's attachment in the path.
    let (status, body) = fx.beat("laptop", &desk).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "binding_missing");
    let (status, body) = fx.detach("laptop", &desk).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "binding_missing");
    // A malformed attachment in the path is refused the same way.
    let (status, _) = fx.detach("laptop", "not-an-attachment").await;
    assert!(status.is_client_error());
    assert_eq!(fx.snapshot(), before);

    // A binding for another mission cannot reach this one, for any route.
    let (status, body) = fx.attach("stranger", json!({"idempotency_key":"k"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "binding_missing");
    let (status, body) = fx.peers("stranger", "").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stranger = fx.attachment("stranger");
    let (status, _) = fx.beat("stranger", &stranger).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = fx.detach("stranger", &stranger).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(fx.snapshot(), before);
    assert!(fx
        .store()
        .remote_presences(&MoleculeId::new(OTHER_MISSION).unwrap())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn write_routes_need_sessions_write_and_peers_need_sessions_read() {
    let fx = fixture(
        &["writer", "reader", "outsider"],
        &[
            ("reader", &[SESSIONS_READ]),
            ("outsider", &["cosmon:work:write"]),
        ],
        &[],
    )
    .await;
    fx.attach("writer", json!({"idempotency_key":"k"})).await;
    let before = fx.snapshot();
    let reader = fx.attachment("reader");
    for (status, body) in [
        fx.attach("reader", json!({"idempotency_key":"k"})).await,
        fx.beat("reader", &reader).await,
        fx.detach("reader", &reader).await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "scope_missing");
    }
    let (status, body) = fx.peers("reader", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = fx.peers("outsider", "").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "scope_missing");
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn peers_list_a_bounded_page_without_writing_anything() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    fx.attach("laptop", json!({"idempotency_key":"k","label":"laptop"}))
        .await;
    fx.attach("desk", json!({"idempotency_key":"k"})).await;
    let before = fx.snapshot();

    let (status, body) = fx.peers("laptop", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["attachments"].as_array().unwrap();
    let mut expected = vec![fx.attachment("laptop"), fx.attachment("desk")];
    expected.sort();
    assert_eq!(
        rows.iter()
            .map(|r| r["attachment"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(body["next_cursor"], Value::Null);
    let me = rows
        .iter()
        .find(|r| r["attachment"] == fx.attachment("laptop"))
        .unwrap();
    assert_eq!(me["self"], true);
    assert_eq!(me["label"], "laptop");
    // The projection carries no retry key, host fact or lease field.
    for row in rows {
        let mut keys = row.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        keys.sort();
        let mut expected = vec!["attached_at", "attachment", "heartbeat_at", "self"];
        if row.get("label").is_some() {
            expected.push("label");
        }
        expected.sort_unstable();
        assert_eq!(keys, expected);
    }

    // Page of one, then the stable cursor.
    let (status, first) = fx.peers("laptop", "?limit=1").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["attachments"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    assert_eq!(cursor, expected[0]);
    let (status, second) = fx.peers("laptop", &format!("?limit=1&after={cursor}")).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["attachments"][0]["attachment"], expected[1]);
    assert_eq!(second["next_cursor"], Value::Null);

    for bad in ["?limit=0", "?limit=101", "?after=nope", "?unknown=1"] {
        let (status, body) = fx.peers("laptop", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, _) = fx.peers("laptop", "?limit=100").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fx.snapshot(), before, "S-PEERS must write nothing");
}
