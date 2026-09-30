// SPDX-License-Identifier: AGPL-3.0-only

//! Token time bounds are checked against the adapter clock.

use std::time::{SystemTime, UNIX_EPOCH};

use cosmon_oidc_testkit::{TEST_RSA_E_B64URL, TEST_RSA_N_B64URL, TEST_RSA_PRIVATE_PEM};
use cosmon_rpp_adapter::{JwksStore, JwtVerifier, Posture, RppRejectReason};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn clock_seconds() -> Result<u64, Box<dyn std::error::Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn verifier_store() -> TestResultStore {
    let mut store = JwksStore::default();
    let keys = json!({"keys": [{"kid": "clock-key", "alg": "RS256", "kty": "RSA",
        "n": TEST_RSA_N_B64URL, "e": TEST_RSA_E_B64URL}]});
    store.replace_remote_jwks(
        "https://issuer.example",
        vec!["cosmon-rpp-test".into()],
        &keys.to_string(),
    )?;
    Ok(store)
}

type TestResultStore = Result<JwksStore, Box<dyn std::error::Error>>;

fn signed_token(
    iat: u64,
    exp: u64,
    nbf: Option<u64>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut claims: Value = json!({"iss": "https://issuer.example", "sub": "operator",
        "aud": "cosmon-rpp-test", "iat": iat, "exp": exp, "jti": "clock-check"});
    if let Some(nbf) = nbf {
        claims["nbf"] = json!(nbf);
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("clock-key".into());
    Ok(jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes())?,
    )?)
}

#[test]
fn future_issuance_is_not_yet_valid() -> TestResult {
    let now = clock_seconds()?;
    let token = signed_token(now + 300, now + 600, None)?;
    assert!(matches!(
        JwtVerifier::validate(&verifier_store()?, &token, Posture::Active),
        Err(RppRejectReason::NotYetValid)
    ));
    Ok(())
}

#[test]
fn future_start_is_not_yet_valid() -> TestResult {
    let now = clock_seconds()?;
    let token = signed_token(now, now + 600, Some(now + 300))?;
    assert!(matches!(
        JwtVerifier::validate(&verifier_store()?, &token, Posture::Active),
        Err(RppRejectReason::NotYetValid)
    ));
    Ok(())
}

#[test]
fn active_cap_uses_clock_remaining_time() -> TestResult {
    let now = clock_seconds()?;
    let year = 365 * 24 * 60 * 60;
    let token = signed_token(now + year - 60, now + year, None)?;
    assert!(matches!(
        JwtVerifier::validate(&verifier_store()?, &token, Posture::Active),
        Err(RppRejectReason::NotYetValid | RppRejectReason::Expired)
    ));
    Ok(())
}

#[test]
fn small_clock_skew_is_accepted() -> TestResult {
    let now = clock_seconds()?;
    let token = signed_token(now + 10, now + 300, Some(now + 10))?;
    assert!(JwtVerifier::validate(&verifier_store()?, &token, Posture::Active).is_ok());
    Ok(())
}
