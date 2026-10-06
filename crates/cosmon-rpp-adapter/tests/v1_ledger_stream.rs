// SPDX-License-Identifier: AGPL-3.0-only

//! `GET /v1/ledger` — replay, then live, over the tenant ledger (issue #184).
//!
//! The acceptance list of the issue, one test per line:
//!
//! 1. append N mixed sequenced and unsequenced lines, connect with a cursor
//!    after line k, append M more while connected, receive exactly lines
//!    k+1..N+M once, in order;
//! 2. the same across a simulated adapter restart;
//! 3. a replaced file produces `ledger.reset`;
//! 4. a worker-side write appears;
//! 5. frames contain no excluded field;
//!
//! plus the scope pair, the one-stream-per-principal bound, the frame cap and
//! the refusal of a cursor that names another log.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_oidc_testkit::{IssueJwt, OidcMock, OidcMockConfig, TenantWorkspaces};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::{HabilitationId, HabilitationMap, Noyau};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::routes::ledger_stream::FRAME_CAP;
use cosmon_rpp_adapter::{router, AppState, BackendHealthRegistry, EventBus, JwksStore, Posture};
use cosmon_state::event_log::{emit_one, resolve_events_log_path};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

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
        harvest_effect: std::sync::Arc::new(
            cosmon_rpp_adapter::harvest_effect::UnavailableHarvestEffect,
        ),
        worker_backend: cosmon_rpp_adapter::worker_env::WorkerBackends::fixed(std::sync::Arc::new(
            cosmon_transport::MockBackend::new(),
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
        events: std::sync::Arc::new(EventBus::with_default_capacity()),
        metrics: std::sync::Arc::new(cosmon_rpp_adapter::MetricsRegistry::new()),
        drains: std::sync::Arc::new(cosmon_rpp_adapter::DrainRegistry::default()),
        admin_seal: std::sync::Arc::new(cosmon_rpp_adapter::admin_seal::AdminSeal::disabled()),
        provisioner: std::sync::Arc::new(cosmon_rpp_adapter::provisioner::Provisioner::inert()),
        portee_provisioner: std::sync::Arc::new(
            cosmon_rpp_adapter::portee::PorteeProvisioner::inert(),
        ),
    }
}

const AUD: &str = "cosmon-rpp-a";
const BOTH: &[&str] = &["cosmon:events:subscribe", "cosmon:molecule:read"];

/// A running deployment over one tenant `a` whose ledger the test writes.
struct Rig {
    oidc: OidcMock,
    tenants: TenantWorkspaces,
    security: tempfile::TempDir,
    /// Unique per rig: the one-stream bound is keyed by principal, and the
    /// tests of this file run in parallel in one process.
    sub: String,
}

impl Rig {
    async fn new() -> Self {
        let mut tenants = TenantWorkspaces::new();
        let _ = tenants.add("a");
        let oidc = OidcMock::start_with(OidcMockConfig {
            audiences: vec![AUD.to_owned()],
            ..OidcMockConfig::default()
        })
        .await;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        Self {
            oidc,
            tenants,
            security: tempfile::tempdir().unwrap(),
            sub: format!(
                "sub-{}",
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        }
    }

    /// Path of tenant `a`'s ledger; the file is created by the first append.
    fn ledger(&self) -> std::path::PathBuf {
        resolve_events_log_path(&self.tenants.tenant("a").unwrap().state_dir)
    }

    /// A fresh adapter over the same tenant state: what a restart yields.
    fn app(&self) -> axum::Router {
        router(make_state(
            &self.oidc,
            &self.tenants,
            vec![(self.sub.as_str(), "nuc-a", "a", AUD)],
            self.security.path(),
        ))
    }

    fn jwt(&self, scopes: &[&str], jti: &str) -> String {
        self.oidc.issue(&IssueJwt {
            subject: &self.sub,
            audience: Some(AUD),
            scopes,
            lifetime_secs: Some(120),
            jti: Some(jti),
        })
    }

    async fn open(
        &self,
        app: &axum::Router,
        cursor: Option<&str>,
        scopes: &[&str],
        jti: &str,
    ) -> axum::response::Response {
        let mut req = Request::builder()
            .uri("/v1/ledger")
            .header("Authorization", format!("Bearer {}", self.jwt(scopes, jti)));
        if let Some(c) = cursor {
            req = req.header("Last-Event-ID", c);
        }
        app.clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
}

/// One parsed SSE event.
#[derive(Debug, Clone)]
struct Frame {
    id: String,
    event: String,
    data: Value,
}

/// Incremental reader of a `text/event-stream` body.
struct Sse {
    body: Body,
    buf: String,
}

impl Sse {
    fn new(resp: axum::response::Response) -> Self {
        assert_eq!(resp.status(), StatusCode::OK);
        Self {
            body: resp.into_body(),
            buf: String::new(),
        }
    }

    /// Next event, or `None` when the stream ends or stays silent for `wait`.
    async fn next(&mut self, wait: Duration) -> Option<Frame> {
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..end + 2).collect();
                let (mut id, mut event, mut data) = (String::new(), String::new(), String::new());
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("id:") {
                        id = v.trim().to_owned();
                    } else if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_owned();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push_str(v.trim_start());
                    }
                }
                if event.is_empty() {
                    continue; // keep-alive comment
                }
                return Some(Frame {
                    id,
                    event,
                    data: serde_json::from_str(&data).unwrap_or(Value::Null),
                });
            }
            match tokio::time::timeout(wait, self.body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(bytes) = frame.into_data() {
                        self.buf.push_str(&String::from_utf8_lossy(&bytes));
                    }
                }
                _ => return None,
            }
        }
    }

    /// The next `n` events that are not epoch or reset frames.
    async fn lines(&mut self, n: usize) -> Vec<Frame> {
        let mut out = Vec::new();
        while out.len() < n {
            let f = self
                .next(Duration::from_secs(5))
                .await
                .unwrap_or_else(|| panic!("stream ended after {} of {n} frames", out.len()));
            if !f.event.starts_with("ledger.") {
                out.push(f);
            }
        }
        out
    }
}

fn append(path: &Path, lines: &[String]) {
    use std::io::Write as _;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// Line `i`, alternating the writer that carries a `seq` with the file
/// store's unsequenced shape, so a `seq` cursor would lose half of them.
fn line(i: usize) -> String {
    if i % 2 == 0 {
        format!(
            r#"{{"seq":{i},"timestamp":"2026-10-06T10:00:{:02}Z","type":"molecule_step_completed","molecule_id":"m-{i}","step":{i},"total":99,"evidence":"e{i}"}}"#,
            i % 60
        )
    } else {
        format!(
            r#"{{"timestamp":"2026-10-06T10:00:{:02}Z","hash":"h{i}","kind":"molecule_evolved","molecule_id":"m-{i}","step":{i},"total":99}}"#,
            i % 60
        )
    }
}

fn molecule_ids(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .map(|f| f.data["molecule_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn resumes_after_line_k_and_delivers_the_rest_exactly_once_in_order() {
    let rig = Rig::new().await;
    let app = rig.app();
    let (n, k, m) = (8usize, 3usize, 4usize);
    append(&rig.ledger(), &(0..n).map(line).collect::<Vec<_>>());

    // Learn the cursor after line k from a first connection.
    let mut first = Sse::new(rig.open(&app, None, BOTH, "j1").await);
    let seen = first.lines(n).await;
    assert_eq!(
        molecule_ids(&seen),
        (0..n).map(|i| format!("m-{i}")).collect::<Vec<_>>()
    );
    let cursor = seen[k - 1].id.clone();
    drop(first);

    // Resume, then append while connected.
    let mut resumed = Sse::new(rig.open(&app, Some(&cursor), BOTH, "j2").await);
    append(&rig.ledger(), &(n..n + m).map(line).collect::<Vec<_>>());
    let got = resumed.lines(n + m - k).await;
    assert_eq!(
        molecule_ids(&got),
        (k..n + m).map(|i| format!("m-{i}")).collect::<Vec<_>>(),
        "exactly lines k+1..N+M, each once, in order"
    );
    assert!(
        resumed.next(Duration::from_millis(1500)).await.is_none(),
        "nothing further is delivered"
    );
}

#[tokio::test]
async fn resumes_across_an_adapter_restart() {
    let rig = Rig::new().await;
    append(&rig.ledger(), &(0..5).map(line).collect::<Vec<_>>());

    let before = rig.app();
    let mut s = Sse::new(rig.open(&before, None, BOTH, "j1").await);
    let seen = s.lines(5).await;
    let cursor = seen[2].id.clone();
    drop(s);
    drop(before);

    // New process state, same tenant directory; more lines arrived meanwhile.
    append(&rig.ledger(), &(5..7).map(line).collect::<Vec<_>>());
    let after = rig.app();
    let mut s = Sse::new(rig.open(&after, Some(&cursor), BOTH, "j2").await);
    assert_eq!(
        molecule_ids(&s.lines(4).await),
        ["m-3", "m-4", "m-5", "m-6"]
    );
}

#[tokio::test]
async fn a_replaced_ledger_produces_a_reset_and_replays_the_new_one() {
    let rig = Rig::new().await;
    let app = rig.app();
    append(&rig.ledger(), &[line(0), line(1)]);
    let mut s = Sse::new(rig.open(&app, None, BOTH, "j1").await);
    s.lines(2).await;

    std::fs::rename(rig.ledger(), rig.ledger().with_extension("jsonl.1")).unwrap();
    append(&rig.ledger(), &[line(10)]);

    let mut saw_reset = None;
    let mut replayed = None;
    while let Some(f) = s.next(Duration::from_secs(5)).await {
        if f.event == "ledger.reset" {
            saw_reset = Some(f);
        } else if f.event != "ledger.epoch" {
            replayed = Some(f);
            break;
        }
    }
    let reset = saw_reset.expect("a replaced file must be announced");
    assert_eq!(reset.data["reason"], "epoch_changed");
    assert_eq!(replayed.unwrap().data["molecule_id"], "m-10");
}

#[tokio::test]
async fn a_cursor_naming_another_log_is_refused_loudly_not_followed() {
    let rig = Rig::new().await;
    let app = rig.app();
    append(&rig.ledger(), &[line(0)]);
    let mut s = Sse::new(rig.open(&app, Some("deadbeef0000.42"), BOTH, "j1").await);
    let first = s.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(first.event, "ledger.epoch");
    let second = s.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(second.event, "ledger.reset");
    assert_eq!(second.data["reason"], "epoch_changed");
    assert_eq!(
        molecule_ids(&s.lines(1).await),
        ["m-0"],
        "replayed from the start"
    );
}

#[tokio::test]
async fn a_write_by_another_process_appears() {
    let rig = Rig::new().await;
    let app = rig.app();
    append(&rig.ledger(), &[line(0)]);
    let mut s = Sse::new(rig.open(&app, None, BOTH, "j1").await);
    s.lines(1).await;

    // The canonical writer, as a worker or `cs` invocation uses it: it never
    // goes through the adapter, so the in-process bus cannot see it.
    emit_one(
        rig.ledger(),
        EventV2::MoleculeCompleted {
            molecule_id: MoleculeId::new("task-20261006-aaaa").unwrap(),
            duration_ms: None,
            reason: "all steps completed".to_owned(),
            summary: Some("shipped".to_owned()),
        },
        None,
    )
    .unwrap();

    let f = s.lines(1).await.remove(0);
    assert_eq!(f.event, "molecule_completed");
    assert_eq!(f.data["molecule_id"], "task-20261006-aaaa");
    assert_eq!(f.data["summary"], "shipped");
    assert_eq!(f.data["schema_version"], 1);
    assert!(
        f.data["seq"].is_u64(),
        "a sequenced line keeps its seq as information"
    );
}

#[tokio::test]
async fn frames_carry_no_excluded_field() {
    let rig = Rig::new().await;
    let app = rig.app();
    append(
        &rig.ledger(),
        &[
            r#"{"seq":1,"type":"operator_present","operator":"x","molecule_id":"m-secret"}"#.to_owned(),
            r#"{"seq":2,"type":"operator_signed","molecule_id":"m-secret"}"#.to_owned(),
            r#"{"seq":3,"type":"worker_killed","worker_id":"w"}"#.to_owned(),
            r#"{"seq":4,"type":"future_event","molecule_id":"m-secret","payload":"p"}"#.to_owned(),
            r#"{"seq":5,"type":"worker_spawned","molecule":"m-1","worker_id":"w-1","adapter_name":"claude","worktree_path":"/home/u/wt","command":"rm -rf /","session_name":"s"}"#.to_owned(),
            r#"{"seq":6,"type":"session_presence","session_id":"5874d35a-0c3c-49a9-9465-8b6478601879","molecule_id":"m-1","role":"worker","state":"idle"}"#.to_owned(),
            line(2),
        ],
    );
    let mut s = Sse::new(rig.open(&app, None, BOTH, "j1").await);
    let frames = s.lines(3).await;
    assert_eq!(
        frames.iter().map(|f| f.event.as_str()).collect::<Vec<_>>(),
        [
            "worker_spawned",
            "session_presence",
            "molecule_step_completed"
        ]
    );
    let wire = serde_json::to_string(&frames.iter().map(|f| &f.data).collect::<Vec<_>>()).unwrap();
    for banned in [
        "m-secret",
        "operator",
        "worktree_path",
        "/home/u",
        "rm -rf",
        "session_name",
        "5874d35a",
        "session_id",
        "emitter",
    ] {
        assert!(!wire.contains(banned), "{banned} leaked: {wire}");
    }
    assert_eq!(frames[0].data["molecule_id"], "m-1");
    assert_eq!(frames[1].data["session_digest"].as_str().unwrap().len(), 16);
}

#[tokio::test]
async fn both_scopes_are_required() {
    let rig = Rig::new().await;
    let app = rig.app();
    for (scopes, jti) in [
        (&["cosmon:events:subscribe"][..], "only-events"),
        (&["cosmon:molecule:read"][..], "only-read"),
        (&["cosmon:logs:subscribe"][..], "neither"),
    ] {
        let resp = rig.open(&app, None, scopes, jti).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{scopes:?}");
    }
    let ok = rig.open(&app, None, BOTH, "both").await;
    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_principal_holds_one_ledger_stream_at_a_time() {
    let rig = Rig::new().await;
    let app = rig.app();
    let first = Sse::new(rig.open(&app, None, BOTH, "j1").await);
    let second = rig.open(&app, None, BOTH, "j2").await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(first);
    let third = rig.open(&app, None, BOTH, "j3").await;
    assert_eq!(
        third.status(),
        StatusCode::OK,
        "the slot is released with the body"
    );
}

#[tokio::test]
async fn a_connection_closes_cleanly_at_the_cap_and_resumes_without_loss() {
    let rig = Rig::new().await;
    let app = rig.app();
    let total = FRAME_CAP + 3;
    append(&rig.ledger(), &(0..total).map(line).collect::<Vec<_>>());

    let mut first = Sse::new(rig.open(&app, None, BOTH, "j1").await);
    let mut frames = Vec::new();
    while let Some(f) = first.next(Duration::from_secs(10)).await {
        if !f.event.starts_with("ledger.") {
            frames.push(f);
        }
    }
    assert_eq!(frames.len(), FRAME_CAP, "closes after the cap, then ends");
    let cursor = frames.last().unwrap().id.clone();
    drop(first);

    let mut rest = Sse::new(rig.open(&app, Some(&cursor), BOTH, "j2").await);
    let tail = rest.lines(3).await;
    assert_eq!(
        molecule_ids(&tail),
        (FRAME_CAP..total)
            .map(|i| format!("m-{i}"))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn an_absent_ledger_is_an_empty_stream_not_an_error() {
    let rig = Rig::new().await;
    let app = rig.app();
    let mut s = Sse::new(rig.open(&app, None, BOTH, "j1").await);
    assert_eq!(
        s.next(Duration::from_secs(5)).await.unwrap().event,
        "ledger.epoch"
    );
    append(&rig.ledger(), &[line(0)]);
    assert_eq!(molecule_ids(&s.lines(1).await), ["m-0"]);
}
