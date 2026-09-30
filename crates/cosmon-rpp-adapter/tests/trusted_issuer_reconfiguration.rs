// SPDX-License-Identifier: AGPL-3.0-only

//! HTTP issuer configuration takes effect on reload and stays effective on refresh.

use cosmon_oidc_testkit::{TEST_RSA_E_B64URL, TEST_RSA_N_B64URL, TEST_RSA_PRIVATE_PEM};
use cosmon_rpp_adapter::{
    JwksFetcher, JwksProvider, JwksStore, JwtVerifier, Posture, SharedJwksStore, TrustedIssuers,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const A: &str = "https://issuer-a.example";
const B: &str = "https://issuer-b.example";

async fn serve_keys(kid: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}/keys", listener.local_addr().unwrap());
    let body = json!({"keys": [{"kid": kid, "alg": "RS256", "kty": "RSA", "n": TEST_RSA_N_B64URL, "e": TEST_RSA_E_B64URL}]}).to_string();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = [0_u8; 1024];
            if socket.read(&mut request).await.is_err() {
                continue;
            }
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (address, task)
}

fn write_issuers(root: &Path, entries: &[(&str, &str, &[&str])]) {
    let body = entries
        .iter()
        .map(|(iss, uri, audiences)| {
            format!("[[issuer]]\niss = {iss:?}\njwks_uri = {uri:?}\naudiences = {audiences:?}\n")
        })
        .collect::<String>();
    std::fs::write(root.join("security/trusted-issuers.toml"), body).unwrap();
}

fn token(issuer: &str, audience: &str, kid: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    jsonwebtoken::encode(
        &header,
        &json!({"iss": issuer, "sub": "alice", "aud": audience, "iat": now, "exp": now + 300, "jti": "reload-config"}),
        &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes()).unwrap(),
    ).unwrap()
}

#[tokio::test]
async fn first_http_issuer_added_after_file_stage_boot_is_fetched_on_reload() {
    let td = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(td.path().join("security/jwks")).unwrap();
    let staged = serde_json::json!({
        "iss": A,
        "audiences": ["aud-file"],
        "keys": [{"kid": "file-key", "alg": "RS256", "kty": "RSA", "n": TEST_RSA_N_B64URL, "e": TEST_RSA_E_B64URL}]
    });
    std::fs::write(
        td.path().join("security/jwks/file.json"),
        staged.to_string(),
    )
    .unwrap();
    let shared = SharedJwksStore::new(JwksStore::load(td.path()).unwrap());
    let provider = JwksProvider::for_reload(shared.clone(), JwksFetcher::new().unwrap());
    assert!(shared.load().contains_kid(A, "file-key"));

    let (uri, server) = serve_keys("http-key").await;
    write_issuers(td.path(), &[(B, &uri, &["aud-http"])]);
    let result =
        cosmon_rpp_adapter::reload::reload_jwks_and_fetch(&shared, &provider, td.path()).await;
    assert!(result.is_ok());
    assert_eq!(result.keys_after, 1);
    assert!(shared.load().contains_kid(B, "http-key"));
    assert!(!shared.load().contains_kid(A, "file-key"));
    assert!(JwtVerifier::validate(
        &shared.load(),
        &token(B, "aud-http", "http-key"),
        Posture::Active
    )
    .is_ok());
    server.abort();
}

#[tokio::test]
async fn http_issuer_edits_match_restart_after_reload_and_refresh() {
    let td = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(td.path().join("security")).unwrap();
    let (uri_a, task_a) = serve_keys("key-a").await;
    let (uri_b, task_b) = serve_keys("key-b").await;
    write_issuers(td.path(), &[(A, &uri_a, &["aud-x", "aud-y"])]);
    let trusted = TrustedIssuers::load(td.path()).unwrap();
    let provider = JwksProvider::new(
        SharedJwksStore::new(JwksStore::default()),
        &trusted.issuers,
        JwksFetcher::new().unwrap(),
    );
    provider.refresh_all().await;
    let shared = provider.shared();
    let old_audience = token(A, "aud-y", "key-a");
    assert!(JwtVerifier::validate(&shared.load(), &old_audience, Posture::Active).is_ok());

    write_issuers(
        td.path(),
        &[(A, &uri_a, &["aud-x", "aud-new"]), (B, &uri_b, &["aud-z"])],
    );
    assert!(cosmon_rpp_adapter::reload::reload_jwks(&shared, td.path()).is_ok());
    assert!(JwtVerifier::validate(&shared.load(), &old_audience, Posture::Active).is_err());
    let new_audience = token(A, "aud-new", "key-a");
    assert!(JwtVerifier::validate(&shared.load(), &new_audience, Posture::Active).is_ok());
    let added = token(B, "aud-z", "key-b");
    assert!(provider.ensure_kid(B, "key-b").await);
    assert!(JwtVerifier::validate(&shared.load(), &added, Posture::Active).is_ok());
    provider.refresh_all().await;
    assert!(JwtVerifier::validate(&shared.load(), &added, Posture::Active).is_ok());
    assert!(JwtVerifier::validate(&shared.load(), &old_audience, Posture::Active).is_err());

    task_a.abort();
    task_b.abort();
}

#[tokio::test]
async fn changed_http_key_location_takes_effect_after_reload() {
    let td = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(td.path().join("security")).unwrap();
    let (old_uri, old_task) = serve_keys("old-key").await;
    let (new_uri, new_task) = serve_keys("new-key").await;
    write_issuers(td.path(), &[(A, &old_uri, &["aud-x"])]);
    let trusted = TrustedIssuers::load(td.path()).unwrap();
    let provider = JwksProvider::new(
        SharedJwksStore::new(JwksStore::default()),
        &trusted.issuers,
        JwksFetcher::new().unwrap(),
    );
    provider.refresh_all().await;
    let shared = provider.shared();
    let old_token = token(A, "aud-x", "old-key");
    let new_token = token(A, "aud-x", "new-key");
    assert!(JwtVerifier::validate(&shared.load(), &old_token, Posture::Active).is_ok());

    write_issuers(td.path(), &[(A, &new_uri, &["aud-x"])]);
    assert!(cosmon_rpp_adapter::reload::reload_jwks(&shared, td.path()).is_ok());
    assert!(JwtVerifier::validate(&shared.load(), &old_token, Posture::Active).is_err());
    provider.refresh_all().await;
    assert!(JwtVerifier::validate(&shared.load(), &new_token, Posture::Active).is_ok());
    assert!(JwtVerifier::validate(&shared.load(), &old_token, Posture::Active).is_err());

    old_task.abort();
    new_task.abort();
}
