// SPDX-License-Identifier: AGPL-3.0-only

//! Independent issuer keys exercise the authentication and binding boundaries.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cosmon_rpp_adapter::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::nucleon_map::HabilitationMap;
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::{
    JwksStore, JwtVerifier, Posture, RppRejectReason, SharedJwksStore, ValidatedJwt,
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header};
use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const A: &str = "https://issuer-a.example";
const B: &str = "https://issuer-b.example";
const AUD: &str = "cosmon-rpp-shared";
const SUB: &str = "overlapping-subject";
const KEY_A: &[u8] = include_bytes!("fixtures/test_rsa_private.pem");
const KEY_B: &[u8] = include_bytes!("fixtures/issuer_b_private.pem");
const PUB_A: &[u8] = include_bytes!("fixtures/test_rsa_public.pem");
const PUB_B: &[u8] = include_bytes!("fixtures/issuer_b_public.pem");

fn store(aud_a: &[&str], aud_b: &[&str]) -> TestResult<JwksStore> {
    assert_ne!(PUB_A, PUB_B);
    Ok(JwksStore::from_pem(
        A,
        "same-kid",
        Algorithm::RS256,
        DecodingKey::from_rsa_pem(PUB_A)?,
        aud_a.iter().map(|s| (*s).to_owned()).collect(),
    )
    .with_pem(
        B,
        "same-kid",
        Algorithm::RS256,
        DecodingKey::from_rsa_pem(PUB_B)?,
        aud_b.iter().map(|s| (*s).to_owned()).collect(),
    ))
}

fn token(iss: &str, key: &[u8], aud: Value) -> TestResult<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let claims = json!({"iss": iss, "sub": SUB, "aud": aud, "iat": now,
        "exp": now + 600, "jti": format!("test-{iss}"),
        "scope": "cosmon:events:subscribe cosmon:molecule:read"});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("same-kid".to_owned());
    Ok(jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(key)?,
    )?)
}

fn pin(root: &Path, id: &str, iss: &str, noyau: &str, scope: &str) -> TestResult {
    let dir = root.join("nucleons").join(id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("oidc-identity.toml"), format!(
        "nucleon_id = {id:?}\nphase = \"Biological\"\nnoyau = {noyau:?}\n[oidc]\nissuer = {iss:?}\nsub = {SUB:?}\naudience = {AUD:?}\n[scopes]\nallowed = [{scope:?}]\n"
    ))?;
    Ok(())
}

fn admit(map: &HabilitationMap, jwt: &ValidatedJwt, root: &Path) -> Result<Spark, RppRejectReason> {
    let limiter = IngressRateLimiter::new(root.join("rate"), 64.0, 0.0);
    let deny = DenyList::new(root.to_path_buf()).with_ttl(Duration::ZERO);
    http_request_to_spark(
        &AdmissionRig {
            nucleon_map: map,
            rate_limiter: &limiter,
            deny_list: &deny,
            inbox_root: &root.join("inbox"),
            now_ms: 0,
        },
        jwt,
        Verb::ObserveMolecule,
        None,
    )
}

#[test]
fn token_from_issuer_a_cannot_redeem_noyau_bound_under_issuer_b() -> TestResult {
    let td = tempfile::tempdir()?;
    pin(td.path(), "binding-b", B, "tenant-b", "scope-b")?;
    let map = HabilitationMap::load(td.path())?;
    let jwks = store(&[AUD], &[AUD])?;
    let a = JwtVerifier::validate(&jwks, &token(A, KEY_A, json!(AUD))?, Posture::Active)?;
    let b = JwtVerifier::validate(&jwks, &token(B, KEY_B, json!(AUD))?, Posture::Active)?;
    assert_eq!(admit(&map, &b, td.path())?.noyau.as_str(), "tenant-b");
    assert!(matches!(
        admit(&map, &a, td.path()),
        Err(RppRejectReason::UnknownSub)
    ));
    Ok(())
}

#[test]
fn audience_allowlists_are_per_issuer_for_scalar_and_array_claims() -> TestResult {
    let jwks = store(&["aud-a"], &["aud-b"])?;
    for (iss, key, own, foreign) in [(A, KEY_A, "aud-a", "aud-b"), (B, KEY_B, "aud-b", "aud-a")] {
        for aud in [json!(own), json!([foreign, own])] {
            assert_eq!(
                JwtVerifier::validate(&jwks, &token(iss, key, aud)?, Posture::Active)?.aud,
                own
            );
        }
        for aud in [json!(foreign), json!([foreign])] {
            assert!(matches!(
                JwtVerifier::validate(&jwks, &token(iss, key, aud)?, Posture::Active),
                Err(RppRejectReason::AudienceMismatch)
            ));
        }
    }
    Ok(())
}

#[test]
fn overlapping_subjects_keep_admission_and_binding_scopes_separate() -> TestResult {
    let td = tempfile::tempdir()?;
    pin(td.path(), "binding-a", A, "tenant-a", "scope-a")?;
    pin(td.path(), "binding-b", B, "tenant-b", "scope-b")?;
    let map = HabilitationMap::load(td.path())?;
    assert_eq!(map.binding_count(), 2);
    let jwks = store(&[AUD], &[AUD])?;
    for (iss, key, tenant, scope) in [
        (A, KEY_A, "tenant-a", "scope-a"),
        (B, KEY_B, "tenant-b", "scope-b"),
    ] {
        let jwt = JwtVerifier::validate(&jwks, &token(iss, key, json!(AUD))?, Posture::Active)?;
        assert_eq!(admit(&map, &jwt, td.path())?.noyau.as_str(), tenant);
        assert_eq!(map.allowed_scopes_for_audience(iss, SUB, AUD), &[scope]);
    }
    Ok(())
}

#[test]
fn signing_key_cannot_impersonate_another_issuer_with_the_same_kid() -> TestResult {
    let jwks = store(&[AUD], &[AUD])?;
    for (iss, wrong_key) in [(B, KEY_A), (A, KEY_B)] {
        assert!(matches!(
            JwtVerifier::validate(&jwks, &token(iss, wrong_key, json!(AUD))?, Posture::Active),
            Err(RppRejectReason::SignatureInvalid)
        ));
    }
    Ok(())
}

#[test]
fn published_store_removal_rejects_next_validation_and_keeps_other_issuer() -> TestResult {
    let shared = SharedJwksStore::new(store(&[AUD], &[AUD])?);
    let a = token(A, KEY_A, json!(AUD))?;
    let b = token(B, KEY_B, json!(AUD))?;
    JwtVerifier::validate(&shared.load(), &a, Posture::Active)?;
    JwtVerifier::validate(&shared.load(), &b, Posture::Active)?;
    shared.store(JwksStore::from_pem(
        B,
        "same-kid",
        Algorithm::RS256,
        DecodingKey::from_rsa_pem(PUB_B)?,
        vec![AUD.to_owned()],
    ));
    assert!(matches!(
        JwtVerifier::validate(&shared.load(), &a, Posture::Active),
        Err(RppRejectReason::IssuerNotPinned)
    ));
    JwtVerifier::validate(&shared.load(), &b, Posture::Active)?;
    Ok(())
}
