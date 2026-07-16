// SPDX-License-Identifier: AGPL-3.0-only

//! Wire-contract tests for the **remote** `cosmon-ops-tools` backend.
//!
//! Each test stands up a `wiremock` mock of the avatar's
//! `cosmon-rpp-adapter`, runs one remote tool's synchronous
//! [`cosmon_agent_harness::Tool::execute`], and asserts the §8p route, the
//! `Authorization: Bearer …` header, and the decoded body the model sees. A
//! failure here means the remote backend has drifted from the §8p surface
//! (ADR-080), the same guarantee `cosmon-remote`'s own `wire_contract.rs`
//! gives its client.
//!
//! ## Why `multi_thread` + `spawn_blocking`
//!
//! `Tool::execute` is synchronous and the remote backend blocks on an
//! isolated runtime inside it (`remote::run_blocking`). Calling that blocking
//! work directly on the test runtime's only worker would starve the mock
//! server. `spawn_blocking` moves it to the blocking pool so the runtime
//! keeps serving the mock — the canonical pattern for sync-in-async tests.

use std::path::PathBuf;

use cosmon_agent_harness::Tool;
use cosmon_ops_tools::{
    RemoteBackend, RemoteEnsembleTool, RemoteNucleateTool, RemoteObserveTool, RemoteTackleTool,
};
use cosmon_remote::Profile;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A backend pointed at the mock server, carrying a fixed bearer token so
/// the tests can assert it reaches the wire.
fn backend_for(server: &MockServer) -> RemoteBackend {
    RemoteBackend::new(
        Profile::from_host(server.uri()),
        Some("fake-jwt".to_owned()),
    )
}

/// The remote tools ignore `work_dir`; any path satisfies the signature.
fn nowhere() -> PathBuf {
    PathBuf::from(".")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_observe_hits_the_v1_route_with_bearer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-20260601-aaaa"))
        .and(header("authorization", "Bearer fake-jwt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-obs",
            "molecule": {
                "id": "task-20260601-aaaa",
                "kind": "task",
                "status": "running"
            }
        })))
        .mount(&server)
        .await;

    let backend = backend_for(&server);
    let raw = tokio::task::spawn_blocking(move || {
        RemoteObserveTool(backend)
            .execute(
                &json!({ "molecule_id": "task-20260601-aaaa" }).to_string(),
                &nowhere(),
            )
            .expect("remote observe must succeed")
    })
    .await
    .expect("blocking task joined");

    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    // The model sees the molecule projection, not the transport envelope.
    assert_eq!(parsed["id"], "task-20260601-aaaa");
    assert_eq!(parsed["status"], "running");
    assert!(parsed.get("request_id").is_none(), "envelope stripped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_ensemble_passes_filters_and_returns_the_index() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules"))
        .and(query_param("status", "running"))
        .and(query_param("tag", "temp:hot"))
        .and(header("authorization", "Bearer fake-jwt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-ens",
            "ensemble": {
                "molecules": [
                    { "id": "task-20260601-aaaa", "kind": "task", "status": "running" }
                ],
                "total": 1
            }
        })))
        .mount(&server)
        .await;

    let backend = backend_for(&server);
    let raw = tokio::task::spawn_blocking(move || {
        RemoteEnsembleTool(backend)
            .execute(
                &json!({ "status": "running", "tags": ["temp:hot"] }).to_string(),
                &nowhere(),
            )
            .expect("remote ensemble must succeed")
    })
    .await
    .expect("blocking task joined");

    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    assert_eq!(parsed["total"], 1);
    assert_eq!(parsed["molecules"][0]["id"], "task-20260601-aaaa");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_nucleate_posts_the_formula_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules"))
        .and(header("authorization", "Bearer fake-jwt"))
        .and(body_json(json!({
            "formula": "task-work",
            "variables": { "topic": "ship it" },
            "tags": ["temp:warm"]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "request_id": "req-nuc",
            "molecule": {
                "id": "task-20260601-bbbb",
                "kind": "task",
                "status": "pending"
            }
        })))
        .mount(&server)
        .await;

    let backend = backend_for(&server);
    let raw = tokio::task::spawn_blocking(move || {
        RemoteNucleateTool(backend)
            .execute(
                &json!({
                    "formula": "task-work",
                    "variables": { "topic": "ship it" },
                    "tags": ["temp:warm"]
                })
                .to_string(),
                &nowhere(),
            )
            .expect("remote nucleate must succeed")
    })
    .await
    .expect("blocking task joined");

    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    assert_eq!(parsed["id"], "task-20260601-bbbb");
    assert_eq!(parsed["status"], "pending");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_tackle_posts_to_the_tackle_route() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-20260601-bbbb/tackle"))
        .and(header("authorization", "Bearer fake-jwt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-tak",
            "tackle": {
                "molecule_id": "task-20260601-bbbb",
                "worker_session": "ruby",
                "spawned_at": "2026-06-01T12:00:00Z"
            }
        })))
        .mount(&server)
        .await;

    let backend = backend_for(&server);
    let raw = tokio::task::spawn_blocking(move || {
        RemoteTackleTool(backend)
            .execute(
                &json!({ "molecule_id": "task-20260601-bbbb" }).to_string(),
                &nowhere(),
            )
            .expect("remote tackle must succeed")
    })
    .await
    .expect("blocking task joined");

    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    assert_eq!(parsed["molecule_id"], "task-20260601-bbbb");
    assert_eq!(parsed["worker_session"], "ruby");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_observe_maps_404_to_io_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-20260601-zzzz"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": "molecule not found"
        })))
        .mount(&server)
        .await;

    let backend = backend_for(&server);
    let err = tokio::task::spawn_blocking(move || {
        RemoteObserveTool(backend)
            .execute(
                &json!({ "molecule_id": "task-20260601-zzzz" }).to_string(),
                &nowhere(),
            )
            .expect_err("absent molecule must fail")
    })
    .await
    .expect("blocking task joined");

    // A 404 is not the model's argument mistake — it is an Io-class failure
    // carrying the adapter's message (map_remote_err routes only 400/422 to
    // InvalidArguments).
    assert!(matches!(err, cosmon_agent_harness::ToolError::Io(_)));
}
