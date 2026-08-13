// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test: a tackle rejected by the `cs` subprocess must leave
//! the captured `stderr_excerpt` in the server log at **`warn`**.
//!
//! Why this deserves a test of its own.
//!
//! `503 tackle_unavailable` is a catch-all: every non-zero `cs` exit that
//! is not "already active" maps to it. An unreachable adapter backend, a
//! missing worker credential and a spawn failure are therefore
//! indistinguishable from the response alone. The one thing that CAN tell
//! them apart is the subprocess stderr, which the route already captures
//! into [`RppRejectReason::SubprocessExitNonZero`].
//!
//! Emitting it at `debug` made it unreachable in practice: a deployment
//! runs at `info`, where a failed tackle leaves behind nothing but the
//! `tower_http` `on_failure` line. A hardened instance exposes neither
//! Docker nor logs, so raising the level was the difference between a
//! diagnosable failure and a three-week blind hunt.
//!
//! The excerpt still does not cross the HTTP boundary — that is
//! deliberate, and this test asserts it too: the caller gets a stable
//! label plus a `request_id`, and whoever holds the logs correlates the
//! two.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use cosmon_oidc_testkit::fake_cs_path;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, JwksStore, Posture};
use serde_json::Value;
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::Level;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::Registry;

// ---------------------------------------------------------------------------
// Capturing subscriber
// ---------------------------------------------------------------------------

/// One recorded `tracing` event, reduced to what this test asserts on.
#[derive(Clone, Debug)]
struct Captured {
    level: Level,
    message: String,
    stderr_excerpt: Option<String>,
}

#[derive(Default)]
struct FieldGrab {
    message: String,
    stderr_excerpt: Option<String>,
}

impl Visit for FieldGrab {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => value.clone_into(&mut self.message),
            "stderr_excerpt" => self.stderr_excerpt = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        // `tracing` renders a bare `stderr_excerpt` field through `Debug`
        // when it is recorded as a `&String`, so both arms must look.
        match field.name() {
            "message" => self.message = rendered,
            "stderr_excerpt" => self.stderr_excerpt = Some(rendered),
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
struct CaptureLayer(Arc<Mutex<Vec<Captured>>>);

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut grab = FieldGrab::default();
        event.record(&mut grab);
        self.0.lock().unwrap().push(Captured {
            level: *event.metadata().level(),
            message: grab.message,
            stderr_excerpt: grab.stderr_excerpt,
        });
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn make_state(
    oidc: &OidcMock,
    tenants: &TenantWorkspaces,
    security_dir: &std::path::Path,
) -> AppState {
    let _ = oidc.write_jwks_file(security_dir).unwrap();
    let jwks = JwksStore::load(security_dir).unwrap();

    let nucleon_map = HabilitationMap::builder()
        .insert(
            oidc.issuer(),
            "sub-a",
            HabilitationId::new("nuc-a"),
            Noyau::new("a"),
            "cosmon-rpp-a",
        )
        .build();

    let rate_limiter = IngressRateLimiter::new(security_dir.join("oidc-rate-limit"), 256.0, 0.0);
    let deny_list = DenyList::new(security_dir.to_path_buf()).with_ttl(Duration::from_secs(0));

    AppState {
        cs_path: fake_cs_path(),
        state_dir: security_dir.to_path_buf(),
        inbox_root: security_dir.join("whispers/inbox"),
        galaxies_root: tenants.galaxies_root().to_path_buf(),
        jwks: cosmon_rpp_adapter::SharedJwksStore::new(jwks),
        nucleon_map: cosmon_rpp_adapter::SharedHabilitationMap::new(nucleon_map),
        rate_limiter: Arc::new(rate_limiter),
        deny_list: Arc::new(deny_list),
        posture: Posture::Prepared,
        subprocess_timeout: Duration::from_secs(10),
        anthropic_api_key: None,
        claude_model: None,
        backend_health: Arc::new(BackendHealthRegistry::new()),
        auth_claude: None,
        artifact_root: std::path::PathBuf::from("/tmp/cosmon"),
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

/// `fake-cs` has no `tackle` verb: it prints `fake-cs: unknown invocation …`
/// on stderr and exits 2. That is exactly the shape this test needs — a
/// non-zero exit carrying a diagnosable stderr.
#[tokio::test(flavor = "current_thread")]
async fn rejected_tackle_logs_stderr_excerpt_at_warn() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = Registry::default().with(CaptureLayer(Arc::clone(&captured)));

    let mut tenants = TenantWorkspaces::new();
    let _ = tenants.add("a");
    tenants
        .tenant("a")
        .expect("noyau 'a' must be registered")
        .insert_molecule(
            "task-20260812-warn",
            &serde_json::json!({"status": "pending"}),
        )
        .unwrap();

    let oidc = OidcMock::start_with(OidcMockConfig {
        audiences: vec!["cosmon-rpp-a".to_owned()],
        ..OidcMockConfig::default()
    })
    .await;

    let security_dir = tempfile::tempdir().unwrap();
    let app = router(make_state(&oidc, &tenants, security_dir.path()));
    let jwt = oidc.issue(&IssueJwt {
        subject: "sub-a",
        audience: Some("cosmon-rpp-a"),
        scopes: &["cosmon:molecule:write", "cosmon:worker:spawn"],
        lifetime_secs: Some(60),
        jti: Some("jti-warn-level"),
    });

    // `set_default` (guard) rather than `with_default` (closure): the
    // closure form would only cover *building* the future, not awaiting
    // it, and the event is emitted during the await.
    let guard = tracing::subscriber::set_default(subscriber);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/molecules/task-20260812-warn/tackle")
                .header("authorization", format!("Bearer {jwt}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    drop(guard);

    let status = resp.status();
    let body_bytes = to_bytes(resp.into_body(), 8192).await.unwrap();
    let body: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a non-zero `cs` exit must map to 503; body: {body}"
    );

    let events = captured.lock().unwrap().clone();
    let rejection = events
        .iter()
        .find(|e| e.message.contains("tackle subprocess rejected"))
        .unwrap_or_else(|| {
            panic!(
                "no rejection event recorded; captured messages: {:?}",
                events.iter().map(|e| &e.message).collect::<Vec<_>>()
            )
        });

    assert_eq!(
        rejection.level,
        Level::WARN,
        "the rejection must be emitted at `warn` — at `debug` it is invisible \
         to a deployment running at `info`, which is the whole point"
    );

    let excerpt = rejection
        .stderr_excerpt
        .as_deref()
        .expect("the rejection event must carry the captured subprocess stderr");
    assert!(
        !excerpt.trim().is_empty(),
        "an empty excerpt diagnoses nothing; got {excerpt:?}"
    );

    // The other half of the contract: the excerpt stays server-side.
    assert_eq!(
        body["error"], "tackle_unavailable",
        "the caller keeps a stable label"
    );
    assert!(
        body.get("detail").is_none() && body.get("stderr_excerpt").is_none(),
        "subprocess stderr must NOT cross the HTTP boundary; body: {body}"
    );
    assert!(
        body.get("request_id").is_some(),
        "the caller needs a `request_id` to correlate with the server log; body: {body}"
    );
}
