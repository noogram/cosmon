// SPDX-License-Identifier: AGPL-3.0-only

//! Real-handler conformance checks for the hand-written RPP read schemas.
//!
//! The OpenAPI document is intentionally not generated from Rust types: the
//! checked-in document is the public contract. These tests send requests
//! through the production router, then validate the resulting JSON against
//! that document. Renaming or removing a required handler field therefore
//! fails here, while an additive field remains compatible.

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn schemas() -> TestResult<BTreeMap<String, Value>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi/v1.yaml");
    let script = "import json,sys,yaml; print(json.dumps(yaml.safe_load(open(sys.argv[1], encoding='utf-8'))['components']['schemas']))";
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "OpenAPI YAML parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn validate(
    value: &Value,
    schema: &Value,
    schemas: &BTreeMap<String, Value>,
    path: &str,
) -> TestResult<()> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/components/schemas/")
            .ok_or_else(|| format!("unsupported schema reference {reference}"))?;
        let target = schemas
            .get(name)
            .ok_or_else(|| format!("missing component schema {name}"))?;
        return validate(value, target, schemas, path);
    }
    if let Some(choices) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        if choices
            .iter()
            .any(|choice| validate(value, choice, schemas, path).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path} matches none of the declared alternatives").into());
    }
    if schema.get("nullable") == Some(&Value::Bool(true)) && value.is_null() {
        return Ok(());
    }
    let types = match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !types.is_empty()
        && !types.iter().any(|kind| match *kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        })
    {
        return Err(format!("{path} has value {value}, expected type {types:?}").into());
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let object = value
            .as_object()
            .ok_or_else(|| format!("{path} must be an object"))?;
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                return Err(format!("{path} is missing required field {field}").into());
            }
        }
    }
    if let (Some(object), Some(properties)) = (
        value.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) {
        for (name, child_schema) in properties {
            if let Some(child) = object.get(name) {
                validate(child, child_schema, schemas, &format!("{path}.{name}"))?;
            }
        }
    }
    if let (Some(items), Some(item_schema)) = (value.as_array(), schema.get("items")) {
        for (index, item) in items.iter().enumerate() {
            validate(item, item_schema, schemas, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();
    let map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "schema-reader",
            HabilitationId::new("nuc-schema"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();
    AppState {
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(Arc::new(
            cosmon_transport::MockBackend::new(),
        )),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(map),
        rate_limiter: Arc::new(IngressRateLimiter::new(
            security_dir.join("oidc-rate-limit"),
            128.0,
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
        artifact_root: security_dir.join("artifacts"),
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

fn jwt(oidc: &OidcMock, jti: &str) -> String {
    oidc.issue(&IssueJwt {
        subject: "schema-reader",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:read", "cosmon:events:subscribe"],
        lifetime_secs: Some(120),
        jti: Some(jti),
    })
}

async fn get_json(app: &axum::Router, uri: &str, jwt: &str) -> TestResult<Value> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("Authorization", format!("Bearer {jwt}"))
                .body(Body::empty())?,
        )
        .await?;
    if response.status() != StatusCode::OK {
        return Err(format!("{uri} returned {}", response.status()).into());
    }
    Ok(serde_json::from_slice(
        &to_bytes(response.into_body(), 1024 * 1024).await?,
    )?)
}

async fn next_sse_data(body: &mut Body, buffer: &mut String) -> TestResult<(String, Value)> {
    loop {
        if let Some(end) = buffer.find("\n\n") {
            let block: String = buffer.drain(..end + 2).collect();
            let mut event = None;
            let mut data = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    event = Some(value.trim().to_owned());
                }
                if let Some(value) = line.strip_prefix("data:") {
                    data = Some(serde_json::from_str(value.trim())?);
                }
            }
            if let (Some(event), Some(data)) = (event, data) {
                return Ok((event, data));
            }
        }
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await?
            .ok_or("ledger stream closed before a frame")??;
        if let Ok(bytes) = frame.into_data() {
            buffer.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
}

#[tokio::test]
async fn real_read_handlers_conform_to_the_checked_in_schemas() -> TestResult<()> {
    let schemas = schemas()?;
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    tenant.insert_molecule(
        "task-20261006-schema",
        &json!({
            "status": "running",
            "kind": "task",
            "originating_branch": "feat/task-20261006-schema",
            "created_at": "2026-10-06T08:00:00Z",
            "updated_at": "2026-10-06T08:01:00Z"
        }),
    )?;
    std::fs::write(
        tenant.state_dir.join("events.jsonl"),
        b"{\"type\":\"molecule_evolved\",\"molecule_id\":\"task-20261006-schema\",\"step\":1,\"total\":2}\n",
    )?;
    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;
    let security = tempfile::tempdir()?;
    let app = router(make_state(&oidc, &tenants, security.path()));

    for (uri, schema_name, jti) in [
        ("/v1/molecules", "EnsembleEnvelope", "schema-list"),
        (
            "/v1/molecules/task-20261006-schema",
            "MoleculeEnvelope",
            "schema-observe",
        ),
        (
            "/v1/molecules/task-20261006-schema/status",
            "StatusEnvelope",
            "schema-status",
        ),
        ("/v1/auth/me", "AuthMeResponse", "schema-auth-me"),
    ] {
        let body = get_json(&app, uri, &jwt(&oidc, jti)).await?;
        let schema = schemas
            .get(schema_name)
            .ok_or_else(|| format!("missing component schema {schema_name}"))?;
        validate(&body, schema, &schemas, schema_name)?;
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/ledger")
                .header(
                    "Authorization",
                    format!("Bearer {}", jwt(&oidc, "schema-ledger")),
                )
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut buffer = String::new();
    let (epoch_event, epoch) = next_sse_data(&mut body, &mut buffer).await?;
    assert_eq!(epoch_event, "ledger.epoch");
    validate(
        &epoch,
        schemas
            .get("LedgerEpochFrame")
            .ok_or("missing LedgerEpochFrame")?,
        &schemas,
        "LedgerEpochFrame",
    )?;
    let (_, ledger_event) = next_sse_data(&mut body, &mut buffer).await?;
    validate(
        &ledger_event,
        schemas
            .get("LedgerEventFrame")
            .ok_or("missing LedgerEventFrame")?,
        &schemas,
        "LedgerEventFrame",
    )?;
    Ok(())
}

#[test]
fn removing_or_renaming_a_required_field_is_rejected() -> TestResult<()> {
    let schemas = schemas()?;
    let schema = schemas
        .get("StatusEnvelope")
        .ok_or("missing StatusEnvelope")?;
    let valid = json!({
        "request_id": "req",
        "molecule_id": "task-20261006-schema",
        "status": "running",
        "phase": "live",
        "updated_at": "2026-10-06T08:01:00Z",
        "terminal": false
    });
    validate(&valid, schema, &schemas, "StatusEnvelope")?;
    let mut renamed = valid;
    renamed["state"] = renamed["status"].take();
    assert!(validate(&renamed, schema, &schemas, "StatusEnvelope").is_err());
    Ok(())
}
