// SPDX-License-Identifier: AGPL-3.0-only

//! HTTP collapse contract (issue #207, ADR-187): legacy classification and
//! response compatibility, conditional outcome matrix, durable receipt recovery,
//! tenant authorization and the shared lease guard, against real FileStores.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantPath, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    router, AppState, BackendHealthRegistry, JwksStore, Posture, SharedHabilitationMap,
};
use serde_json::Value;
use tower::ServiceExt;

struct Fixture {
    oidc: OidcMock,
    tenant: TenantPath,
    tenant_b: TenantPath,
    state: AppState,
    _tenants: TenantWorkspaces,
    _security_dir: tempfile::TempDir,
    _artifact_root: tempfile::TempDir,
    app: axum::Router,
}

async fn fixture() -> Fixture {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    let tenant_b = tenants.add("b");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned(), "cosmon-rpp-b".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let artifact_root = tempfile::tempdir().unwrap();
    let _ = oidc.write_jwks_file(security_dir.path()).unwrap();
    let jwks = JwksStore::load(security_dir.path()).unwrap();

    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .insert(
            oidc.issuer(),
            "sub-b",
            HabilitationId::new("nuc-b"),
            Noyau::new("b"),
            "cosmon-rpp-b",
        )
        .build();
    let rate_limiter =
        IngressRateLimiter::new(security_dir.path().join("oidc-rate-limit"), 64.0, 0.0);
    let deny_list =
        DenyList::new(security_dir.path().to_path_buf()).with_ttl(Duration::from_secs(0));

    let state = AppState {
        harvest_effect: std::sync::Arc::new(
            cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect,
        ),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(std::sync::Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.path().to_path_buf(),
        inbox_root: security_dir.path().join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        drain_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: artifact_root.path().to_path_buf(),
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
    };
    let app = router(state.clone());
    Fixture {
        oidc,
        tenant,
        tenant_b,
        state,
        _tenants: tenants,
        _security_dir: security_dir,
        _artifact_root: artifact_root,
        app,
    }
}

fn jwt_with_scopes(oidc: &OidcMock, scopes: &[&str], jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes,
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

/// Absolute path of the persistent molecule dir for a `default`-fleet
/// molecule under tenant `a`.
async fn post_collapse(fx: &Fixture, id: &str, jwt: &str, body: &Value) -> StatusCode {
    fx.app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/molecules/{id}/collapse"))
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn stored_state(fx: &Fixture, id: &str) -> Value {
    let path = fx
        .tenant
        .state_dir
        .join("fleets/default/molecules")
        .join(id)
        .join("state.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn collapse_route_records_the_requested_reason_kind() {
    let fx = fixture().await;
    let id = "task-20261014-aaaa";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({ "status": "pending" }))
        .unwrap();
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "jti-rk");

    let status = post_collapse(
        &fx,
        id,
        &jwt,
        &serde_json::json!({ "reason": "gate red", "reason_kind": "gate_failed" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let state = stored_state(&fx, id);
    assert_eq!(state["status"], "collapsed");
    assert_eq!(
        state["collapse_reason_kind"],
        serde_json::json!("gate_failed"),
        "the request's reason_kind must reach the stored molecule: {state}",
    );
}

#[tokio::test]
async fn collapse_route_without_reason_kind_records_none() {
    let fx = fixture().await;
    let id = "task-20261014-bbbb";
    fx.tenant
        .insert_molecule(id, &serde_json::json!({ "status": "pending" }))
        .unwrap();
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "jti-nork");

    let status = post_collapse(&fx, id, &jwt, &serde_json::json!({ "reason": "plain" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(stored_state(&fx, id)["collapse_reason_kind"].is_null());
}

const KEY: &str = "broker-request-0001";
const ID: &str = "task-20261017-aaaa";

async fn request_wire(
    app: &axum::Router,
    method: &str,
    uri: &str,
    jwt: Option<&str>,
    body: Value,
) -> anyhow::Result<(StatusCode, axum::http::HeaderMap, Vec<u8>)> {
    use http_body_util::BodyExt;
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json");
    if let Some(jwt) = jwt {
        req = req.header("Authorization", format!("Bearer {jwt}"));
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(serde_json::to_vec(&body)?))?)
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok((status, headers, bytes.to_vec()))
}

async fn request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    jwt: Option<&str>,
    body: Value,
) -> anyhow::Result<(StatusCode, axum::http::HeaderMap, Value)> {
    let (status, headers, wire) = request_wire(app, method, uri, jwt, body).await?;
    Ok((status, headers, serde_json::from_slice(&wire)?))
}

/// Locate the receipt value without reserializing or normalizing its wire bytes.
fn wire_receipt(wire: &[u8]) -> anyhow::Result<&[u8]> {
    let marker = b"\"receipt\":";
    let start = wire
        .windows(marker.len())
        .position(|w| w == marker)
        .ok_or_else(|| anyhow::anyhow!("receipt field absent"))?
        + marker.len();
    let mut stream = serde_json::Deserializer::from_slice(&wire[start..]).into_iter::<Value>();
    let receipt = stream
        .next()
        .ok_or_else(|| anyhow::anyhow!("receipt value absent"))??;
    anyhow::ensure!(receipt.is_object(), "receipt must be an object");
    Ok(&wire[start..start + stream.byte_offset()])
}

fn revision(tenant: &TenantPath, id: &str) -> anyhow::Result<String> {
    let bytes = std::fs::read(
        tenant
            .state_dir
            .join("fleets/default/molecules")
            .join(id)
            .join("state.json"),
    )?;
    Ok(cosmon_core::collapse_contract::MoleculeRevision::of_state_bytes(&bytes)?.to_string())
}

fn intent(fx: &Fixture, id: &str) -> anyhow::Result<Value> {
    Ok(
        serde_json::json!({"reason": "approved", "expect_status": "pending",
        "expect_revision": revision(&fx.tenant, id)?, "reason_kind": "gate_failed"}),
    )
}

fn uri(id: &str, key: &str) -> String {
    format!("/v1/molecules/{id}/collapse-receipts/{key}")
}

fn snapshot(
    root: &std::path::Path,
) -> anyhow::Result<std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>> {
    let mut out = std::collections::BTreeMap::new();
    if root.exists() {
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if path.is_dir() {
                out.extend(snapshot(&path)?);
            } else if !path.ends_with("dispatch.lock") {
                // Lock inode creation is coordination, not molecule/key state (B1 contract).
                out.insert(path.clone(), std::fs::read(path)?);
            }
        }
    }
    Ok(out)
}

#[tokio::test]
async fn get_revision_and_etag_hash_the_raw_snapshot_including_unknown_fields() -> anyhow::Result<()>
{
    let fx = fixture().await;
    fx.tenant.insert_molecule(
        ID,
        &serde_json::json!({"status": "pending", "future_field": {"b": 2, "a": 1}}),
    )?;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "revision");
    let (code, headers, body) = request(
        &fx.app,
        "GET",
        &format!("/v1/molecules/{ID}"),
        Some(&jwt),
        Value::Null,
    )
    .await?;
    assert_eq!(code, StatusCode::OK);
    let rev = revision(&fx.tenant, ID)?;
    assert_eq!(body["molecule"]["revision"], rev);
    assert_eq!(headers["etag"], format!("\"{rev}\""));
    assert!(stored_state(&fx, ID).get("revision").is_none());
    Ok(())
}

#[tokio::test]
async fn http_outcome_matrix_and_refusals_leave_state_and_keys_unchanged() -> anyhow::Result<()> {
    let cases = [
        ("pending", "unexpected_status", 409),
        ("pending", "stale_revision", 412),
        ("pending", "approval_expired", 412),
        ("completed", "already_completed", 409),
        ("collapsed", "already_collapsed", 409),
    ];
    for (status, outcome, code) in cases {
        let fx = fixture().await;
        fx.tenant
            .insert_molecule(ID, &serde_json::json!({"status": status}))?;
        let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "matrix");
        let mut body = intent(&fx, ID)?;
        match outcome {
            "unexpected_status" => body["expect_status"] = serde_json::json!("running"),
            "stale_revision" => {
                body["expect_revision"] = serde_json::json!(format!("b3:{}", "0".repeat(64)))
            }
            "approval_expired" => body["not_after"] = serde_json::json!("2000-01-01T00:00:00Z"),
            _ => {}
        }
        let before = snapshot(&fx.tenant.state_dir)?;
        let (actual_code, _, response) =
            request(&fx.app, "PUT", &uri(ID, KEY), Some(&jwt), body).await?;
        assert_eq!(actual_code.as_u16(), code, "{response}");
        assert_eq!(response["outcome"], outcome);
        assert_eq!(
            snapshot(&fx.tenant.state_dir)?,
            before,
            "refusal {outcome} wrote tenant state"
        );
    }
    Ok(())
}

#[tokio::test]
async fn lost_response_restart_replay_and_key_reuse_preserve_the_durable_receipt(
) -> anyhow::Result<()> {
    let fx = fixture().await;
    let other = "task-20261017-bbbb";
    for id in [ID, other] {
        fx.tenant
            .insert_molecule(id, &serde_json::json!({"status": "pending"}))?;
    }
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "replay");
    let body = intent(&fx, ID)?;
    let (status, _, first_wire) =
        request_wire(&fx.app, "PUT", &uri(ID, KEY), Some(&jwt), body.clone()).await?;
    let first: Value = serde_json::from_slice(&first_wire)?;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(first["outcome"], "transitioned");
    assert_eq!(stored_state(&fx, ID)["collapse_reason_kind"], "gate_failed");
    // The expected digest includes the verified subject, never an ambient server identity.
    let expected = cosmon_core::collapse_contract::CollapseIntentV1 {
        target: ID,
        reason: "approved",
        cause: None,
        account: None,
        kind: None,
        reason_kind: Some("gate_failed"),
        expected_status: "pending",
        expected_revision: body["expect_revision"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("revision"))?,
        actor: Some("jwt:sub-a"),
        approval_digest: None,
        not_after: None,
    }
    .digest()?;
    assert_eq!(first["digest"], expected.as_str());
    let receipt_bytes = wire_receipt(&first_wire)?.to_vec();
    let durable: cosmon_core::collapse_contract::CollapseReceipt =
        serde_json::from_value(stored_state(&fx, ID)["collapse_receipt"].clone())?;
    assert_eq!(
        receipt_bytes,
        durable.canonical_bytes()?,
        "PUT receipt must be canonical durable bytes"
    );
    // Recreate the router and its event bus. Every handler opens a fresh FileStore;
    // no response cache or in-process receipt survives this boundary.
    let mut restarted_state = fx.state.clone();
    restarted_state.events = Arc::new(cosmon_rpp_adapter::EventBus::with_default_capacity());
    let restarted = router(restarted_state);
    let (status, _, lookup_wire) =
        request_wire(&restarted, "GET", &uri(ID, KEY), Some(&jwt), Value::Null).await?;
    let lookup: Value = serde_json::from_slice(&lookup_wire)?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lookup["state"], "committed");
    assert_eq!(wire_receipt(&lookup_wire)?, receipt_bytes);
    let (status, _, replay_wire) =
        request_wire(&restarted, "PUT", &uri(ID, KEY), Some(&jwt), body.clone()).await?;
    let replay: Value = serde_json::from_slice(&replay_wire)?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["outcome"], "replayed");
    assert_eq!(wire_receipt(&replay_wire)?, receipt_bytes);
    for target in [ID, other] {
        let mut changed = body.clone();
        if target == ID {
            changed["reason"] = serde_json::json!("approved.");
        }
        let before = snapshot(&fx.tenant.state_dir)?;
        let (status, _, reused) =
            request(&restarted, "PUT", &uri(target, KEY), Some(&jwt), changed).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(reused["outcome"], "idempotency_key_reused");
        assert!(reused.get("receipt").is_none());
        assert_eq!(snapshot(&fx.tenant.state_dir)?, before);
    }
    Ok(())
}

#[tokio::test]
async fn receipt_authz_visibility_and_tenant_isolation_cover_read_and_write() -> anyhow::Result<()>
{
    let fx = fixture().await;
    fx.tenant
        .insert_molecule(ID, &serde_json::json!({"status": "pending"}))?;
    let write = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "authz-write");
    let read = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "authz-read");
    let none = jwt_with_scopes(&fx.oidc, &[], "authz-none");
    let b = fx.oidc.issue(&IssueJwt {
        subject: "sub-b",
        audience: Some("cosmon-rpp-b"),
        scopes: &["cosmon:molecule:write"],
        lifetime_secs: Some(60),
        jti: Some("tenant-b"),
    });
    let body = intent(&fx, ID)?;
    for method in ["GET", "PUT"] {
        assert_eq!(
            request(&fx.app, method, &uri(ID, KEY), None, body.clone())
                .await?
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&fx.app, method, &uri(ID, KEY), Some(&none), body.clone())
                .await?
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&fx.app, method, &uri(ID, KEY), Some(&b), body.clone())
                .await?
                .0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        request(&fx.app, "PUT", &uri(ID, KEY), Some(&read), body.clone())
            .await?
            .0,
        StatusCode::FORBIDDEN
    );
    let (_, _, unknown) = request(&fx.app, "GET", &uri(ID, KEY), Some(&read), Value::Null).await?;
    assert_eq!(unknown["state"], "unknown");
    assert_eq!(
        request(&fx.app, "PUT", &uri(ID, KEY), Some(&write), body.clone())
            .await?
            .0,
        StatusCode::CREATED
    );
    fx.tenant_b
        .insert_molecule(ID, &serde_json::json!({"status": "pending"}))?;
    let (_, _, unknown_b) = request(&fx.app, "GET", &uri(ID, KEY), Some(&b), Value::Null).await?;
    assert_eq!(unknown_b["state"], "unknown");
    assert!(unknown_b["receipt"].is_null());
    let mut body_b = body;
    body_b["expect_revision"] = serde_json::json!(revision(&fx.tenant_b, ID)?);
    assert_eq!(
        request(&fx.app, "PUT", &uri(ID, KEY), Some(&b), body_b)
            .await?
            .0,
        StatusCode::CREATED
    );
    let other = "task-20261017-cccc";
    fx.tenant
        .insert_molecule(other, &serde_json::json!({"status": "pending"}))?;
    let (_, _, bound) = request(&fx.app, "GET", &uri(other, KEY), Some(&read), Value::Null).await?;
    assert_eq!(bound["state"], "idempotency_key_reused");
    assert!(bound["receipt"].is_null());
    assert!(!serde_json::to_string(&bound)?.contains(ID));
    Ok(())
}

#[tokio::test]
async fn strict_keyed_body_rejects_typos_and_malformed_conditions() -> anyhow::Result<()> {
    let fx = fixture().await;
    fx.tenant
        .insert_molecule(ID, &serde_json::json!({"status": "pending"}))?;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "invalid");
    for (field, value) in [
        ("typo", serde_json::json!(true)),
        ("expect_status", serde_json::json!("completed")),
        ("expect_revision", serde_json::json!("oops")),
        ("not_after", serde_json::json!("yesterday")),
        ("reason", serde_json::json!(" ")),
    ] {
        let mut body = intent(&fx, ID)?;
        body[field] = value;
        let before = snapshot(&fx.tenant.state_dir)?;
        assert_eq!(
            request(&fx.app, "PUT", &uri(ID, KEY), Some(&jwt), body)
                .await?
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(snapshot(&fx.tenant.state_dir)?, before);
    }
    Ok(())
}

/// Seed an actual signed lease ledger in the tenant store, without lifecycle commands.
fn lease(tenant: &TenantPath, id: &str) -> anyhow::Result<()> {
    use cosmon_core::id::{MoleculeId, SessionId};
    use cosmon_core::operator_attestation::{GrantChallenge, OperatorAttestation, OperatorKeyId};
    use cosmon_core::pilot_lease::{LeaseEpoch, PilotLease};
    let operator = cosmon_minisign_testkit::Operator::from_seed(23);
    std::fs::write(
        tenant
            .state_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("cosmon dir"))?
            .join("takeover.pub"),
        operator.public_key_file(),
    )?;
    let mission = MoleculeId::new(id)?;
    let holder = SessionId::new("fixture-holder".to_owned())?;
    let challenge = GrantChallenge::new(
        mission.clone(),
        holder.clone(),
        LeaseEpoch::first(),
        "operator",
        None,
    )?;
    let signed = cosmon_notary::minisign::MinisignSignature::parse(
        &operator.sign(&challenge.canonical_bytes()),
    )?;
    let attestation = OperatorAttestation {
        key_id: OperatorKeyId::from_bytes(signed.key_id),
        signature: signed.signature_line(),
        global_signature: signed.global_signature_line(),
        trusted_comment: signed.trusted_comment.clone(),
        untrusted_comment: signed.untrusted_comment.clone(),
    };
    cosmon_harvest::pilot_gesture::leases_at(&tenant.state_dir)?.grant(
        &PilotLease::new(
            mission,
            holder,
            LeaseEpoch::first(),
            "operator",
            chrono::Utc::now(),
            None,
        )
        .attested_by(attestation),
    )?;
    Ok(())
}

#[tokio::test]
async fn both_http_collapse_routes_refuse_a_leased_mission_in_the_tenant_store(
) -> anyhow::Result<()> {
    let fx = fixture().await;
    fx.tenant
        .insert_molecule(ID, &serde_json::json!({"status": "pending"}))?;
    lease(&fx.tenant, ID)?;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "lease");
    let before = snapshot(&fx.tenant.state_dir)?;
    for (method, uri, body) in [
        (
            "POST",
            format!("/v1/molecules/{ID}/collapse"),
            serde_json::json!({"reason": "approved"}),
        ),
        ("PUT", uri(ID, KEY), intent(&fx, ID)?),
    ] {
        let (status, _, response) = request(&fx.app, method, &uri, Some(&jwt), body).await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method}: {response}");
        assert!(serde_json::to_string(&response)?.contains("unleased_pilot_gesture"));
        assert_eq!(snapshot(&fx.tenant.state_dir)?, before);
    }
    // Exercise the same authority locally: no JWT-to-session bridge is invented.
    use cosmon_core::collapse_contract::CallerContext;
    use cosmon_state::ops::collapse::CollapseAuthority;
    let authority =
        cosmon_harvest::pilot_gesture::LeaseCollapseAuthority::new(&fx.tenant.state_dir, "fixture");
    let id = cosmon_core::id::MoleculeId::new(ID)?;
    assert!(authority
        .check(&id, &CallerContext::Local { session: None })?
        .is_some());
    assert!(authority
        .check(
            &id,
            &CallerContext::Remote {
                subject: "sub-a".into()
            }
        )?
        .is_some());
    Ok(())
}

#[tokio::test]
async fn receipt_lookup_exposes_reserved_and_inconsistent_without_writing() -> anyhow::Result<()> {
    use cosmon_core::collapse_contract::{CollapseKeyRecord, IdempotencyKey, RequestDigest};
    use cosmon_core::id::MoleculeId;
    use cosmon_state::StateStore;
    let fx = fixture().await;
    fx.tenant
        .insert_molecule(ID, &serde_json::json!({"status": "pending"}))?;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:read"], "lookup-states");
    let key = IdempotencyKey::parse(KEY)?;
    let id = MoleculeId::new(ID)?;
    let store = cosmon_filestore::FileStore::new(&fx.tenant.state_dir);
    let digest = RequestDigest::try_from(format!("b3:{}", "0".repeat(64)))?;
    let record = CollapseKeyRecord::reserved(key.clone(), id.clone(), digest.clone());
    assert!(store.reserve_collapse_key(&record)?);
    let before = snapshot(&fx.tenant.state_dir)?;
    let (status, _, reserved) =
        request(&fx.app, "GET", &uri(ID, KEY), Some(&jwt), Value::Null).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reserved["state"], "reserved_not_applied");
    assert_eq!(reserved["digest"], digest.as_str());
    assert_eq!(snapshot(&fx.tenant.state_dir)?, before);
    let receipt = cosmon_core::collapse_contract::CollapseReceipt {
        schema: cosmon_core::collapse_contract::RECEIPT_SCHEMA.to_owned(),
        key,
        digest,
        molecule: id,
        previous_status: cosmon_core::molecule::MoleculeStatus::Pending,
        previous_revision: cosmon_core::collapse_contract::MoleculeRevision::parse(&revision(
            &fx.tenant, ID,
        )?)?,
        committed_at: chrono::Utc::now(),
    };
    // An inconsistent on-disk pair, e.g. after the known pre-B3 stale writer.
    store.commit_collapse_key(&record.committed(receipt))?;
    let before = snapshot(&fx.tenant.state_dir)?;
    let (status, _, inconsistent) =
        request(&fx.app, "GET", &uri(ID, KEY), Some(&jwt), Value::Null).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inconsistent["state"], "inconsistent");
    assert!(inconsistent["receipt"].is_null());
    assert_eq!(snapshot(&fx.tenant.state_dir)?, before);
    Ok(())
}

/// A keyed request waits out dispatch on a blocking thread, leaving HTTP reads live.
#[tokio::test]
async fn keyed_http_lock_wait_does_not_block_the_async_executor() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fx = fixture().await;
    fx.tenant
        .insert_molecule(ID, &serde_json::json!({"status": "pending"}))?;
    let jwt = jwt_with_scopes(&fx.oidc, &["cosmon:molecule:write"], "lock-wait");
    let body = intent(&fx, ID)?;
    let state_dir = fx.tenant.state_dir.clone();
    let released = Arc::new(AtomicBool::new(false));
    let observed_release = released.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || -> anyhow::Result<()> {
        let store = cosmon_filestore::FileStore::new(state_dir);
        let id = cosmon_core::id::MoleculeId::new(ID)?;
        let guard = store.acquire_dispatch_lock(&id)?;
        ready_tx.send(())?;
        // A bounded cleanup path makes a broken async boundary fail, not hang.
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
        observed_release.store(true, Ordering::SeqCst);
        drop(guard);
        Ok(())
    });
    ready_rx.recv_timeout(Duration::from_secs(5))?;
    let app = fx.app.clone();
    let put_jwt = jwt.clone();
    let put =
        tokio::spawn(
            async move { request(&app, "PUT", &uri(ID, KEY), Some(&put_jwt), body).await },
        );
    // This test runs on a single-thread Tokio executor. The spawned handler
    // must yield while the OS thread holds its dispatch lock.
    tokio::task::yield_now().await;
    let (status, _, _) = request(
        &fx.app,
        "GET",
        &format!("/v1/molecules/{ID}"),
        Some(&jwt),
        Value::Null,
    )
    .await?;
    let read_before_release = !released.load(Ordering::SeqCst);
    let _ = release_tx.send(());
    holder
        .join()
        .map_err(|_| anyhow::anyhow!("lock holder panicked"))??;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(put.await??.0, StatusCode::CREATED);
    assert!(
        read_before_release,
        "HTTP executor waited on dispatch.lock before serving the read"
    );
    Ok(())
}
