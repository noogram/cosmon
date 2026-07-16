// SPDX-License-Identifier: AGPL-3.0-only

//! E2E tests of the `run` composition (`do` + attributed cost) against a
//! mocked adapter.
//!
//! Three gates pinned here:
//!
//! 1. **`run` prices a successful run** — the same happy path the `do`
//!    tests exercise, plus two `GET /v1/quota` reads that bracket the
//!    flow; the reported [`CostDelta`] reflects the before/after pair.
//! 2. **The bracket is best-effort** — when the adapter has no quota
//!    surface (404 on `/v1/quota`), the work still completes and the
//!    cost degrades to `None` rather than failing the run.
//! 3. **The quota read precedes the first spend** — the BEFORE snapshot
//!    is taken before the tackle, so it captures the pre-charge level.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cosmon_remote::client::Client;
use cosmon_remote::config::Profile;
use cosmon_remote::cost::run_with_cost;
use cosmon_remote::do_flow::{DoOptions, EphemeralGuardMemory};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

fn profile_for(server: &MockServer) -> Profile {
    Profile {
        host: server.uri(),
        sub: "tenant-demo-operator".into(),
        aud: "cosmon-rpp-tenant".into(),
        oidc_url: server.uri(),
        issuer: None,
        client_id: None,
        noyau: Some("default".into()),
        scopes: vec![
            "cosmon:molecule:read".into(),
            "cosmon:molecule:write".into(),
        ],
        artifacts_dir: None,
        timeout_secs: 5,
        phone_home: true,
    }
}

fn fast_opts() -> DoOptions {
    DoOptions {
        variables: BTreeMap::from([("topic".to_owned(), "write a haiku".to_owned())]),
        poll_interval: Duration::from_millis(10),
        poll_timeout: Duration::from_secs(5),
        follow_events: false,
        ..DoOptions::default()
    }
}

/// Mount nucleate → tackle → observe(running, then completed). No quota,
/// no result — those are mounted per-test.
async fn mount_lifecycle(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/molecules"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "request_id": "req-run-1",
            "molecule": {"id": "task-run-0001", "kind": "task", "status": "pending"},
        })))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-run-0001/tackle"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "request_id": "req-run-2",
            "tackle": {
                "molecule_id": "task-run-0001",
                "worker_session": "tmux-run",
                "spawned_at": "2026-06-25T10:00:00Z",
            },
        })))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-run-0001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-run-3",
            "molecule": {"id": "task-run-0001", "kind": "task", "status": "running"},
        })))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-run-0001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-run-4",
            "molecule": {"id": "task-run-0001", "kind": "task", "status": "completed"},
        })))
        .mount(server)
        .await;
}

/// A quota responder that returns a rising bucket level on each call:
/// first read (BEFORE) is low, every later read (AFTER) is higher — so a
/// passing test proves the two snapshots are distinct calls and the
/// delta is non-trivial.
struct RisingQuota {
    calls: Arc<AtomicU64>,
}

impl Respond for RisingQuota {
    fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        // BEFORE: level 4 / remaining 26. AFTER: level 6 / remaining 24.
        // `floor` is carried as an explicit integer (no f64→i64 cast).
        let (level, floor, remaining) = if n == 0 { (4.0, 4, 26) } else { (6.0, 6, 24) };
        ResponseTemplate::new(200).set_body_json(json!({
            "request_id": format!("req-quota-{n}"),
            "limits": {"burst_capacity": 30, "leak_per_minute": 10.0, "leak_per_hour": 600.0},
            "current": {"bucket_level": level, "bucket_level_floor": floor},
            "remaining": remaining,
            "reset_at": "2026-06-25T16:00:00Z",
        }))
    }
}

/// Gate 1 — `run` prices a successful run: the flow reaches `completed`
/// and the cost delta reflects the bracketing quota snapshots.
#[tokio::test]
async fn run_prices_a_successful_run() {
    let server = MockServer::start().await;
    mount_lifecycle(&server).await;
    let calls = Arc::new(AtomicU64::new(0));
    Mock::given(method("GET"))
        .and(path("/v1/quota"))
        .respond_with(RisingQuota {
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut memory = EphemeralGuardMemory::default();

    let outcome = run_with_cost(&client, fast_opts(), &mut memory, |_| Ok(true), |_| {})
        .await
        .unwrap();

    assert_eq!(outcome.do_outcome.molecule_id, "task-run-0001");
    assert_eq!(
        outcome.do_outcome.terminal_status.as_deref(),
        Some("completed")
    );
    let cost = outcome
        .cost
        .expect("both quota snapshots present → cost is computed");
    assert!((cost.before_level - 4.0).abs() < 1e-9);
    assert!((cost.after_level - 6.0).abs() < 1e-9);
    assert!((cost.level_delta - 2.0).abs() < 1e-9);
    assert_eq!(cost.remaining_delta, -2);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "exactly two quota reads bracket the run",
    );
}

/// Gate 2 — the bracket is best-effort: a 404 on `/v1/quota` degrades
/// the cost to `None` but the work still completes.
#[tokio::test]
async fn missing_quota_surface_degrades_cost_not_the_run() {
    let server = MockServer::start().await;
    mount_lifecycle(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/quota"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not_found"})))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut memory = EphemeralGuardMemory::default();

    let outcome = run_with_cost(&client, fast_opts(), &mut memory, |_| Ok(true), |_| {})
        .await
        .unwrap();

    assert_eq!(
        outcome.do_outcome.terminal_status.as_deref(),
        Some("completed"),
        "the work completes even when pricing is unavailable",
    );
    assert!(
        outcome.cost.is_none(),
        "a failed quota snapshot degrades cost to None",
    );
}
