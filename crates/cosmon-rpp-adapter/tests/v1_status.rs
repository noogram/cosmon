// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/molecules/{id}/status` — the poll surface (issue #51 follow-up).
//!
//! The brief's falsifiers 1, 2 and 3 live here; the client-side half of 3, and
//! 4, 5 and 6, are in `cosmon-remote/tests/wait_flow.rs`. Each was watched fail
//! before the route existed — the whole file 404'd on an unmounted path.
//!
//! 1. The status read is strictly cheaper than the full molecule read, asserted
//!    on payload size and on the *field set*: the three fields the full read
//!    pays a growing log scan for (`energy`, `api_tokens`, `model`) are absent
//!    here, which is the read that is not happening.
//! 1b. …and on **cost**, which is the property those two are proxies for. A
//!    handler that folds the full observation and then projects the same six
//!    fields passes both of them; it does not survive a measurement taken
//!    with megabytes planted in the three logs.
//! 2. A conditional poll on an unchanged molecule gets `304` and no body.
//! 3. No server-side state is keyed by a waiter — there is no `wait` route to
//!    create any, which this pins structurally.
//!
//! Plus the invariants every molecule read carries: tenant isolation, the
//! scope gate, and a malformed id answering `404` rather than an existence
//! oracle.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
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
    _tenants: TenantWorkspaces,
    _security_dir: tempfile::TempDir,
    app: axum::Router,
}

/// Build the app with tenants `a` and `b` bound to distinct noyaux, so the
/// admission-boundary falsifier has a second tenant to fail against.
async fn fixture() -> Fixture {
    let mut tenants = TenantWorkspaces::new();
    let tenant = tenants.add("a");
    let _ = tenants.add("b");

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned(), "cosmon-rpp-b".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
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
        harvest_effect: Arc::new(cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::Fixed(
            cosmon_rpp_adapter::worker_env::SharedBackend(Arc::new(
                cosmon_transport::MockBackend::new(),
            )),
        ),
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
        artifact_root: security_dir.path().join("artifacts"),
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
    };
    let app = router(state);
    Fixture {
        oidc,
        tenant,
        _tenants: tenants,
        _security_dir: security_dir,
        app,
    }
}

fn jwt(fx: &Fixture, sub: &str, audience: &str, scopes: &[&str], jti: &str) -> String {
    fx.oidc.issue(&IssueJwt {
        subject: sub,
        audience: Some(audience),
        scopes,
        lifetime_secs: Some(60),
        jti: Some(jti),
    })
}

/// The read grant this route requires — the same one the full molecule read
/// requires, because this is a strictly smaller answer about the same object.
fn read_jwt(fx: &Fixture, jti: &str) -> String {
    jwt(fx, "sub-a", "cosmon-rpp-a", &["cosmon:molecule:read"], jti)
}

/// Issue a GET and return status, headers and raw body bytes.
///
/// The bytes, not a parsed `Value`: falsifier 1 is a claim about payload size
/// and falsifier 2 about a body that is not there, and both are lost the
/// moment the body is deserialised.
async fn get(
    fx: &Fixture,
    uri: &str,
    token: Option<&str>,
    if_none_match: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if let Some(tag) = if_none_match {
        req = req.header("If-None-Match", tag);
    }
    let resp = fx
        .app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    (status, headers, bytes.to_vec())
}

fn seed(tenant: &TenantPath, id: &str, status: &str) {
    tenant
        .insert_molecule(
            id,
            &serde_json::json!({
                "status": status,
                "assigned_worker": "ruby",
                "adapter": "claude",
            }),
        )
        .unwrap();
}

/// Falsifier 1 — the poll is strictly cheaper than the full molecule read.
///
/// Three measurements, because payload size alone is a weak claim. The
/// load-bearing one is the THIRD: a molecule with variables, tags, links and
/// completed steps makes the full read grow, and leaves the status answer the
/// same size to the byte. That is the shape of the defect — polling the full
/// read costs more the more there is to say about a molecule, and the more
/// there is to say the longer it has been running — and it is why `wait` needs
/// a route rather than a smaller rendering of this one.
///
/// The field set is the second measurement: `energy`, `api_tokens` and `model`
/// are exactly the three projections `observe` folds by scanning append-only
/// logs. Their absence is the read that is not happening; payload size alone
/// would be satisfied by a handler that does every scan and throws it away.
#[tokio::test]
async fn the_status_read_is_strictly_cheaper_than_the_full_molecule_read() {
    let fx = fixture().await;
    seed(&fx.tenant, "task-20260907-0001", "running");
    fx.tenant
        .insert_molecule(
            "task-20260907-0009",
            &serde_json::json!({
                "status": "running",
                "assigned_worker": "ruby",
                "adapter": "claude",
                "variables": {
                    "topic": "a molecule with something to say about itself",
                    "formula": "task-work",
                    "base_branch": "main",
                },
                "tags": ["fleet:default", "issue:51", "surface:rpp"],
                "links": ["task-20260907-e376", "task-20260907-6ddc"],
                "completed_steps": ["implement", "verify"],
                "current_step": 2,
            }),
        )
        .unwrap();
    let token = read_jwt(&fx, "cheap-1");

    let (s1, _, small) = get(
        &fx,
        "/v1/molecules/task-20260907-0001/status",
        Some(&token),
        None,
    )
    .await;
    let (s2, _, big) = get(&fx, "/v1/molecules/task-20260907-0001", Some(&token), None).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert!(
        small.len() * 2 < big.len(),
        "status payload {} B is not decisively smaller than the full read's {} B",
        small.len(),
        big.len(),
    );

    let (_, _, small_fat) = get(
        &fx,
        "/v1/molecules/task-20260907-0009/status",
        Some(&token),
        None,
    )
    .await;
    let (_, _, big_fat) = get(&fx, "/v1/molecules/task-20260907-0009", Some(&token), None).await;
    assert_eq!(
        small_fat.len(),
        small.len(),
        "the status answer must not grow with the molecule — that is the \
         whole reason it exists",
    );
    assert!(
        big_fat.len() > big.len(),
        "fixture is not exercising the claim: the full read did not grow \
         ({} B → {} B)",
        big.len(),
        big_fat.len(),
    );

    let small: Value = serde_json::from_slice(&small).unwrap();
    let obj = small.as_object().expect("status body is an object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "molecule_id",
            "phase",
            "request_id",
            "status",
            "terminal",
            "updated_at"
        ],
        "a field added here is a field every poll pays for",
    );
    assert_eq!(obj["status"], "running");
    assert_eq!(obj["phase"], "live");
    assert_eq!(obj["terminal"], false);
    assert!(
        obj["updated_at"].as_str().is_some_and(|s| !s.is_empty()),
        "a poller needs to know WHEN, not only what",
    );
}

/// Falsifier 1b — the status read's **cost** does not grow with the logs.
///
/// The payload test above is a proxy and cannot be anything else: a handler
/// that calls the full `observe` and then projects the same six fields emits
/// byte-identical answers and passes every assertion in it. Payload size is
/// what the client pays; what the *server* pays is reads, and the three reads
/// this route exists to avoid — the coupling report's `log/energy.jsonl`, the
/// token meter's `instrumentation/tokens.jsonl`, and the model attribution's
/// `events.jsonl` — are invisible in the answer. They are tolerant readers: a
/// missing or unreadable log yields `None`, never an error, so no response
/// byte and no status code can report whether they ran.
///
/// The one channel that can is cost at scale. So: measure the route against a
/// tenant whose logs are empty, plant megabytes in all three, and measure
/// again. The claim is that the second number is the first — the status answer
/// is a function of `state.json` alone, and `state.json` did not change. The
/// full molecule read on the same molecule is measured beside it as the
/// fixture's own guard: if *it* did not slow down, the logs are not being read
/// by anybody and this test is asserting nothing.
///
/// Thresholds are one-sided and generous. The claim is a *difference in kind*
/// — constant versus linear in log size — so a 4× ceiling on a route that
/// should not move at all, against a full read that moves by an order of
/// magnitude, has room for a loaded CI box without having room for the defect.
#[tokio::test]
async fn the_status_read_does_not_pay_for_the_logs_it_does_not_read() {
    let fx = fixture().await;
    seed(&fx.tenant, "task-20260907-0010", "running");
    let token = read_jwt(&fx, "cost-1");

    // Best-of-N: the minimum is the run the scheduler left alone, and it is
    // the only honest estimate of what the work itself costs.
    async fn best_of(fx: &Fixture, uri: &str, token: &str) -> Duration {
        let mut best = Duration::MAX;
        for _ in 0..7 {
            let started = std::time::Instant::now();
            let (status, _, _) = get(fx, uri, Some(token), None).await;
            assert_eq!(status, StatusCode::OK);
            best = best.min(started.elapsed());
        }
        best
    }

    const STATUS_URI: &str = "/v1/molecules/task-20260907-0010/status";
    const FULL_URI: &str = "/v1/molecules/task-20260907-0010";
    let lean_status = best_of(&fx, STATUS_URI, &token).await;
    let lean_full = best_of(&fx, FULL_URI, &token).await;

    // Plant the three logs the full read folds. The lines are well-formed and
    // about *other* molecules, so every reader parses them in full and then
    // discards them — the exact work a poll must not repeat.
    let state_dir = &fx.tenant.state_dir;
    std::fs::create_dir_all(state_dir.join("log")).unwrap();
    std::fs::create_dir_all(state_dir.join("instrumentation")).unwrap();
    let mut energy = String::new();
    let mut tokens = String::new();
    let mut events = String::new();
    for i in 0..40_000 {
        let other = format!("task-20260101-{:04x}", i % 0xffff);
        energy.push_str(&format!(
            r#"{{"molecule":"{other}","joules":1.5,"at":"2026-01-01T00:00:00Z"}}"#
        ));
        energy.push('\n');
        tokens.push_str(&format!(
            r#"{{"molecule_id":"{other}","input_tokens":11,"output_tokens":22,"at":"2026-01-01T00:00:00Z"}}"#
        ));
        tokens.push('\n');
        events.push_str(&format!(
            r#"{{"seq":{i},"event":{{"type":"ModelSelected","mol_id":"{other}"}}}}"#
        ));
        events.push('\n');
    }
    std::fs::write(state_dir.join("log/energy.jsonl"), &energy).unwrap();
    std::fs::write(state_dir.join("instrumentation/tokens.jsonl"), &tokens).unwrap();
    std::fs::write(state_dir.join("events.jsonl"), &events).unwrap();

    let fat_status = best_of(&fx, STATUS_URI, &token).await;
    let fat_full = best_of(&fx, FULL_URI, &token).await;

    // The fixture's own guard, the cost twin of the payload test's
    // `big_fat > big`: if the full read did not notice several megabytes of
    // log, nothing here is reading them and the assertion below is vacuous.
    assert!(
        fat_full > lean_full * 3,
        "fixture is not exercising the claim: the full read barely moved \
         ({lean_full:?} → {fat_full:?}) against {} B of planted log",
        energy.len() + tokens.len() + events.len(),
    );

    assert!(
        fat_status < (lean_status * 4).max(Duration::from_millis(20)),
        "the status read grew with the logs ({lean_status:?} → {fat_status:?}) \
         — it is folding scans it has no business folding; the full read went \
         {lean_full:?} → {fat_full:?} over the same logs",
    );
    assert!(
        fat_status * 3 < fat_full,
        "the status read is no longer decisively cheaper than the full read \
         ({fat_status:?} vs {fat_full:?})",
    );
}

/// Falsifier 2 — a conditional poll on an unchanged molecule gets `304` and no
/// body.
#[tokio::test]
async fn an_unchanged_molecule_answers_304_with_no_body() {
    let fx = fixture().await;
    seed(&fx.tenant, "task-20260907-0002", "running");
    let token = read_jwt(&fx, "cond-1");

    let (first, headers, body) = get(
        &fx,
        "/v1/molecules/task-20260907-0002/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(first, StatusCode::OK);
    assert!(!body.is_empty());
    let etag = headers
        .get("etag")
        .expect("the route must issue a tag or a poller cannot be conditional")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        etag.starts_with("W/\""),
        "{etag} must be a WEAK tag: the \
envelope carries a fresh request_id, so equal answers are not equal bytes"
    );

    let (second, headers2, body2) = get(
        &fx,
        "/v1/molecules/task-20260907-0002/status",
        Some(&token),
        Some(&etag),
    )
    .await;
    assert_eq!(second, StatusCode::NOT_MODIFIED);
    assert!(
        body2.is_empty(),
        "a 304 carries no body, got {} B",
        body2.len()
    );
    assert_eq!(
        headers2.get("etag").map(|v| v.to_str().unwrap()),
        Some(etag.as_str()),
        "the 304 must re-state the tag so the next poll stays conditional",
    );
}

/// The other half of falsifier 2: when the molecule DOES move, the same
/// conditional request must not be answered `304`. A route that always says
/// "unchanged" would pass the test above and hide every transition.
#[tokio::test]
async fn a_moved_molecule_breaks_the_conditional() {
    let fx = fixture().await;
    seed(&fx.tenant, "task-20260907-0003", "running");
    let token = read_jwt(&fx, "cond-2");

    let (_, headers, _) = get(
        &fx,
        "/v1/molecules/task-20260907-0003/status",
        Some(&token),
        None,
    )
    .await;
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_owned();

    seed(&fx.tenant, "task-20260907-0003", "completed");

    let (status, _, body) = get(
        &fx,
        "/v1/molecules/task-20260907-0003/status",
        Some(&token),
        Some(&etag),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a moved molecule must not 304");
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["status"], "completed");
    assert_eq!(body["terminal"], true, "the server owns the terminal set");
    assert_eq!(body["phase"], "done");
}

/// Falsifier 3, server half — there is no route that blocks, so nothing on the
/// server can hold a waiter. Structural: an unmounted path 404s, and a `wait`
/// route appearing later fails this test on the day it is added.
#[tokio::test]
async fn there_is_no_wait_route_to_hold_a_blocked_thread() {
    let fx = fixture().await;
    seed(&fx.tenant, "task-20260907-0004", "running");
    let token = read_jwt(&fx, "no-wait");
    for path in [
        "/v1/molecules/task-20260907-0004/wait",
        "/v1/molecules/task-20260907-0004/status/wait",
    ] {
        let (status, _, _) = get(&fx, path, Some(&token), None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} must not exist: a blocking route would hold one server \
             thread and one piece of state per waiting client",
        );
    }
}

/// Tenant isolation: a JWT for noyau `b` may not read noyau `a`'s molecule,
/// and the refusal is a `404` — never an existence oracle.
#[tokio::test]
async fn a_second_tenant_cannot_read_this_molecules_status() {
    let fx = fixture().await;
    seed(&fx.tenant, "task-20260907-0005", "running");
    let intruder = jwt(
        &fx,
        "sub-b",
        "cosmon-rpp-b",
        &["cosmon:molecule:read"],
        "cross-1",
    );
    let (status, _, _) = get(
        &fx,
        "/v1/molecules/task-20260907-0005/status",
        Some(&intruder),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The scope gate, and the malformed-id rule the whole surface follows.
#[tokio::test]
async fn the_gate_is_molecule_read_and_a_bad_id_is_a_404() {
    let fx = fixture().await;
    seed(&fx.tenant, "task-20260907-0006", "running");

    let (unauth, _, _) = get(&fx, "/v1/molecules/task-20260907-0006/status", None, None).await;
    assert_eq!(unauth, StatusCode::UNAUTHORIZED);

    let scopeless = jwt(
        &fx,
        "sub-a",
        "cosmon-rpp-a",
        &["cosmon:quota:read"],
        "gate-1",
    );
    let (forbidden, _, _) = get(
        &fx,
        "/v1/molecules/task-20260907-0006/status",
        Some(&scopeless),
        None,
    )
    .await;
    assert_eq!(forbidden, StatusCode::FORBIDDEN);

    // `:write` implies `:read` — the same rule the full molecule read follows.
    let writer = jwt(
        &fx,
        "sub-a",
        "cosmon-rpp-a",
        &["cosmon:molecule:write"],
        "gate-2",
    );
    let (ok, _, _) = get(
        &fx,
        "/v1/molecules/task-20260907-0006/status",
        Some(&writer),
        None,
    )
    .await;
    assert_eq!(ok, StatusCode::OK);

    let token = read_jwt(&fx, "gate-3");
    let (bad, _, _) = get(
        &fx,
        "/v1/molecules/not%20an%20id/status",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(bad, StatusCode::NOT_FOUND);
}
