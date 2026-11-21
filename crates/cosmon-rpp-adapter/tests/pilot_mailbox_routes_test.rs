// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot mailbox routes (issue #212, W7b): S-SEND, S-PULL and S-ACK through
//! the authenticated router against the existing pilot mailbox.
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use cosmon_core::collaboration::{
    AdmittedIdentity, AttachmentProof, BindingSpec, CollaborationBinding, CollaborationCapability,
    CollaborationScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::id::SessionId;
use cosmon_core::pilot_message::PilotMessage;
use cosmon_core::remote_mailbox::MAX_PENDING_PER_ATTACHMENT;
use cosmon_filestore::{PilotMailbox, RemoteSend};
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

    fn mailbox(&self) -> PilotMailbox {
        PilotMailbox::new(&self.tenant_state)
    }

    /// The mailbox of `name`'s attachment.
    fn sid(&self, name: &str) -> SessionId {
        SessionId::new(self.attachment(name)).unwrap()
    }

    fn inbox(&self, name: &str) -> Vec<PilotMessage> {
        self.mailbox().envelopes(&self.sid(name)).unwrap()
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

    async fn request_bytes(
        &self,
        name: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Vec<u8>) {
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
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (status, bytes.to_vec())
    }

    async fn request(
        &self,
        name: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, bytes) = self.request_bytes(name, method, path, body).await;
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)}));
        (status, value)
    }

    async fn send(&self, name: &str, body: Value) -> (StatusCode, Value) {
        self.request(
            name,
            "POST",
            &format!("/v1/pilot/{MISSION}/messages"),
            Some(body),
        )
        .await
    }

    async fn pull(&self, name: &str, query: &str) -> (StatusCode, Value) {
        self.request(
            name,
            "GET",
            &format!("/v1/pilot/{MISSION}/messages{query}"),
            None,
        )
        .await
    }

    async fn ack(&self, name: &str, key: &str) -> (StatusCode, Value) {
        self.request(
            name,
            "POST",
            &format!("/v1/pilot/{MISSION}/messages/{key}/ack"),
            None,
        )
        .await
    }

    /// Deliver `count` messages from `from` to `to` through the store, as
    /// earlier accepted sends would have.
    fn seed(&self, from: &str, to: &str, count: usize, body: &str) -> Vec<PilotMessage> {
        let recipients = [self.sid(from), self.sid(to)].into_iter().collect();
        (0..count)
            .map(|n| {
                self.mailbox()
                    .send_remote(
                        &MoleculeId::new(MISSION).unwrap(),
                        &self.sid(from),
                        &recipients,
                        &RemoteSend {
                            key: cosmon_core::work_message::MessageKey::new(format!("seed{n}"))
                                .unwrap(),
                            to: self.sid(to),
                            body: body.to_owned(),
                            ttl_secs: None,
                        },
                        Utc::now(),
                    )
                    .unwrap()
                    .0
            })
            .collect()
    }

    /// Write a local pilot's presence snapshot naming `mission`.
    fn local_pilot(&self, sid: &str, mission: &str) {
        let dir = self.tenant_state.join("presence");
        std::fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        std::fs::write(
            dir.join(format!("{sid}.json")),
            serde_json::to_vec(&json!({
                "session_id": sid, "galaxy": "a", "cwd": "/w", "pid": 1,
                "started_at": now, "heartbeat_at": now, "headline": "",
                "mission": mission,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test]
async fn send_takes_author_and_mission_from_the_binding_and_retries_by_key() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    let desk = fx.attachment("desk");
    let request = json!({"idempotency_key":"m1","to":desk,"message":"hello","expires_in_secs":60});
    let (status, body) = fx.send("laptop", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["mission"], MISSION);
    assert!(body["received_at"].is_string());
    assert_eq!(body["duplicate"], false);
    assert_eq!(body["message"]["from"], fx.attachment("laptop"));
    assert_eq!(body["message"]["to"], desk);
    assert_eq!(body["message"]["sequence"], 1);
    let stored = fx.inbox("desk");
    assert_eq!(stored.len(), 1);
    assert_eq!(body["message"]["id"], stored[0].id.as_str());
    assert_eq!(fx.mailbox().body(&stored[0]).unwrap(), b"hello");
    let before = fx.snapshot();

    // Same key, same request: the same message, nothing written.
    let (status, again) = fx.send("laptop", request).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["message"], body["message"]);
    assert_eq!(fx.snapshot(), before);

    // Same key, another body, lifetime or recipient: refused, first kept.
    let laptop = fx.attachment("laptop");
    for other in [
        json!({"idempotency_key":"m1","to":desk,"message":"bye","expires_in_secs":60}),
        json!({"idempotency_key":"m1","to":desk,"message":"hello"}),
        json!({"idempotency_key":"m1","to":laptop,"message":"hello","expires_in_secs":60}),
    ] {
        let (status, collision) = fx.send("laptop", other.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{other} -> {collision}");
        assert_eq!(collision["error"], "key_collision");
    }
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn send_refuses_forged_author_mission_and_unknown_fields() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    let desk = fx.attachment("desk");
    let before = fx.snapshot();
    for forged in [
        json!({"idempotency_key":"m","to":desk,"message":"x","from":desk}),
        json!({"idempotency_key":"m","to":desk,"message":"x","author":desk}),
        json!({"idempotency_key":"m","to":desk,"message":"x","mission":OTHER_MISSION}),
        json!({"idempotency_key":"m","to":desk,"message":"x","sequence":7}),
        json!({"to":desk,"message":"x"}),
        json!({"idempotency_key":"bad key","to":desk,"message":"x"}),
        json!({"idempotency_key":"m","to":desk,"message":"x","expires_in_secs":u64::MAX}),
    ] {
        let (status, body) = fx.send("laptop", forged.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{forged} -> {body}");
        assert_eq!(body["error"], "invalid_json_body", "{forged}");
    }
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn send_reaches_only_pilots_of_the_mission() {
    let fx = fixture(&["laptop", "desk", "stranger"], &[], &["stranger"]).await;
    fx.local_pilot("host-pilot", MISSION);
    fx.local_pilot("elsewhere-pilot", OTHER_MISSION);
    let before = fx.snapshot();
    for to in [
        fx.attachment("stranger"),
        "elsewhere-pilot".to_owned(),
        "nobody".to_owned(),
        "../../collaboration/x".to_owned(),
    ] {
        let (status, body) = fx
            .send(
                "laptop",
                json!({"idempotency_key":"m","to":to,"message":"x"}),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{to} -> {body}");
        assert_eq!(body["error"], "not_a_member", "{to}");
    }
    assert_eq!(fx.snapshot(), before);

    // A local pilot of the mission is a recipient like an attachment.
    let (status, body) = fx
        .send(
            "laptop",
            json!({"idempotency_key":"m","to":"host-pilot","message":"x"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let host = SessionId::new("host-pilot").unwrap();
    let inbox = fx.mailbox().envelopes(&host).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, fx.sid("laptop"));
}

#[tokio::test]
async fn a_message_is_at_most_65536_bytes() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    let desk = fx.attachment("desk");
    let (status, body) = fx
        .send(
            "laptop",
            json!({"idempotency_key":"big","to":desk,"message":"x".repeat(65_536)}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = fx.snapshot();
    let (status, body) = fx
        .send(
            "laptop",
            json!({"idempotency_key":"bigger","to":desk,"message":"x".repeat(65_537)}),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"], "payload_too_large");
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn an_attachment_holds_at_most_1000_pending_messages() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    let desk = fx.attachment("desk");
    let seeded = fx.seed("laptop", "desk", MAX_PENDING_PER_ATTACHMENT - 1, "x");
    let (status, body) = fx
        .send(
            "laptop",
            json!({"idempotency_key":"last","to":desk,"message":"x"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let before = fx.snapshot();
    let (status, body) = fx
        .send(
            "laptop",
            json!({"idempotency_key":"over","to":desk,"message":"x"}),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"], "pending_limit");
    assert_eq!(fx.snapshot(), before);
    // A retry of an accepted send still answers at the cap.
    let (status, body) = fx
        .send(
            "laptop",
            json!({"idempotency_key":"last","to":desk,"message":"x"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["duplicate"], true);
    // The recipient's acknowledgment frees one place.
    let (status, body) = fx.ack("desk", seeded[0].id.as_str()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = fx
        .send(
            "laptop",
            json!({"idempotency_key":"over","to":desk,"message":"x"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn pull_reads_the_own_mailbox_without_consuming_it() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    let sent = fx.seed("laptop", "desk", 3, "note");
    fx.seed("desk", "laptop", 1, "reply");
    let before = fx.snapshot();
    let (status, body) = fx.pull("desk", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["mission"], MISSION);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    for (got, want) in messages.iter().zip(&sent) {
        assert_eq!(got["id"], want.id.as_str());
        assert_eq!(got["from"], fx.attachment("laptop"));
        assert_eq!(got["body"], "note");
        assert_eq!(got["state"], "queued");
    }
    assert_eq!(body["undeliverable"], json!([]));
    assert_eq!(body["next_cursor"], Value::Null);
    // Read-only: the store is byte-identical, and a repeat sees the same.
    assert_eq!(fx.snapshot(), before);
    let (_, again) = fx.pull("desk", "").await;
    assert_eq!(again["messages"], body["messages"]);
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn pull_pages_by_sequence_at_most_100_messages() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    let sent = fx.seed("laptop", "desk", 101, "x");
    let (status, first) = fx.pull("desk", "").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["messages"].as_array().unwrap().len(), 100);
    assert_eq!(first["next_cursor"], 100);
    // The cursor is a sequence: acknowledging an earlier message does not
    // shift the next page.
    fx.ack("desk", sent[0].id.as_str()).await;
    let (_, second) = fx.pull("desk", "?after=100").await;
    let second = second["messages"].as_array().unwrap().clone();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["id"], sent[100].id.as_str());
    let (_, small) = fx.pull("desk", "?limit=2").await;
    assert_eq!(small["messages"].as_array().unwrap().len(), 2);
    assert_eq!(small["next_cursor"], 3);
    for query in ["?limit=0", "?limit=101", "?after=x", "?peek=true"] {
        let (status, body) = fx.pull("desk", query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query} -> {body}");
    }
}

#[tokio::test]
async fn pull_responses_stay_within_1048576_bytes() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    fx.seed("laptop", "desk", 20, &"y".repeat(65_536));
    let path = format!("/v1/pilot/{MISSION}/messages");
    let (status, bytes) = fx.request_bytes("desk", "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.len() <= 1_048_576, "{} bytes", bytes.len());
    let page: Value = serde_json::from_slice(&bytes).unwrap();
    let count = page["messages"].as_array().unwrap().len();
    assert!(count > 1 && count < 20, "{count}");
    assert_eq!(page["next_cursor"], count);
    let (_, bytes) = fx
        .request_bytes("desk", "GET", &format!("{path}?after={count}"), None)
        .await;
    assert!(bytes.len() <= 1_048_576);
    let rest: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(rest["messages"].as_array().unwrap().len(), 20 - count);
}

#[tokio::test]
async fn a_body_the_wire_cannot_carry_is_listed_as_undeliverable() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    // A local sender is not bounded: one body over the whole response, one
    // not UTF-8, one whose blob is gone.
    let cas = cosmon_filestore::cas::FileCas::new(fx.tenant_state.join("cas"));
    let desk = fx.sid("desk");
    let deliver = |seq: u64, bytes: &[u8], keep: bool| {
        let hash = cosmon_core::cas::CasStore::put(&cas, bytes).unwrap();
        let message = PilotMessage::new(
            SessionId::new("host-pilot").unwrap(),
            desk.clone(),
            seq,
            format!("cas/{}/{}", hash.prefix(), hash.as_str()),
            hash.as_str(),
            Utc::now(),
            None,
        );
        if !keep {
            std::fs::remove_file(fx.tenant_state.join(&message.payload_ref)).unwrap();
        }
        fx.mailbox().deliver(&message).unwrap();
        message
    };
    let big = deliver(1, &vec![b'z'; 1_048_576], true);
    let binary = deliver(2, &[0xff, 0xfe], true);
    let gone = deliver(3, b"vanished", false);
    let fine = deliver(4, b"fine", true);
    let before = fx.snapshot();
    let (status, body) = fx.pull("desk", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"], fine.id.as_str());
    let undeliverable = body["undeliverable"].as_array().unwrap();
    let reasons: Vec<_> = undeliverable
        .iter()
        .map(|u| (u["id"].as_str().unwrap().to_owned(), u["reason"].clone()))
        .collect();
    assert_eq!(
        reasons,
        vec![
            (big.id.to_string(), json!("payload_too_large")),
            (binary.id.to_string(), json!("payload_not_utf8")),
            (gone.id.to_string(), json!("payload_unreadable")),
        ]
    );
    assert!(undeliverable.iter().all(|u| u.get("body").is_none()));
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn ack_is_idempotent_and_bound_to_the_own_mailbox() {
    let fx = fixture(&["laptop", "desk"], &[], &[]).await;
    let sent = fx.seed("laptop", "desk", 2, "x");
    let (status, body) = fx.ack("desk", sent[0].id.as_str()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], sent[0].id.as_str());
    assert_eq!(body["duplicate"], false);
    let before = fx.snapshot();
    // Acknowledging twice succeeds without writing.
    let (status, body) = fx.ack("desk", sent[0].id.as_str()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["duplicate"], true);
    assert_eq!(fx.snapshot(), before);
    // An unknown key, or a key of another attachment's mailbox, is refused.
    for (name, key) in [
        ("desk", "rmsg-000000000000"),
        ("laptop", sent[1].id.as_str()),
    ] {
        let (status, body) = fx.ack(name, key).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name} {key} -> {body}");
        assert_eq!(body["error"], "unknown_envelope");
    }
    assert_eq!(fx.snapshot(), before);
    // The pull no longer lists the acknowledged message.
    let (_, page) = fx.pull("desk", "").await;
    let ids: Vec<_> = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    assert_eq!(ids, vec![json!(sent[1].id.as_str())]);
}

#[tokio::test]
async fn another_mission_in_the_path_is_refused() {
    let fx = fixture(&["laptop", "desk", "stranger"], &[], &["stranger"]).await;
    let sent = fx.seed("laptop", "desk", 1, "x");
    let before = fx.snapshot();
    let desk = fx.attachment("desk");
    let send = json!({"idempotency_key":"m","to":desk,"message":"x"});
    for (status, body) in [
        fx.send("stranger", send.clone()).await,
        fx.pull("stranger", "").await,
        fx.ack("stranger", sent[0].id.as_str()).await,
        fx.request(
            "desk",
            "GET",
            &format!("/v1/pilot/{OTHER_MISSION}/messages"),
            None,
        )
        .await,
        fx.request(
            "laptop",
            "POST",
            &format!("/v1/pilot/{OTHER_MISSION}/messages"),
            Some(send.clone()),
        )
        .await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "binding_missing");
    }
    assert_eq!(fx.snapshot(), before);
}

#[tokio::test]
async fn send_and_ack_need_sessions_write_and_pull_needs_sessions_read() {
    let fx = fixture(
        &["writer", "reader", "outsider"],
        &[
            ("reader", &[SESSIONS_READ]),
            ("outsider", &["cosmon:work:write"]),
        ],
        &[],
    )
    .await;
    let sent = fx.seed("writer", "reader", 1, "x");
    let before = fx.snapshot();
    let writer = fx.attachment("writer");
    for (status, body) in [
        fx.send(
            "reader",
            json!({"idempotency_key":"m","to":writer,"message":"x"}),
        )
        .await,
        fx.ack("reader", sent[0].id.as_str()).await,
        fx.pull("outsider", "").await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "scope_missing");
    }
    let (status, body) = fx.pull("reader", "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(fx.snapshot(), before);
}
