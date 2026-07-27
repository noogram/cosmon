// SPDX-License-Identifier: AGPL-3.0-only

//! What a browser can do with `cs-api`, observed over a real socket.
//!
//! These tests exist because the failure they guard is silent: a
//! wildcard `Access-Control-Allow-Origin` on a daemon whose
//! `POST /molecules/{id}/tackle` spawns a worker means any page the
//! operator opens can drive it, and nothing in the daemon's own
//! behaviour looks wrong while that is true.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use cosmon_api::{router, router_with_cors, AppState, CorsPolicy};
use reqwest::header;

/// Any path is fine — the daemon never executes it here; only the
/// header behaviour is under test.
fn dummy_cs_path() -> PathBuf {
    PathBuf::from("/nonexistent/cs")
}

async fn spawn(cors: CorsPolicy) -> SocketAddr {
    let app = router_with_cors(AppState::new(dummy_cs_path()), cors);
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

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

/// `router` — the constructor `main` used before this change and that
/// every test uses — must emit nothing a browser can act on.
#[tokio::test]
async fn default_router_emits_no_allow_origin() {
    let app = router(AppState::new(dummy_cs_path()));
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
