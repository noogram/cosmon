// SPDX-License-Identifier: AGPL-3.0-only

//! Integration tests against a mocked cosmon-rpp v1 adapter.
//!
//! These tests verify the wire-level contract of every endpoint the
//! CLI calls: request path, method, headers, body shape, and the
//! decoded response envelope. A failure here means the CLI has drifted
//! from the `OpenAPI` surface, not that the underlying server has a bug.
//!
//! The tests use `wiremock` (workspace v0.6); the matcher syntax is
//! identical to the harness used by `almanac-resolver` and `cosmon-provider`.

use std::collections::BTreeMap;

use cosmon_remote::client::{
    Client, CollapseRequest, ListFilters, NucleateRequest, ReasonRequest, TagRequest,
};
use cosmon_remote::config::Profile;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
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

#[tokio::test]
async fn healthz_returns_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "service": "rpp"})),
        )
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), None).unwrap();
    let body = client.healthz().await.unwrap();
    assert_eq!(body["ok"], json!(true));
}

#[tokio::test]
async fn mint_jwt_passes_sub_aud_scopes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(query_param("sub", "tenant-demo-operator"))
        .and(query_param("aud", "cosmon-rpp-tenant"))
        .and(query_param(
            "scopes",
            "cosmon:molecule:read cosmon:molecule:write",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"access_token": "test.jwt.value"})),
        )
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), None).unwrap();
    let minted = client
        .mint_jwt(&[
            "cosmon:molecule:read".into(),
            "cosmon:molecule:write".into(),
        ])
        .await
        .unwrap();
    assert_eq!(minted.access_token, "test.jwt.value");
}

#[tokio::test]
async fn nucleate_sends_bearer_and_decodes_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules"))
        .and(header("authorization", "Bearer fake-jwt"))
        .and(body_json(json!({
            "formula": "task-work",
            "variables": {"topic": "test"},
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "request_id": "req-1",
            "molecule": {
                "id": "task-20260522-zzzz",
                "kind": "task",
                "status": "pending",
            }
        })))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let mut vars = BTreeMap::new();
    vars.insert("topic".into(), "test".into());
    let env = client
        .nucleate(&NucleateRequest {
            formula: "task-work".into(),
            kind: None,
            variables: vars,
            tags: vec![],
        })
        .await
        .unwrap();
    assert_eq!(env.molecule.id, "task-20260522-zzzz");
    assert_eq!(env.molecule.status, "pending");
}

#[tokio::test]
async fn list_molecules_passes_filters_as_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules"))
        .and(query_param("status", "running"))
        .and(query_param("kind", "task"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-2",
            "ensemble": {
                "molecules": [
                    { "id": "task-1", "kind": "task", "status": "running" }
                ]
            }
        })))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let env = client
        .list_molecules(&ListFilters {
            status: Some("running".into()),
            kind: Some("task".into()),
            tag: None,
            fleet: None,
        })
        .await
        .unwrap();
    let mols = env.molecules();
    assert_eq!(mols.len(), 1);
    assert_eq!(mols[0].id, "task-1");
}

#[tokio::test]
async fn get_molecule_hits_right_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-3",
            "molecule": { "id": "task-1", "kind": "task", "status": "completed" }
        })))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let env = client.get_molecule("task-1").await.unwrap();
    assert_eq!(env.molecule.status, "completed");
}

#[tokio::test]
async fn tackle_decodes_t9_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-1/tackle"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "request_id": "req-4",
            "tackle": {
                "molecule_id": "task-1",
                "worker_session": "tmux-1",
                "spawned_at": "2026-05-22T10:00:00Z"
            }
        })))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let env = client.tackle("task-1").await.unwrap();
    assert_eq!(env.tackle.worker_session.as_deref(), Some("tmux-1"));
}

/// `run` dials the B2 bounded-drain route and decodes the 202
/// envelope — bounds are the server's read face, never sent by the
/// client (the request has no body).
#[tokio::test]
async fn run_decodes_bounded_drain_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-root/run"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "request_id": "req-run-1",
            "drain": {
                "root": "task-root",
                "status": "started",
                "bounds": {"budget": 128, "max_depth": 8, "max_molecules": 256},
                "timeout_secs": 3600,
                "started_at": "2026-06-11T10:00:00Z"
            }
        })))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let env = client.run("task-root").await.unwrap();
    assert_eq!(env.drain.root, "task-root");
    assert_eq!(env.drain.status, "started");
    assert_eq!(env.drain.bounds.budget, 128);
    assert_eq!(env.drain.bounds.max_depth, 8);
    assert_eq!(env.drain.bounds.max_molecules, 256);
}

/// Collapse and stuck post `{reason}` to their own routes; freeze and
/// thaw BOTH dial the fused freeze route with the mandatory `state`
/// discriminator (adapter fusion v1.0.0-rc).
///
/// The pre-A2 version of this test pinned the drifted behaviour
/// (`{reason}` alone, and a `POST …/thaw` the adapter had replaced
/// with a 410) — the M2 drift the tenant-CLI fusion closes. The pins
/// below are the canon truth the adapter actually accepts.
#[tokio::test]
async fn collapse_freeze_thaw_stuck_post_their_canon_bodies() {
    let server = MockServer::start().await;
    let reason = json!({"reason": "design_change"});
    for verb in ["collapse", "stuck"] {
        Mock::given(method("POST"))
            .and(path(format!("/v1/molecules/task-1/{verb}")))
            .and(body_json(reason.clone()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "verb": verb})),
            )
            .mount(&server)
            .await;
    }
    for state in ["frozen", "active"] {
        Mock::given(method("POST"))
            .and(path("/v1/molecules/task-1/freeze"))
            .and(body_json(
                json!({"state": state, "reason": "design_change"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "state": state})),
            )
            .mount(&server)
            .await;
    }

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let _ = client
        .collapse(
            "task-1",
            &CollapseRequest {
                reason: "design_change".into(),
                cause: None,
            },
        )
        .await
        .unwrap();
    let _ = client
        .freeze("task-1", Some("design_change"))
        .await
        .unwrap();
    let _ = client.thaw("task-1", Some("design_change")).await.unwrap();
    let _ = client
        .stuck(
            "task-1",
            &ReasonRequest {
                reason: "design_change".into(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn tag_posts_add_and_remove() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/molecules/task-1/tags"))
        .and(body_json(
            json!({"add": ["temp:hot"], "remove": ["temp:warm"]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let _ = client
        .tag(
            "task-1",
            &TagRequest {
                add: vec!["temp:hot".into()],
                remove: vec!["temp:warm".into()],
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn list_artifacts_decodes_manifest() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-1/artifacts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-5",
            "molecule_id": "task-1",
            "artifacts": [
                {
                    "name": "haiku.txt",
                    "content_type": "text/plain",
                    "size_bytes": 42,
                    "integrity": {"algo": "blake3", "hex": "deadbeef"},
                    "created_at": "2026-05-22T11:00:00Z",
                    "token": "art_01234567890123456789ABCD"
                }
            ]
        })))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let m = client.list_artifacts("task-1").await.unwrap();
    assert_eq!(m.artifacts.len(), 1);
    assert_eq!(m.artifacts[0].name, "haiku.txt");
    assert_eq!(m.artifacts[0].integrity.algo, "blake3");
}

#[tokio::test]
async fn fetch_artifact_writes_bytes_and_returns_metadata() {
    let server = MockServer::start().await;
    let payload = b"hello smithy";
    Mock::given(method("GET"))
        .and(path(
            "/v1/molecules/task-1/artifacts/art_01234567890123456789ABCD",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(payload.to_vec())
                .insert_header("Content-Type", "text/plain")
                .insert_header("ETag", "deadbeef"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let dest = tmp.path().join("nested").join("haiku.txt");
    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let fetched = client
        .fetch_artifact("task-1", "art_01234567890123456789ABCD", &dest)
        .await
        .unwrap();
    assert_eq!(fetched.bytes, payload.len() as u64);
    assert_eq!(fetched.content_type.as_deref(), Some("text/plain"));
    assert_eq!(fetched.etag.as_deref(), Some("deadbeef"));
    assert_eq!(
        std::fs::read(&fetched.dest).unwrap(),
        payload,
        "file contents should match server response"
    );
}

/// On `GET /v1/molecules/{id}/artifacts/{token}` the bearer must be the
/// JWT — the artifact token's only place is the path segment. The
/// echo-server capture showed `Authorization: Bearer tok-Y` instead.
/// The `header` matcher makes the assertion wire-level: a wrong bearer
/// means no mock matches and the call errors.
#[tokio::test]
async fn fetch_artifact_sends_jwt_bearer_and_artifact_token_in_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/task-X/artifacts/tok-Y"))
        .and(header("authorization", "Bearer AAA.BBB.CCC"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"payload".to_vec())
                .insert_header("Content-Type", "text/plain"),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let dest = tmp.path().join("payload.txt");
    let client = Client::new(&profile_for(&server), Some("AAA.BBB.CCC".into())).unwrap();
    let fetched = client
        .fetch_artifact("task-X", "tok-Y", &dest)
        .await
        .unwrap();
    assert_eq!(fetched.bytes, 7);
}

#[tokio::test]
async fn push_artifact_sends_digest_and_content_type() {
    use blake3::Hasher;
    let server = MockServer::start().await;
    let payload = b"haiku\n";
    let mut hasher = Hasher::new();
    hasher.update(payload);
    let hex = hasher.finalize().to_hex().to_string();

    Mock::given(method("PUT"))
        .and(path("/v1/molecules/task-1/artifacts/haiku.txt"))
        .and(header("content-type", "text/plain"))
        .and(header("digest", format!("blake3={hex}").as_str()))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "request_id": "req-6",
            "artifact": {
                "name": "haiku.txt",
                "content_type": "text/plain",
                "size_bytes": payload.len() as u64,
                "integrity": {"algo": "blake3", "hex": hex},
                "created_at": "2026-05-22T12:00:00Z",
                "token": "art_aaaaaaaaaaaaaaaaaaaaaaaa"
            }
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::TempDir::new().unwrap();
    let file = tmp.path().join("haiku.txt");
    std::fs::write(&file, payload).unwrap();

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let env = client
        .push_artifact("task-1", "haiku.txt", &file, Some("text/plain"), None)
        .await
        .unwrap();
    assert_eq!(env.artifact.size_bytes, payload.len() as u64);
}

#[tokio::test]
async fn auth_claude_flow_walks_start_email_confirm() {
    let server = MockServer::start().await;
    // Fixture mirrors the REAL `/start` wire shape (routes.rs): the
    // deadline field is `ttl_at`; `expires_at` only exists from `/email`
    // onward (PKCE deadline). A fixture carrying `expires_at` here is
    // exactly how replay-Dave D1 slipped through (task-20260610-828e):
    // the parser was green against a response the server never sends.
    // The authoritative net is the real-route snapshot test in
    // cosmon-rpp-adapter/tests/auth_claude_integration.rs.
    Mock::given(method("POST"))
        .and(path("/v1/auth/claude/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "session_id": "sess-1",
            "state": "AWAITING_EMAIL",
            "created_at": "2026-05-22T12:45:00Z",
            "ttl_at": "2026-05-22T13:00:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/claude/email"))
        .and(body_json(
            json!({"session_id": "sess-1", "email": "op@example.invalid"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "verification_url": "https://claude.com/cai/oauth/authorize?code=true&…",
            "state": "AWAITING_USER_APPROVAL",
            "expires_at": "2026-05-22T13:15:00Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/claude/confirm"))
        .and(body_json(json!({
            "session_id": "sess-1",
            "authorization_code": "AUTH_CODE#state"
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": true, "state": "COMPLETED"})),
        )
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let start = client.auth_start().await.unwrap();
    assert_eq!(start.session_id, "sess-1");
    assert_eq!(start.ttl_at, "2026-05-22T13:00:00Z");
    let email_resp = client
        .auth_email(&start.session_id, "op@example.invalid")
        .await
        .unwrap();
    assert!(email_resp.verification_url.contains("oauth/authorize"));
    let confirm = client
        .auth_confirm(&start.session_id, "AUTH_CODE#state")
        .await
        .unwrap();
    assert_eq!(confirm["state"], json!("COMPLETED"));
}

#[tokio::test]
async fn events_stream_parses_sse_chunks() {
    use std::sync::{Arc, Mutex};

    let server = MockServer::start().await;
    // Two real SSE events plus a keep-alive comment and a malformed
    // line — the parser must ignore comments and emit two events.
    let body = "id: 1\nevent: molecule.state_changed\ndata: {\"molecule_id\":\"task-a\",\"new_state\":\"active\"}\n\n: keep-alive\n\nid: 2\nevent: molecule.event_appended\ndata: {\"molecule_id\":\"task-a\",\"event\":{\"kind\":\"tag\"}}\n\n";
    Mock::given(method("GET"))
        .and(path("/v1/events"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let collected: Arc<Mutex<Vec<cosmon_remote::client::SseEvent>>> =
        Arc::new(Mutex::new(Vec::new()));
    let coll_clone = collected.clone();
    client
        .events_stream(None, None, move |evt| {
            coll_clone.lock().unwrap().push(evt);
        })
        .await
        .unwrap();

    let collected = collected.lock().unwrap();
    assert_eq!(collected.len(), 2, "expected 2 events, got {collected:?}");
    assert_eq!(collected[0].id.as_deref(), Some("1"));
    assert_eq!(collected[0].event, "molecule.state_changed");
    assert_eq!(
        collected[0].data_obj().unwrap()["new_state"],
        json!("active")
    );
    assert_eq!(collected[1].id.as_deref(), Some("2"));
    assert_eq!(collected[1].event, "molecule.event_appended");
}

#[tokio::test]
async fn events_stream_rejects_missing_scope_with_403() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/events"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": "forbidden"})))
        .mount(&server)
        .await;
    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let err = client
        .events_stream(None, None, |_| panic!("no event expected on 403"))
        .await
        .unwrap_err();
    match err {
        cosmon_remote::error::Error::Api { status, .. } => assert_eq!(status, 403),
        other => panic!("expected Api 403, got {other:?}"),
    }
}

#[tokio::test]
async fn converse_posts_canon_body_and_decodes_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/avatar/converse"))
        .and(header("authorization", "Bearer fake-jwt"))
        .and(body_json(json!({
            "avatar_id": "ava-1",
            "message": "bonjour",
            "kind": "request",
            "hop": 2,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-conv-1",
            "converse": {"message_id": "msg-req-conv-1", "accepted": true},
        })))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake-jwt".into())).unwrap();
    let env = client
        .converse("ava-1", &json!("bonjour"), "request", 2)
        .await
        .unwrap();
    assert_eq!(env["converse"]["accepted"], json!(true));
    assert_eq!(env["converse"]["message_id"], json!("msg-req-conv-1"));
}

#[tokio::test]
async fn converse_surfaces_the_stable_refusal_codes() {
    // Off-binding (503 no_binding) and hop-bound (409
    // max_hops_exceeded) both surface as Api errors carrying the
    // stable label — the CLI never remaps them.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/avatar/converse"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error": "no_binding"})))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let err = client
        .converse("ava-unbound", &json!("hi"), "request", 0)
        .await
        .unwrap_err();
    match err {
        cosmon_remote::error::Error::Api { status, body } => {
            assert_eq!(status, 503);
            assert_eq!(body["error"], json!("no_binding"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[tokio::test]
async fn api_error_carries_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/ghost"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not_found"})))
        .mount(&server)
        .await;

    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    let err = client.get_molecule("ghost").await.unwrap_err();
    match err {
        cosmon_remote::error::Error::Api { status, body } => {
            assert_eq!(status, 404);
            assert_eq!(body["error"], json!("not_found"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

/// Gate "opt-out effectif" at the wire boundary: with a pending
/// phone-home report spooled, an enabled profile
/// lets the pair ride the next request as `X-Cosmon-Phone-Home`; after
/// the `config set phone-home off` gesture, the header never leaves
/// the machine — even with reports still spooled. One test fn because
/// the spool dir override is a process-global env var.
#[tokio::test]
async fn phone_home_header_rides_when_enabled_and_stops_on_opt_out() {
    use cosmon_remote::phone_home;

    let spool = tempfile::TempDir::new().unwrap();
    // SAFETY: single-threaded with respect to this env var — the only
    // test in the binary that touches it.
    unsafe {
        std::env::set_var(phone_home::ENV_DIR, spool.path());
    }

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/molecules/mol-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "request_id": "req-ok",
            "molecule": {"id": "mol-1", "kind": "task", "status": "pending"},
        })))
        .mount(&server)
        .await;

    let host = server.uri().trim_end_matches('/').to_owned();
    let queue_report = || {
        let _ = phone_home::on_failure(
            spool.path(),
            &host,
            true,
            "cosmon-remote",
            &cosmon_remote::error::Error::Api {
                status: 503,
                body: json!({"error": "tackle_unavailable", "request_id": "req-fail"}),
            },
            chrono::Utc::now(),
        );
    };

    // Enabled: the queued pair rides the next request.
    queue_report();
    let client = Client::new(&profile_for(&server), Some("fake".into())).unwrap();
    client.get_molecule("mol-1").await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let carried = requests
        .last()
        .unwrap()
        .headers
        .get("x-cosmon-phone-home")
        .expect("enabled profile must carry the pending report");
    assert_eq!(carried.to_str().unwrap(), "req-fail:503_tackle_unavailable");

    // The gesture: phone-home off. Re-queue a report directly in the
    // spool (simulating a pre-gesture leftover), then call again — the
    // header must NOT ride.
    queue_report();
    let mut off_profile = profile_for(&server);
    off_profile.set("phone-home", "off".into()).unwrap();
    assert!(!off_profile.phone_home);
    let client_off = Client::new(&off_profile, Some("fake".into())).unwrap();
    client_off.get_molecule("mol-1").await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .last()
            .unwrap()
            .headers
            .get("x-cosmon-phone-home")
            .is_none(),
        "opt-out profile must never carry the header"
    );

    unsafe {
        std::env::remove_var(phone_home::ENV_DIR);
    }
}
