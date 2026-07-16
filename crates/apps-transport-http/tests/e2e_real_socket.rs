// SPDX-License-Identifier: Apache-2.0

//! Bind on a real loopback socket and verify the wire shape with a
//! plain `reqwest` client. This is the cross-language seam: the Swift
//! side has its own `XCTest` mirror in
//! `apps/AppsTransportHTTP/Tests/AppsTransportHTTPTests/`, both
//! consume the canonical `{ok: true}` body and the canonical
//! `{error, code, detail}` error shape.

use apps_transport_http::{access_log_layer, request_id_layer, routing::v1, ApplicationError};
use axum::middleware::from_fn;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

#[tokio::test]
async fn binds_and_serves_v1_health_on_loopback() {
    // `serve_http_on_tailscale` would need port advertisement; for a
    // simple round-trip we skip the helper and use axum::serve directly
    // with a port-0 listener. The bind helper itself is unit-tested in
    // `bind.rs` and the dispatch in `tests/e2e.rs`.
    let app = Router::new()
        .route(
            v1::HEALTH,
            get(|| async { Json(json!({"ok": true, "service": "e2e"})) }),
        )
        .route(
            "/v1/error",
            get(|| async {
                Err::<&'static str, _>(ApplicationError::BadRequest("synthetic".into()))
            }),
        )
        .layer(from_fn(access_log_layer))
        .layer(from_fn(request_id_layer));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
    // Give the server a beat to enter accept().
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();

    // Healthy round-trip.
    let url = format!("http://{addr}/v1/health");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["service"], "e2e");

    // Canonical error shape.
    let url = format!("http://{addr}/v1/error");
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "bad_request");
    assert_eq!(body["detail"], "synthetic");

    server.abort();
}
