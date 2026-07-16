// SPDX-License-Identifier: AGPL-3.0-only

//! E2E tests of the `do` composition against a mocked adapter.
//!
//! The two gates pinned here:
//!
//! 1. **A `do` produces a recoverable result end-to-end** — nucleate →
//!    guard → tackle → follow-to-completed, then `result <id>` hands
//!    back the deliverable bytes.
//! 2. **The credit guard displays BEFORE the first spend** — when the
//!    operator declines (or stdin is closed), the tackle route is hit
//!    ZERO times (wiremock `expect(0)` verifies on drop), while the
//!    free nucleate already happened.

use std::collections::BTreeMap;
use std::time::Duration;

use cosmon_remote::client::Client;
use cosmon_remote::config::Profile;
use cosmon_remote::do_flow::{
    run_do, DoOptions, EphemeralGuardMemory, GuardMemory, CREDIT_GUARD_PROMPT,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// Mount the happy-path molecule lifecycle: nucleate → tackle →
/// observe (running once, then completed) → result.
async fn mount_happy_path(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/molecules"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "request_id": "req-do-1",
            "molecule": {"id": "task-do-0001", "kind": "task", "status": "pending"},
        })))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-do-0001/tackle"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "request_id": "req-do-2",
            "tackle": {
                "molecule_id": "task-do-0001",
                "worker_session": "tmux-do",
                "spawned_at": "2026-06-11T10:00:00Z",
            },
        })))
        .expect(1)
        .mount(server)
        .await;
    // First observe sees the worker running; every later one sees the
    // terminal state. Mount order matters: the bounded mock wins while
    // it has uses left.
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-do-0001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-do-3",
            "molecule": {"id": "task-do-0001", "kind": "task", "status": "running"},
        })))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-do-0001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-do-4",
            "molecule": {"id": "task-do-0001", "kind": "task", "status": "completed"},
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-do-0001/result"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-do-5",
            "molecule_id": "task-do-0001",
            "status": "completed",
            "result": {
                "source": "result.md",
                "content_type": "text/markdown",
                "encoding": "utf8",
                "content": "silicon morning —\nthe drain hums through the trellis\nresults ripen, picked",
                "size_bytes": 84,
                "integrity": {"algo": "blake3", "hex": "deadbeef"},
            },
        })))
        .mount(server)
        .await;
}

/// Gate 1 — a `do` produces a recoverable result end-to-end: the
/// composition reaches `completed`, and the canonical deliverable is
/// then fetchable through `result`.
#[tokio::test]
async fn do_produces_recoverable_result_end_to_end() {
    let server = MockServer::start().await;
    mount_happy_path(&server).await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut memory = EphemeralGuardMemory::default();
    let mut lines = Vec::new();

    let outcome = run_do(
        &client,
        fast_opts(),
        &mut memory,
        |_prompt| Ok(true),
        |line| lines.push(line.to_owned()),
    )
    .await
    .unwrap();

    assert_eq!(outcome.molecule_id, "task-do-0001");
    assert_eq!(outcome.terminal_status.as_deref(), Some("completed"));
    assert!(outcome.guard_shown, "first run must show the guard");
    assert!(
        lines.iter().any(|l| l.contains("running → completed")),
        "follow loop must report the transition, got {lines:?}",
    );

    // The deliverable is recoverable — the `result` gesture of the
    // golden path (login → do → result).
    let result = client.get_result(&outcome.molecule_id).await.unwrap();
    assert_eq!(result.status, "completed");
    let body = result.result.expect("a completed deliverable is present");
    assert!(body.content.contains("silicon morning"));
}

/// Gate 2 — the guard displays before the FIRST spend: a declined
/// guard leaves the tackle route untouched (`expect(0)`), while the
/// free nucleate already happened (`expect(1)`).
#[tokio::test]
async fn declined_guard_blocks_before_first_spend() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "request_id": "req-guard-1",
            "molecule": {"id": "task-do-0002", "kind": "task", "status": "pending"},
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-do-0002/tackle"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut memory = EphemeralGuardMemory::default();
    let mut prompts = Vec::new();

    let err = run_do(
        &client,
        fast_opts(),
        &mut memory,
        |prompt| {
            prompts.push(prompt.to_owned());
            Ok(false)
        },
        |_| {},
    )
    .await
    .unwrap_err();

    assert_eq!(prompts, vec![CREDIT_GUARD_PROMPT.to_owned()]);
    let msg = err.to_string();
    assert!(
        msg.contains("credit guard declined"),
        "decline must be named, got: {msg}",
    );
    assert!(
        msg.contains("molecule tackle task-do-0002"),
        "decline must name the manual dispatch gesture, got: {msg}",
    );
    assert!(
        !memory.acknowledged(),
        "a declined guard must not be remembered",
    );
    // wiremock verifies expect(0) on the tackle mock at drop.
}

/// An acknowledged memory skips the prompt entirely — asked once.
#[tokio::test]
async fn acknowledged_memory_skips_guard() {
    let server = MockServer::start().await;
    mount_happy_path(&server).await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut memory = EphemeralGuardMemory::default();
    memory.remember().unwrap();

    let outcome = run_do(
        &client,
        fast_opts(),
        &mut memory,
        |_prompt| panic!("guard must not prompt once acknowledged"),
        |_| {},
    )
    .await
    .unwrap();
    assert!(!outcome.guard_shown);
    assert_eq!(outcome.terminal_status.as_deref(), Some("completed"));
}

/// `--yes` skips the prompt for THIS run without persisting consent —
/// a script's yes is not the operator's.
#[tokio::test]
async fn assume_yes_skips_without_persisting() {
    let server = MockServer::start().await;
    mount_happy_path(&server).await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut memory = EphemeralGuardMemory::default();

    let outcome = run_do(
        &client,
        DoOptions {
            assume_yes: true,
            ..fast_opts()
        },
        &mut memory,
        |_prompt| panic!("--yes must bypass the prompt"),
        |_| {},
    )
    .await
    .unwrap();
    assert!(!outcome.guard_shown);
    assert!(
        !memory.acknowledged(),
        "--yes must not persist the acknowledgment",
    );
}

/// A confirmed interactive yes is persisted — the second `do` never
/// prompts again (the « mémorisable » clause of the gate).
#[tokio::test]
async fn confirmed_guard_is_remembered_for_the_next_do() {
    let server = MockServer::start().await;
    mount_happy_path(&server).await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut memory = EphemeralGuardMemory::default();

    let first = run_do(&client, fast_opts(), &mut memory, |_| Ok(true), |_| {})
        .await
        .unwrap();
    assert!(first.guard_shown);
    assert!(memory.acknowledged(), "confirmed yes must persist");

    // Second run: fresh mocks, same memory — no prompt.
    let server2 = MockServer::start().await;
    mount_happy_path(&server2).await;
    let client2 = Client::new(&profile_for(&server2), Some("fake-jwt".into())).unwrap();
    let second = run_do(
        &client2,
        fast_opts(),
        &mut memory,
        |_prompt| panic!("second do must not prompt"),
        |_| {},
    )
    .await
    .unwrap();
    assert!(!second.guard_shown);
}
