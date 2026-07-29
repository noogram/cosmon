// SPDX-License-Identifier: AGPL-3.0-only

//! What a browser can do with `cs-api`, observed over a real socket.
//!
//! These tests exist because the failure they guard is silent: a
//! wildcard `Access-Control-Allow-Origin` on a daemon whose
//! `POST /molecules/{id}/tackle` spawns a worker means any page the
//! operator opens can drive it, and nothing in the daemon's own
//! behaviour looks wrong while that is true.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cosmon_api::{router, router_with_cors, AppState, CorsPolicy};
use reqwest::header;

/// Any path is fine — the daemon never executes it here; only the
/// header behaviour is under test.
fn dummy_cs_path() -> PathBuf {
    PathBuf::from("/nonexistent/cs")
}

async fn serve(app: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    addr
}

async fn spawn(cors: CorsPolicy) -> SocketAddr {
    serve(router_with_cors(AppState::new(dummy_cs_path()), cors)).await
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

/// A stand-in `cs` binary that records the fact it ran, and nothing
/// else. Its existence on disk is the observable we assert on: the
/// response header is the property next door, the side effect is the
/// property under test.
fn tattletale_cs(dir: &Path, marker: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("cs");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{}'\necho '{{}}'\n",
            marker.to_string_lossy()
        ),
    )
    .expect("write stand-in cs");
    let mut perms = std::fs::metadata(&script)
        .expect("stat stand-in cs")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod stand-in cs");
    script
}

/// Post to the worker-spawning route the way a hostile page can without
/// a preflight: a *simple* request. `text/plain` and no custom header
/// is precisely the combination a browser sends first and judges after.
async fn simple_post_tackle(addr: SocketAddr, origin: Option<&str>) -> reqwest::StatusCode {
    let mut req = client()
        .post(format!("http://{addr}/molecules/task-20260727-0001/tackle"))
        .header(header::CONTENT_TYPE, "text/plain")
        .body("");
    if let Some(origin) = origin {
        req = req.header(header::ORIGIN, origin);
    }
    req.send().await.expect("send").status()
}

/// The finding this whole module now exists for: withholding
/// `Access-Control-Allow-Origin` from an unlisted origin does not stop
/// the write, because the write already happened. Asserted on the side
/// effect — a `cs` subprocess that leaves a file behind — not on the
/// header.
///
/// The second half is the control. Without it the first half would
/// stay green if `/molecules/{id}/tackle` had simply stopped spawning
/// anything at all, which is the failure mode this test is meant to be
/// immune to.
#[tokio::test]
async fn a_cross_origin_simple_post_never_reaches_the_worker_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("the-worker-was-spawned");
    let ndjson = dir.path().join("engine-calls.ndjson");
    let state =
        AppState::new(tattletale_cs(dir.path(), &marker)).with_instrumentation_path(ndjson.clone());
    let addr = serve(router_with_cors(
        state,
        CorsPolicy::from_allowed_origins(["http://localhost:5173"]).expect("policy"),
    ))
    .await;

    let status = simple_post_tackle(addr, Some("https://evil.test")).await;
    // The side effect first, deliberately. If the guard regresses, the
    // failure this test reports should be "a subprocess ran", not "the
    // status was 200" — the status is the property next door.
    assert!(
        !marker.exists(),
        "the handler ran: an unlisted origin spawned a `cs` subprocess"
    );
    assert!(
        !ndjson.exists(),
        "the handler ran: an engine call was recorded for an unlisted origin"
    );
    assert_eq!(
        status,
        reqwest::StatusCode::FORBIDDEN,
        "an unlisted origin must be refused, not served"
    );

    // Control: the same request without an `Origin` — a native client —
    // does reach the handler, so the assertions above have teeth.
    let status = simple_post_tackle(addr, None).await;
    assert!(
        status.is_success(),
        "a native client (no Origin) must still be served, got {status}"
    );
    assert!(
        marker.exists(),
        "control failed: this route no longer spawns anything, so the \
         cross-origin assertion above proves nothing"
    );
}

/// The same refusal under the default policy, where there is no
/// allow-list to miss. This is the configuration every operator runs,
/// and the one that previously had no middleware in the stack at all.
#[tokio::test]
async fn under_deny_a_cross_origin_simple_post_never_reaches_the_worker_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("the-worker-was-spawned");
    let state = AppState::new(tattletale_cs(dir.path(), &marker));
    let addr = serve(router_with_cors(state, CorsPolicy::Deny)).await;

    let status = simple_post_tackle(addr, Some("https://evil.test")).await;
    assert!(
        !marker.exists(),
        "under Deny the handler still ran for a cross-origin write"
    );
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);

    let status = simple_post_tackle(addr, None).await;
    assert!(status.is_success(), "got {status}");
    assert!(marker.exists(), "control failed: the route spawns nothing");
}

/// `router` — the constructor `main` used before this change and that
/// every test uses — must emit nothing a browser can act on.
#[tokio::test]
async fn default_router_emits_no_allow_origin() {
    let addr = serve(router(AppState::new(dummy_cs_path()))).await;

    let resp = client()
        .get(format!("http://{addr}/healthz"))
        .header(header::ORIGIN, "https://evil.test")
        .send()
        .await
        .expect("send");
    assert!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "default policy must not grant any browser origin"
    );
}

#[tokio::test]
async fn denied_origin_gets_no_allow_header_on_a_write_route() {
    let addr =
        spawn(CorsPolicy::from_allowed_origins(["http://localhost:5173"]).expect("policy")).await;
    let resp = client()
        .post(format!("http://{addr}/session/start"))
        .header(header::ORIGIN, "https://evil.test")
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("send");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the write route must refuse an unlisted origin outright"
    );
    assert!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "an unlisted origin must never be echoed back"
    );
}

#[tokio::test]
async fn preflight_from_an_unlisted_origin_is_refused() {
    let addr =
        spawn(CorsPolicy::from_allowed_origins(["http://localhost:5173"]).expect("policy")).await;
    let resp = client()
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{addr}/molecules/task-1/tackle"),
        )
        .header(header::ORIGIN, "https://evil.test")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(resp
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .is_none());
}

#[tokio::test]
async fn listed_origin_is_echoed_exactly_and_varies() {
    let addr =
        spawn(CorsPolicy::from_allowed_origins(["http://localhost:5173"]).expect("policy")).await;
    let resp = client()
        .request(
            reqwest::Method::OPTIONS,
            format!("http://{addr}/session/start"),
        )
        .header(header::ORIGIN, "http://localhost:5173")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .expect("listed origin is allowed"),
        "http://localhost:5173"
    );
    assert!(
        resp.headers().get(header::VARY).is_some(),
        "a cache must not serve one origin's response to another"
    );
}
