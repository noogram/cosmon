// SPDX-License-Identifier: AGPL-3.0-only

//! The host allowlist remains authoritative across JWKS reloads.

use cosmon_oidc_testkit::{TEST_RSA_E_B64URL, TEST_RSA_N_B64URL, TEST_RSA_PRIVATE_PEM};
use cosmon_rpp_adapter::{
    JwksFetcher, JwksProvider, JwksStore, JwtVerifier, Posture, RppRejectReason, SharedJwksStore,
    TrustedIssuers,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const A: &str = "https://issuer-a.example";
const B: &str = "https://issuer-b.example";
const AUD: &str = "cosmon-rpp-shared";

fn write_allowlist(root: &Path, issuers: &[&str]) {
    let body = issuers
        .iter()
        .map(|iss| format!("[[issuer]]\niss = {iss:?}\njwks_uri = \"http://127.0.0.1:9/jwks\"\naudiences = [{AUD:?}]\n"))
        .collect::<String>();
    std::fs::write(root.join("security/trusted-issuers.toml"), body).unwrap();
}

fn token(issuer: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("k".into());
    jsonwebtoken::encode(
        &header,
        &json!({"iss": issuer, "sub": "alice", "aud": AUD, "iat": now, "exp": now + 300, "jti": "reload-test"}),
        &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes()).unwrap(),
    )
    .unwrap()
}

fn write_staged_key(root: &Path, issuer: &str, name: &str) {
    std::fs::write(
        root.join("security/jwks").join(name),
        json!({"iss": issuer, "audiences": [AUD], "keys": [{"kid": "k", "alg": "RS256", "kty": "RSA", "n": TEST_RSA_N_B64URL, "e": TEST_RSA_E_B64URL}]}).to_string(),
    )
    .unwrap();
}

#[test]
fn trusted_issuer_set_is_identical_at_boot_and_after_reload() {
    let td = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(td.path().join("security/jwks")).unwrap();
    write_allowlist(td.path(), &[A]);
    write_staged_key(td.path(), A, "current.json");
    write_staged_key(td.path(), B, "old.json");

    // Exercise the same file admission rule as the HTTP-fetch boot path,
    // without making a network request.
    let trusted = TrustedIssuers::load(td.path()).unwrap();
    let configured = HashSet::from([A.to_owned()]);
    let staged = JwksStore::load_with_allowlist(td.path(), Some(&configured)).unwrap();
    let provider = JwksProvider::new(
        SharedJwksStore::new(staged),
        trusted.issuers,
        JwksFetcher::new().unwrap(),
    );
    let shared = provider.shared();
    let a = token(A);
    let b = token(B);
    assert!(JwtVerifier::validate(&shared.load(), &a, Posture::Active).is_ok());
    assert!(matches!(
        JwtVerifier::validate(&shared.load(), &b, Posture::Active),
        Err(RppRejectReason::IssuerNotPinned)
    ));

    assert!(cosmon_rpp_adapter::reload::reload_jwks(&shared, td.path()).is_ok());
    assert!(matches!(
        JwtVerifier::validate(&shared.load(), &b, Posture::Active),
        Err(RppRejectReason::IssuerNotPinned)
    ));

    // Removing an issuer from the file must revoke it even when its staged
    // key remains on disk and later reloads read that directory again.
    write_allowlist(td.path(), &[A, B]);
    assert!(cosmon_rpp_adapter::reload::reload_jwks(&shared, td.path()).is_ok());
    assert!(JwtVerifier::validate(&shared.load(), &b, Posture::Active).is_ok());
    write_allowlist(td.path(), &[A]);
    for _ in 0..2 {
        assert!(cosmon_rpp_adapter::reload::reload_jwks(&shared, td.path()).is_ok());
        assert!(matches!(
            JwtVerifier::validate(&shared.load(), &b, Posture::Active),
            Err(RppRejectReason::IssuerNotPinned)
        ));
    }

    // An existing but empty allowlist is still authoritative.
    write_allowlist(td.path(), &[]);
    assert!(cosmon_rpp_adapter::reload::reload_jwks(&shared, td.path()).is_ok());
    assert!(matches!(
        JwtVerifier::validate(&shared.load(), &b, Posture::Active),
        Err(RppRejectReason::IssuerNotPinned)
    ));
}
