// SPDX-License-Identifier: AGPL-3.0-only

//! Work routes exercise the authenticated router against canonical custody.
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use cosmon_core::advisory_attempt::AdvisorySeatId;
use cosmon_core::collaboration::{
    AdmittedIdentity, AttachmentProof, BindingSpec, CollaborationBinding, CollaborationCapability,
    CollaborationScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::work_message::{
    MessageBudget, SeatDecl, Stage, WorkMessageStore, WorkScope, WORK_MESSAGE_SCHEMA_VERSION,
};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use cosmon_state::collaboration::CollaborationBindingStore;
use cosmon_state::work_message::FileWorkMessageStore;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;
const OWNER: &str = "task-20261023-a000";
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
    owner_dir: std::path::PathBuf,
    tenant_state: std::path::PathBuf,
    scope: WorkScope,
    clients: BTreeMap<String, Client>,
    backend: cosmon_transport::MockBackend,
}

fn seat(name: &str) -> AdvisorySeatId {
    AdvisorySeatId::new(name).unwrap()
}

async fn fixture(names: &[&str], max_messages: u32) -> Fixture {
    fixture_scoped(names, max_messages, &[]).await
}

const WORK_WRITE: &str = "cosmon:work:write";

/// Like [`fixture`], but a seat listed in `granted` holds exactly those token
/// scopes instead of `cosmon:work:write`. Its binding keeps `WorkWrite`, so
/// the token scope is the only thing a route can refuse on.
async fn fixture_scoped(
    names: &[&str],
    max_messages: u32,
    granted: &[(&str, &'static [&'static str])],
) -> Fixture {
    let scopes_of = |name: &str| -> &'static [&'static str] {
        granted
            .iter()
            .find(|(seat, _)| *seat == name)
            .map_or(&[WORK_WRITE][..], |(_, scopes)| *scopes)
    };
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    tenant
        .insert_molecule(OWNER, &json!({"status":"running"}))
        .unwrap();
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
    let state = make_state(&oidc, &tenants, bindings, security.path(), backend.clone());
    let owner_dir = tenant
        .state_dir
        .join("fleets/default/molecules")
        .join(OWNER);
    let scope = WorkScope {
        schema_version: WORK_MESSAGE_SCHEMA_VERSION,
        owner: MoleculeId::new(OWNER).unwrap(),
        seats: names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (
                    seat(name),
                    SeatDecl {
                        molecule: MoleculeId::new(format!("task-20261023-a{:03}", i + 1)).unwrap(),
                        required: true,
                        provider_requirement: None,
                    },
                )
            })
            .collect(),
        budget: MessageBudget {
            max_payload_bytes: 256,
            max_messages_per_seat: max_messages,
            max_bytes_per_seat: 4096,
            default_ttl_secs: 3600,
            redeliver_after_secs: 1,
            max_delivery_attempts: 2,
        },
        declared_at: Utc::now(),
    };
    // Every seat has its own persistent molecule; a seat is not an
    // unattached label and no message creates its lifecycle owner.
    for declaration in scope.seats.values() {
        tenant
            .insert_molecule(declaration.molecule.as_str(), &json!({"status":"running"}))
            .unwrap();
    }
    FileWorkMessageStore::new(owner_dir.clone())
        .declare(&scope)
        .unwrap();
    let store = CollaborationBindingStore::new(&tenant.state_dir);
    let clients = names
        .iter()
        .map(|name| {
            let (binding, proof) = store
                .provision(
                    BindingSpec {
                        identity: AdmittedIdentity::new(oidc.issuer(), *name, "cosmon-rpp-a", "a")
                            .unwrap(),
                        capability: CollaborationCapability::WorkSeat {
                            owner: scope.owner.clone(),
                            seat: seat(name),
                        },
                        scopes: [CollaborationScope::WorkWrite].into_iter().collect(),
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
        owner_dir,
        tenant_state: tenant.state_dir,
        scope,
        clients,
        backend,
    }
}

impl Fixture {
    fn send(&self, key: &str, recipient: &str, payload: &str) -> Value {
        json!({"scope_revision":self.scope.revision().unwrap(),"recipient":recipient,
            "idempotency_key":key,"sender_time":"2026-10-23T10:00:00Z",
            "confidentiality":"internal","payload":payload})
    }
    fn pull(&self) -> Value {
        json!({"scope_revision":self.scope.revision().unwrap(),"adapter":"pull", "peek":false})
    }
    async fn request(
        &self,
        name: &str,
        method: &str,
        suffix: &str,
        body: Option<Value>,
        attachment: bool,
    ) -> (StatusCode, Value) {
        let client = &self.clients[name];
        let mut req = Request::builder()
            .method(method)
            .uri(format!("/v1/work/{OWNER}{suffix}"))
            .header("Authorization", format!("Bearer {}", client.token));
        if attachment {
            req = req
                .header("cosmon-attachment", client.binding.attachment.to_string())
                .header("cosmon-attachment-proof", client.proof.expose_secret());
        }
        let data = body.map_or(Body::empty(), |body| {
            Body::from(serde_json::to_vec(&body).unwrap())
        });
        let response = self
            .app
            .clone()
            .oneshot(
                req.header("Content-Type", "application/json")
                    .body(data)
                    .unwrap(),
            )
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
}

#[tokio::test]
async fn binding_is_required_and_sender_injection_is_refused() {
    let fx = fixture(&["cto", "mado"], 10).await;
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("finding", "cto", "finding")),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let mut forged = fx.send("forged", "cto", "finding");
    forged["sender"] = json!("cto");
    let (status, body) = fx
        .request("mado", "POST", "/messages", Some(forged), true)
        .await;
    assert!(
        status.is_client_error(),
        "sender field must be refused: {status} {body}"
    );
    assert!(FileWorkMessageStore::new(fx.owner_dir.clone())
        .load_all()
        .unwrap()
        .envelopes
        .is_empty());
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("valid", "cto", "finding")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(body["envelope"]["sender"], "mado");
    assert_eq!(body["envelope"]["sender_evidence"], "admitted_non_local");
}

#[tokio::test]
async fn replay_admits_once_collision_is_refused_and_revocation_keeps_history() {
    let fx = fixture(&["cto", "mado"], 10).await;
    let send = fx.send("finding", "cto", "finding");
    let (status, first) = fx
        .request("mado", "POST", "/messages", Some(send.clone()), true)
        .await;
    assert!(status.is_success(), "{status} {first}");
    let (status, duplicate) = fx
        .request("mado", "POST", "/messages", Some(send), true)
        .await;
    assert!(status.is_success(), "{status} {duplicate}");
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["envelope"], first["envelope"]);
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("finding", "cto", "changed")),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let store = FileWorkMessageStore::new(fx.owner_dir.clone());
    let before = store.load_all().unwrap();
    assert_eq!(before.envelopes.len(), 1);
    assert_eq!(
        before
            .receipts
            .iter()
            .filter(|r| matches!(r.stage, Stage::Admitted))
            .count(),
        1
    );
    CollaborationBindingStore::new(&fx.tenant_state)
        .revoke(&fx.clients["mado"].binding.id, Utc::now())
        .unwrap();
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("after", "cto", "after")),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let after = store.load_all().unwrap();
    assert_eq!(after.envelopes, before.envelopes);
    assert_eq!(after.receipts, before.receipts);
    let (status, body) = fx
        .request("cto", "POST", "/inbox", Some(fx.pull()), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["messages"][0]["payload"], "finding");
}

#[tokio::test]
async fn dispositions_are_receipts_and_reply_is_a_separate_message() {
    let fx = fixture(&["cto", "mado"], 10).await;
    for (index, disposition) in ["considered", "deferred", "rejected"].iter().enumerate() {
        let key = format!("finding-{index}");
        let (status, sent) = fx
            .request(
                "mado",
                "POST",
                "/messages",
                Some(fx.send(&key, "cto", "finding")),
                true,
            )
            .await;
        assert!(status.is_success(), "{status} {sent}");
        let ack = json!({"scope_revision":fx.scope.revision().unwrap(),"payload_digest":sent["envelope"]["payload_digest"],"disposition":disposition});
        let (status, body) = fx
            .request(
                "cto",
                "POST",
                &format!("/messages/{key}/ack"),
                Some(ack.clone()),
                true,
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
        let (status, body) = fx
            .request(
                "cto",
                "POST",
                &format!("/messages/{key}/ack"),
                Some(ack),
                true,
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
    }
    let before = FileWorkMessageStore::new(fx.owner_dir.clone())
        .load_all()
        .unwrap();
    assert_eq!(
        before.envelopes.len(),
        3,
        "acks do not create result messages"
    );
    assert_eq!(
        before
            .receipts
            .iter()
            .filter(|r| matches!(r.stage, Stage::Consumed { .. }))
            .count(),
        3,
        "duplicate ack appends nothing"
    );
    let mut reply = fx.send("result", "mado", "result bytes");
    reply["reply_to"] = json!("finding-0");
    let (status, body) = fx
        .request("cto", "POST", "/messages", Some(reply), true)
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = fx
        .request("mado", "POST", "/inbox", Some(fx.pull()), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["payload"], "result bytes");
    assert_eq!(body["messages"][0]["envelope"]["reply_to"], "finding-0");
    assert_eq!(
        body["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|receipt| receipt["stage"]["stage"] == "consumed")
            .count(),
        3
    );
}

fn lifecycle_snapshot(root: &std::path::Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(
        root: &std::path::Path,
        path: &std::path::Path,
        out: &mut BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == "work") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".lock"))
            {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[tokio::test]
async fn configured_extra_seat_and_work_operations_have_no_control_effects() {
    let fx = fixture(&["cto", "mado", "reviewer"], 10).await;
    let before = lifecycle_snapshot(&fx.tenant_state);
    let (status, list) = fx.request("reviewer", "GET", "", None, true).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["seats"].as_object().unwrap().len(), 3);
    assert!(list["received_at"].is_string());
    assert!(list["schema_version"].is_number());
    let (status, sent) = fx
        .request(
            "reviewer",
            "POST",
            "/messages",
            Some(fx.send("extra", "cto", "extra seat works")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {sent}");
    assert_eq!(sent["envelope"]["sender"], "reviewer");
    let suffix = format!("/inbox?scope_revision={}", fx.scope.revision().unwrap());
    let receipts_before = FileWorkMessageStore::new(fx.owner_dir.clone())
        .load_all()
        .unwrap()
        .receipts;
    let (status, body) = fx.request("cto", "GET", &suffix, None, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        FileWorkMessageStore::new(fx.owner_dir.clone())
            .load_all()
            .unwrap()
            .receipts,
        receipts_before,
        "peek is read-only"
    );
    let (status, body) = fx
        .request("cto", "POST", "/inbox", Some(fx.pull()), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ack = json!({"scope_revision":fx.scope.revision().unwrap(),"payload_digest":sent["envelope"]["payload_digest"],"disposition":"considered"});
    let (status, body) = fx
        .request("cto", "POST", "/messages/extra/ack", Some(ack), true)
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert!(
        fx.backend.calls().is_empty(),
        "no worker command, input or lifecycle call"
    );
    assert_eq!(
        lifecycle_snapshot(&fx.tenant_state),
        before,
        "fleet, molecules, lifecycle journals and leases unchanged"
    );
}

#[tokio::test]
async fn payload_message_and_delivery_budgets_bound_the_routes() {
    let fx = fixture(&["cto", "mado"], 1).await;
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("large", "cto", &"x".repeat(257))),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("one", "cto", "one")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("two", "cto", "two")),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    for _ in 0..2 {
        let (status, body) = fx
            .request("cto", "POST", "/inbox", Some(fx.pull()), true)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        tokio::time::sleep(Duration::from_millis(1050)).await;
    }
    let (status, body) = fx
        .request("cto", "POST", "/inbox", Some(fx.pull()), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["messages"].as_array().unwrap().is_empty(),
        "delivery attempts exhausted"
    );
    let records = FileWorkMessageStore::new(fx.owner_dir.clone())
        .load_all()
        .unwrap();
    assert_eq!(
        records
            .receipts
            .iter()
            .filter(|r| matches!(r.stage, Stage::DeliveryAttempted { .. }))
            .count(),
        2
    );
}

/// Opt-in real loopback server for the two-process witness. The shell script
/// keeps all profiles and credentials in its private temporary home.
#[tokio::test]
#[ignore = "loopback witness server; stopped by witness script"]
async fn loopback_witness_server() {
    use cosmon_remote::config::{Profile, ProfileStore};
    use cosmon_remote::credential::{
        AttachmentSlot, CredentialKey, CredentialStore, SecretToken, StoredCredential,
    };
    let directory = std::path::PathBuf::from(std::env::var("COSMON_WORK_WITNESS_DIR").unwrap());
    let config_root =
        std::path::PathBuf::from(std::env::var("COSMON_WORK_WITNESS_CONFIG_ROOT").unwrap());
    let fx = fixture(&["cto", "mado"], 10).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let profiles = ProfileStore::at(&config_root);
    let credentials = CredentialStore::file_at(&config_root);
    for (name, client) in &fx.clients {
        let mut profile = Profile::from_host(format!("http://{address}"));
        profile.sub = name.to_owned();
        profile.aud = "cosmon-rpp-a".to_owned();
        profile.oidc_url = fx._oidc.issuer().to_owned();
        profile.issuer = Some(fx._oidc.issuer().to_owned());
        profile.client_id = Some("cosmon-rpp-a".to_owned());
        profile.attachment = Some(client.binding.attachment.to_string());
        profile.scopes = vec!["cosmon:work:write".to_owned()];
        profile.phone_home = false;
        profiles.write_profile(name, &profile).unwrap();
        let key = CredentialKey::new(fx._oidc.issuer(), name, "cosmon-rpp-a");
        credentials
            .store(
                &key,
                &StoredCredential::new(
                    SecretToken::new(&client.token),
                    SecretToken::new(""),
                    Utc::now() + chrono::Duration::hours(1),
                ),
            )
            .unwrap();
        credentials
            .store_attachment_proof(
                &AttachmentSlot::new(key, client.binding.attachment.clone()),
                &client.proof,
            )
            .unwrap();
    }
    std::fs::write(directory.join("ready.json"),serde_json::to_vec(&json!({"host":format!("http://{address}"),"owner":OWNER,"scope_revision":fx.scope.revision().unwrap()})).unwrap()).unwrap();
    axum::serve(listener, fx.app.clone()).await.unwrap();
}

#[tokio::test]
async fn same_key_with_changed_request_tuple_is_not_a_replay() {
    let fx = fixture(&["cto", "mado", "reviewer"], 10).await;
    let original = fx.send("tuple-key", "cto", "same bytes");
    let (status, body) = fx
        .request("mado", "POST", "/messages", Some(original.clone()), true)
        .await;
    assert!(status.is_success(), "{status} {body}");
    for (field, value) in [
        ("recipient", json!("reviewer")),
        ("phase", json!("review")),
        ("ttl_secs", json!(12)),
        ("confidentiality", json!("confidential")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        let (status, body) = fx
            .request("mado", "POST", "/messages", Some(changed), true)
            .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "changed {field} must collide: {body}"
        );
    }
    let (status, body) = fx
        .request("reviewer", "POST", "/messages", Some(original), true)
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a different admitted sender is a collision: {body}"
    );
    assert_eq!(
        FileWorkMessageStore::new(fx.owner_dir)
            .load_all()
            .unwrap()
            .envelopes
            .len(),
        1
    );
}

#[tokio::test]
async fn inbox_caps_response_bytes_before_recording_delivery_attempts() {
    let mut fx = fixture(&["cto", "mado"], 10).await;
    fx.scope.budget.max_payload_bytes = 600_000;
    fx.scope.budget.max_bytes_per_seat = 2_000_000;
    FileWorkMessageStore::new(fx.owner_dir.clone())
        .declare(&fx.scope)
        .unwrap();
    for key in ["big-1", "big-2"] {
        let (status, body) = fx
            .request(
                "mado",
                "POST",
                "/messages",
                Some(fx.send(key, "cto", &"\"\n".repeat(140_000))),
                true,
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
    }
    for _ in 0..2 {
        let (status, body) = fx
            .request("cto", "POST", "/inbox", Some(fx.pull()), true)
            .await;
        assert_eq!(status, StatusCode::OK, "{status}");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert!(serde_json::to_vec(&body).unwrap().len() <= 1_048_576);
    }
    let records = FileWorkMessageStore::new(fx.owner_dir).load_all().unwrap();
    assert_eq!(
        records
            .receipts
            .iter()
            .filter(|r| matches!(r.stage, Stage::DeliveryAttempted { .. }))
            .count(),
        2,
        "only envelopes returned to the caller cost attempts"
    );
}

#[tokio::test]
async fn nested_context_fields_are_strict_and_never_write_observations_on_refusal() {
    let fx = fixture(&["cto", "mado"], 10).await;
    let mut pull = fx.pull();
    pull["context_observation"] =
        json!({"status":"unknown","reason":"cannot observe model input","sender":"mado"});
    let (status, body) = fx.request("cto", "POST", "/inbox", Some(pull), true).await;
    assert!(
        status.is_client_error(),
        "unknown nested field must be refused: {status} {body}"
    );
    assert!(FileWorkMessageStore::new(fx.owner_dir)
        .load_all()
        .unwrap()
        .receipts
        .is_empty());
}

/// A separate native client process using the same two explicitly addressed
/// profile and proof stores as the CLI, without changing system home/config.
#[tokio::test]
#[ignore = "invoked as a separate client process by the loopback witness"]
async fn loopback_witness_client() {
    use cosmon_core::collaboration::AttachmentId;
    use cosmon_core::work_message::{Confidentiality, DeliveryAdapter, Disposition};
    use cosmon_remote::config::ProfileStore;
    use cosmon_remote::credential::{AttachmentSlot, CredentialStore};
    use cosmon_remote::work::{AckRequest, PullRequest, SendRequest};
    let directory = std::path::PathBuf::from(std::env::var("COSMON_WORK_WITNESS_DIR").unwrap());
    let root = std::path::PathBuf::from(std::env::var("COSMON_WORK_WITNESS_CONFIG_ROOT").unwrap());
    let name = std::env::var("COSMON_WORK_WITNESS_PROFILE").unwrap();
    let action = std::env::var("COSMON_WORK_WITNESS_ACTION").unwrap();
    let profile = ProfileStore::at(&root).read_profile(&name).unwrap();
    let key = profile.credential_key().unwrap();
    let store = CredentialStore::file_at(root);
    let credential = store.load(&key).unwrap().unwrap();
    let attachment = AttachmentId::new(profile.attachment.as_deref().unwrap()).unwrap();
    let proof = store
        .load_attachment_proof(&AttachmentSlot::new(key, attachment.clone()))
        .unwrap()
        .unwrap();
    let client = cosmon_remote::client::Client::new(
        &profile,
        Some(credential.access_token().expose().to_owned()),
    )
    .unwrap();
    let ready: Value =
        serde_json::from_slice(&std::fs::read(directory.join("ready.json")).unwrap()).unwrap();
    let revision = ready["scope_revision"].as_str().unwrap().to_owned();
    let output = match action.as_str() {
        "send" | "reply" => client
            .work_send(
                OWNER,
                &attachment,
                &proof,
                &SendRequest {
                    scope_revision: revision,
                    recipient: if action == "send" { "cto" } else { "mado" }.to_owned(),
                    idempotency_key: if action == "send" {
                        "finding-1"
                    } else {
                        "result-1"
                    }
                    .to_owned(),
                    sender_time: Utc::now().to_rfc3339(),
                    reply_to: (action == "reply").then(|| "finding-1".to_owned()),
                    phase: None,
                    confidentiality: Confidentiality::Internal,
                    ttl_secs: None,
                    payload: if action == "send" {
                        "Please inspect the implementation."
                    } else {
                        "Inspection result: accepted."
                    }
                    .to_owned(),
                },
            )
            .await
            .unwrap(),
        "pull" => client
            .work_inbox(
                OWNER,
                &attachment,
                &proof,
                &PullRequest {
                    scope_revision: revision,
                    adapter: DeliveryAdapter::Pull,
                    context_observation: None,
                    peek: false,
                },
            )
            .await
            .unwrap(),
        "ack" => {
            let sent: Value =
                serde_json::from_slice(&std::fs::read(directory.join("send.json")).unwrap())
                    .unwrap();
            client
                .work_ack(
                    OWNER,
                    "finding-1",
                    &attachment,
                    &proof,
                    &AckRequest {
                        scope_revision: revision,
                        payload_digest: sent["envelope"]["payload_digest"]
                            .as_str()
                            .unwrap()
                            .to_owned(),
                        disposition: Disposition::Considered,
                        reply: None,
                        note: None,
                    },
                )
                .await
                .unwrap()
        }
        _ => panic!("unknown witness action"),
    };
    std::fs::write(
        std::env::var("COSMON_WORK_WITNESS_OUTPUT").unwrap(),
        serde_json::to_vec_pretty(&output).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn expired_remote_ack_is_refused_without_consuming_the_envelope() {
    let fx = fixture(&["cto", "mado"], 10).await;
    let mut body = fx.send("expired", "cto", "short lifetime");
    body["ttl_secs"] = json!(1);
    let (status, sent) = fx
        .request("mado", "POST", "/messages", Some(body), true)
        .await;
    assert!(status.is_success(), "{sent}");
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let ack = json!({"scope_revision":fx.scope.revision().unwrap(),"payload_digest":sent["envelope"]["payload_digest"],"disposition":"considered"});
    let (status, response) = fx
        .request("cto", "POST", "/messages/expired/ack", Some(ack), true)
        .await;
    assert_eq!(status, StatusCode::GONE, "{response}");
    assert_eq!(response["error"], "envelope_expired");
    assert!(!FileWorkMessageStore::new(fx.owner_dir)
        .load_all()
        .unwrap()
        .receipts
        .iter()
        .any(|r| matches!(r.stage, Stage::Consumed { .. })));
}

#[tokio::test]
async fn sender_sees_the_most_recent_acknowledgments_beyond_one_page() {
    let mut fx = fixture(&["cto", "mado"], 200).await;
    fx.scope.budget.max_delivery_attempts = 5;
    FileWorkMessageStore::new(fx.owner_dir.clone())
        .declare(&fx.scope)
        .unwrap();
    for i in 0..101 {
        let (status, body) = fx
            .request(
                "mado",
                "POST",
                "/messages",
                Some(fx.send(&format!("m-{i:03}"), "cto", "x")),
                true,
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
    }
    // Keyed by message: a later page may also carry redeliveries.
    let mut pulled = BTreeMap::new();
    for _ in 0..4 {
        if pulled.len() == 101 {
            break;
        }
        let (status, body) = fx
            .request("cto", "POST", "/inbox", Some(fx.pull()), true)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for message in body["messages"].as_array().unwrap() {
            let key = message["envelope"]["key"].as_str().unwrap().to_owned();
            pulled.insert(key, message.clone());
        }
    }
    assert_eq!(pulled.len(), 101, "every message is eventually delivered");
    for message in pulled.values() {
        let key = message["envelope"]["key"].as_str().unwrap();
        let ack = json!({"scope_revision": fx.scope.revision().unwrap(),
            "payload_digest": message["envelope"]["payload_digest"],
            "disposition": "considered"});
        let (status, body) = fx
            .request(
                "cto",
                "POST",
                &format!("/messages/{key}/ack"),
                Some(ack),
                true,
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
    }
    let (status, body) = fx
        .request("mado", "POST", "/inbox", Some(fx.pull()), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let keys: Vec<&str> = body["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 100, "receipts stay bounded to one page");
    assert!(
        keys.contains(&"m-100"),
        "the 101st acknowledgment must reach its sender: {keys:?}"
    );
}

#[tokio::test]
async fn an_unrepresentable_payload_does_not_block_the_remote_inbox() {
    use cosmon_core::work_message::{AdmittedWorkCaller, Confidentiality, SenderEvidence};
    use cosmon_state::work_ops::{SendRequest, WorkOperations};
    let fx = fixture(&["cto", "mado"], 10).await;
    // A local sender may admit arbitrary bytes; this wire carries UTF-8 text.
    let local = AdmittedWorkCaller::new(
        fx.scope.owner.clone(),
        seat("mado"),
        SenderEvidence::CallerEnvSameUid,
    );
    WorkOperations::new(fx.owner_dir.clone())
        .send(
            &local,
            SendRequest {
                scope_revision: fx.scope.revision().unwrap(),
                recipient: seat("cto"),
                key: Some(cosmon_core::work_message::MessageKey::new("binary").unwrap()),
                sender_time: Utc::now(),
                reply_to: None,
                phase: None,
                confidentiality: Confidentiality::Internal,
                ttl_secs: None,
            },
            &[0xff, 0xfe, 0x00],
            Utc::now(),
        )
        .unwrap();
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("text", "cto", "readable")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let peek = format!(
        "/inbox?scope_revision={}&adapter=pull",
        fx.scope.revision().unwrap()
    );
    for (method, path, body) in [
        ("GET", peek.as_str(), None),
        ("POST", "/inbox", Some(fx.pull())),
    ] {
        let (status, body) = fx.request("cto", method, path, body, true).await;
        assert_eq!(status, StatusCode::OK, "{method}: {body}");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "{method}: {body}");
        assert_eq!(messages[0]["envelope"]["key"], "text");
        assert_eq!(body["undeliverable"][0]["envelope"]["key"], "binary");
        assert_eq!(body["undeliverable"][0]["reason"], "payload_not_utf8");
    }
    let attempts: Vec<String> = FileWorkMessageStore::new(fx.owner_dir)
        .load_all()
        .unwrap()
        .receipts
        .iter()
        .filter(|r| matches!(r.stage, Stage::DeliveryAttempted { .. }))
        .map(|r| r.key.to_string())
        .collect();
    assert_eq!(attempts, ["text"], "the skipped envelope costs no attempt");
}

#[tokio::test]
async fn a_payload_whose_encoding_exceeds_one_response_is_reported_not_blocking() {
    let mut fx = fixture(&["cto", "mado"], 10).await;
    fx.scope.budget.max_payload_bytes = 600_000;
    fx.scope.budget.max_bytes_per_seat = 2_000_000;
    FileWorkMessageStore::new(fx.owner_dir.clone())
        .declare(&fx.scope)
        .unwrap();
    // A local sender admits 200000 control bytes; each encodes as six JSON
    // bytes, so no single response can carry the message.
    let local = cosmon_core::work_message::AdmittedWorkCaller::new(
        fx.scope.owner.clone(),
        seat("mado"),
        cosmon_core::work_message::SenderEvidence::CallerEnvSameUid,
    );
    cosmon_state::work_ops::WorkOperations::new(fx.owner_dir.clone())
        .send(
            &local,
            cosmon_state::work_ops::SendRequest {
                scope_revision: fx.scope.revision().unwrap(),
                recipient: seat("cto"),
                key: Some(cosmon_core::work_message::MessageKey::new("escaped").unwrap()),
                sender_time: Utc::now(),
                reply_to: None,
                phase: None,
                confidentiality: cosmon_core::work_message::Confidentiality::Internal,
                ttl_secs: None,
            },
            "\u{1}".repeat(200_000).as_bytes(),
            Utc::now(),
        )
        .unwrap();
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("text", "cto", "ok")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = fx
        .request("cto", "POST", "/inbox", Some(fx.pull()), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{status}");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["envelope"]["key"], "text");
    assert_eq!(body["undeliverable"][0]["envelope"]["key"], "escaped");
    assert_eq!(body["undeliverable"][0]["reason"], "payload_too_large");
}

/// A revocation that lands after the request was admitted, but before its
/// effect, must still refuse the effect: W-ACK rechecks the binding inside
/// the commit, under the binding lock. Holding that lock pins the request
/// between admission and commit, so the order is deterministic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_between_admission_and_commit_refuses_the_ack() {
    let fx = fixture(&["cto", "mado"], 10).await;
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("finding", "cto", "finding")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, pulled) = fx
        .request("cto", "POST", "/inbox", Some(fx.pull()), true)
        .await;
    assert_eq!(status, StatusCode::OK, "{pulled}");
    let ack = json!({"scope_revision": fx.scope.revision().unwrap(),
        "payload_digest": pulled["messages"][0]["envelope"]["payload_digest"],
        "disposition": "considered"});
    let collaboration = fx.tenant_state.join("collaboration");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(collaboration.join("bindings.lock"))
        .unwrap();
    lock.lock().unwrap();
    let fx = Arc::new(fx);
    let pending = {
        let fx = Arc::clone(&fx);
        tokio::spawn(async move {
            fx.request("cto", "POST", "/messages/finding/ack", Some(ack), true)
                .await
        })
    };
    std::thread::sleep(Duration::from_millis(500));
    let consumed = |fx: &Fixture| {
        FileWorkMessageStore::new(fx.owner_dir.clone())
            .load_all()
            .unwrap()
            .receipts
            .iter()
            .filter(|r| matches!(r.stage, Stage::Consumed { .. }))
            .count()
    };
    assert_eq!(
        consumed(&fx),
        0,
        "the ack effect must wait for the binding lock"
    );
    // Revoke while the request waits at its commit; the store writer would
    // need this same lock, so write its record directly.
    let binding = &fx.clients["cto"].binding;
    let mut bytes = serde_json::to_vec_pretty(&binding.revoked(Utc::now())).unwrap();
    bytes.push(b'\n');
    std::fs::write(
        collaboration
            .join("bindings")
            .join(format!("{}.json", binding.id)),
        bytes,
    )
    .unwrap();
    lock.unlock().unwrap();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        consumed(&fx),
        0,
        "a revoked binding records no acknowledgment"
    );
}

const READ_ONLY: &[&str] = &["cosmon:work:read"];
const UNRELATED: &[&str] = &["cosmon:sessions:read"];

/// Everything a refused request must leave untouched: admissions, delivery
/// attempts and consumption receipts all live in the work store.
fn work_records(fx: &Fixture) -> (usize, usize) {
    let all = FileWorkMessageStore::new(fx.owner_dir.clone())
        .load_all()
        .unwrap();
    (all.envelopes.len(), all.receipts.len())
}

/// The five W4 routes, each as `(method, suffix, body)` for a caller whose
/// request would otherwise be valid.
fn routes(fx: &Fixture, key: &str) -> [(&'static str, String, Option<Value>); 5] {
    let ack = json!({"scope_revision": fx.scope.revision().unwrap(),
        "payload_digest": "unused", "disposition": "considered"});
    let peek = format!(
        "/inbox?scope_revision={}&adapter=pull",
        fx.scope.revision().unwrap()
    );
    [
        ("GET", String::new(), None),
        (
            "POST",
            "/messages".to_owned(),
            Some(fx.send("probe", "cto", "probe")),
        ),
        ("GET", peek, None),
        ("POST", "/inbox".to_owned(), Some(fx.pull())),
        ("POST", format!("/messages/{key}/ack"), Some(ack)),
    ]
}

#[tokio::test]
async fn a_read_only_token_is_refused_on_the_write_routes_and_leaves_no_record() {
    let fx = fixture_scoped(&["cto", "mado"], 10, &[("mado", READ_ONLY)]).await;
    let (status, sent) = fx
        .request(
            "cto",
            "POST",
            "/messages",
            Some(fx.send("finding", "mado", "finding")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {sent}");
    let before = work_records(&fx);
    let [_, send, _, _, ack] = routes(&fx, "finding");
    for (name, (method, suffix, body)) in [("W-SEND", send), ("W-ACK", ack)] {
        let (status, body) = fx.request("mado", method, &suffix, body, true).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {body}");
        assert_eq!(body["error"], "scope_missing", "{name}: {body}");
    }
    assert_eq!(work_records(&fx), before, "a refusal records nothing");
}

#[tokio::test]
async fn a_token_without_any_work_scope_is_refused_on_all_five_routes() {
    let fx = fixture_scoped(&["cto", "mado"], 10, &[("mado", UNRELATED)]).await;
    let (status, sent) = fx
        .request(
            "cto",
            "POST",
            "/messages",
            Some(fx.send("finding", "mado", "finding")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {sent}");
    let before = work_records(&fx);
    let names = ["W-LIST", "W-SEND", "W-PEEK", "W-PULL", "W-ACK"];
    for (name, (method, suffix, body)) in names.into_iter().zip(routes(&fx, "finding")) {
        let (status, body) = fx.request("mado", method, &suffix, body, true).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {body}");
        assert_eq!(body["error"], "scope_missing", "{name}: {body}");
    }
    assert_eq!(work_records(&fx), before, "a refusal records nothing");
}

#[tokio::test]
async fn a_read_only_token_succeeds_on_the_read_routes() {
    let fx = fixture_scoped(&["cto", "mado"], 10, &[("mado", READ_ONLY)]).await;
    let (status, sent) = fx
        .request(
            "cto",
            "POST",
            "/messages",
            Some(fx.send("finding", "mado", "finding")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {sent}");
    let [list, _, peek, pull, _] = routes(&fx, "finding");
    for (name, (method, suffix, body)) in [("W-LIST", list), ("W-PEEK", peek), ("W-PULL", pull)] {
        let (status, body) = fx.request("mado", method, &suffix, body, true).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
    }
}

#[tokio::test]
async fn a_write_only_token_succeeds_on_the_write_routes_and_implies_read() {
    // Spec section 3: `cosmon:work:write` implies `cosmon:work:read`, so the
    // default seat token (write only) is admitted on all five routes.
    let fx = fixture(&["cto", "mado"], 10).await;
    let (status, sent) = fx
        .request(
            "cto",
            "POST",
            "/messages",
            Some(fx.send("finding", "mado", "finding")),
            true,
        )
        .await;
    assert!(status.is_success(), "{status} {sent}");
    let ack = json!({"scope_revision": fx.scope.revision().unwrap(),
        "payload_digest": sent["envelope"]["payload_digest"], "disposition": "considered"});
    let (status, body) = fx
        .request("mado", "POST", "/messages/finding/ack", Some(ack), true)
        .await;
    assert_eq!(status, StatusCode::OK, "W-ACK: {body}");
    let [list, _, peek, pull, _] = routes(&fx, "finding");
    for (name, (method, suffix, body)) in [("W-LIST", list), ("W-PEEK", peek), ("W-PULL", pull)] {
        let (status, body) = fx.request("mado", method, &suffix, body, true).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
    }
    let (status, body) = fx
        .request(
            "mado",
            "POST",
            "/messages",
            Some(fx.send("reply", "cto", "reply")),
            true,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "W-SEND: {body}");
}
