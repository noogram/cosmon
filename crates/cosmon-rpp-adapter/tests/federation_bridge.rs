// SPDX-License-Identifier: AGPL-3.0-only

//! Federated bridge end-to-end — ADR-0023 MVP-A (Dave↔Casey↔`speck`).
//!
//! The thesis (ADR-0023 D1/D2): a peer instance does **not** push its
//! scope across the border; it *presents a signed identity*, and the
//! receiving instance — host-side and sealed at home — decides what that
//! identity opens. This is posture (b) of ADR-0022 with one novelty: the
//! pinned `(iss, sub)` points at a **foreign** instance instead of the
//! local oidc-mock. There is **no new type** and **no refactor of
//! `nucleon_map.rs`** — a peer is simply a second issuer.
//!
//! Concretely, on *Casey's* server:
//!
//! - **authn (JWKS).** Casey trusts Dave's instance JWKS as a second
//!   issuer. `jwt.rs` already keys on `(iss, kid)`, so this is purely
//!   additive (drop `<iss>.json` under `security/jwks/`).
//! - **authz (pin).** Casey writes, host-side and BLAKE3-sealed, a pin
//!   `(iss = dave-instance, sub = dave) → noyau = speck`. The
//!   binding map already keys on `(iss, sub)`, so the foreign emitter is
//!   absorbed with zero special-casing.
//!
//! The two doors are distinct (ADR-0023 addendum point 2 —
//! *« JWKS (authN) ≠ pin (authZ) »*): the signature **proposes**, the pin
//! **disposes**. A token whose issuer is not in the JWKS never
//! authenticates; a perfectly-authenticated token whose `(iss, sub)` has
//! no pin is **inert** (D2). Revocation is passive and sovereign:
//! `rm` the pin (authz) or `rm` the JWKS (authn) + reload — the security
//! *increases* as the bridge is dismantled (D6).
//!
//! NB on the fixture: `cosmon-oidc-testkit` signs every issuer with the
//! same embedded RSA key (only `iss`/`kid` differ). That is sufficient to
//! prove `(iss, kid)`-routing, the authz sovereignty, and the hot-reload
//! of both doors — which is what MVP-A is about. Cryptographic key
//! separation between instances is a property of the production loader
//! (each issuer ships its own JWKS) and is not the subject here.

use std::path::Path;
use std::time::Duration;

use cosmon_oidc_testkit::{OidcMock, OidcMockConfig};
use cosmon_rpp_adapter::admission::{http_request_to_spark, AdmissionRig, Spark, Verb};
use cosmon_rpp_adapter::deny_list::DenyList;
use cosmon_rpp_adapter::image_init::ImageInit;
use cosmon_rpp_adapter::nucleon_map::{HabilitationMap, SharedHabilitationMap};
use cosmon_rpp_adapter::rate_limit::IngressRateLimiter;
use cosmon_rpp_adapter::reload::{reload, reload_jwks};
use cosmon_rpp_adapter::{
    JwksStore, JwtVerifier, Posture, RppRejectReason, SharedJwksStore, ValidatedJwt,
};

const PARTON_AUD: &str = "cosmon-rpp-speck";

/// Casey's own local `IdP` (`oidc-mock`) config — his home login.
fn jesse_local_config() -> OidcMockConfig {
    OidcMockConfig {
        issuer: "https://casey.local.idp".to_owned(),
        audiences: vec!["cosmon-rpp-casey-home".to_owned()],
        kid: "casey-kid".to_owned(),
        default_lifetime_secs: 600,
    }
}

/// Dave's *peer instance* `IdP` config — the foreign emitter Casey
/// federates with on `speck`.
fn dave_peer_config() -> OidcMockConfig {
    OidcMockConfig {
        issuer: "https://dave.instance.peer".to_owned(),
        audiences: vec![PARTON_AUD.to_owned()],
        kid: "dave-kid".to_owned(),
        default_lifetime_secs: 600,
    }
}

/// Write a host-side, sealed pin `(iss, sub) → noyau` exactly as
/// `cs nucleon bind` / the audited renderer would. The loader BLAKE3-seals
/// it on read.
fn write_pin(state_dir: &Path, hab_id: &str, noyau: &str, iss: &str, sub: &str, aud: &str) {
    let dir = state_dir.join("nucleons").join(hab_id);
    std::fs::create_dir_all(&dir).unwrap();
    let body = format!(
        "nucleon_id = \"{hab_id}\"\n\
         phase = \"Biological\"\n\
         noyau = \"{noyau}\"\n\n\
         [oidc]\n\
         issuer = \"{iss}\"\n\
         sub = \"{sub}\"\n\
         audience = \"{aud}\"\n",
    );
    std::fs::write(dir.join("oidc-identity.toml"), body).unwrap();
}

fn image_init_in(td: &Path) -> ImageInit {
    ImageInit {
        inbox_root: td.join("inbox"),
        galaxies_root: td.join("galaxies"),
        cs_path: td.join("nonexistent-cs"),
        claude_home: td.join("home"),
        formulas_seed_dir: None,
    }
}

/// Run the five-clause admission boundary with a fresh, generous rate
/// limiter and no deny-list, so a rejection is always an *authz* verdict,
/// never rate/kill noise.
fn admit(map: &HabilitationMap, jwt: &ValidatedJwt, td: &Path) -> Result<Spark, RppRejectReason> {
    let lim = IngressRateLimiter::new(td.join("rl"), 1024.0, 0.0);
    let dl = DenyList::new(td.to_path_buf()).with_ttl(Duration::ZERO);
    let rig = AdmissionRig {
        nucleon_map: map,
        rate_limiter: &lim,
        deny_list: &dl,
        inbox_root: &td.join("inbox/api"),
        now_ms: 0,
    };
    http_request_to_spark(&rig, jwt, Verb::ObserveMolecule, Some("mol-20260616-aaaa"))
}

#[tokio::test]
async fn federated_bridge_admits_peer_and_keeps_authz_sovereign() {
    let td = tempfile::tempdir().unwrap();
    let state_dir = td.path();

    // Casey's server trusts TWO issuers (authn): his own IdP + Dave's
    // peer instance. Both JWKS files land in the same `security/jwks/`.
    let casey = OidcMock::start_with(jesse_local_config()).await;
    let dave = OidcMock::start_with(dave_peer_config()).await;
    let rogue = OidcMock::start_with(OidcMockConfig {
        issuer: "https://rogue.instance.untrusted".to_owned(),
        audiences: vec![PARTON_AUD.to_owned()],
        kid: "rogue-kid".to_owned(),
        default_lifetime_secs: 600,
    })
    .await;
    casey.write_jwks_file(state_dir).unwrap();
    dave.write_jwks_file(state_dir).unwrap();
    // The rogue's JWKS is DELIBERATELY never written — it is untrusted.
    let jwks = JwksStore::load(state_dir).unwrap();

    // Casey pins, host-side and sealed, exactly ONE federation grant:
    // Dave's identity → the `speck` galaxy. (Capability, not ACL:
    // ADR-0023 D4 — one badge, one galaxy.)
    write_pin(
        state_dir,
        "casey-federates-dave-on-speck",
        "speck",
        dave.issuer(),
        "dave",
        PARTON_AUD,
    );
    let shared_map = SharedHabilitationMap::new(HabilitationMap::load(state_dir).unwrap());

    // ── Property 1 — federated admission (D1) ──────────────────────────
    // Dave presents a token signed by his own instance. Casey
    // authenticates it (foreign issuer in the JWKS) and resolves the pin
    // → the `speck` noyau. No cross-account, no shared IdP.
    let dave_token = dave.issue_jwt("dave", &[]);
    let validated = JwtVerifier::validate(&jwks, &dave_token, Posture::Prepared)
        .expect("peer issuer authenticates against the second JWKS");
    assert_eq!(validated.iss, dave.issuer());
    let spark =
        admit(&shared_map.load(), &validated, state_dir).expect("pinned peer identity is admitted");
    assert_eq!(
        spark.noyau.as_str(),
        "speck",
        "the foreign emitter resolves to the galaxy Casey pinned"
    );

    // ── Property 2 — over-declared badge is inert (D2) ─────────────────
    // A different Dave-instance principal (no pin) authenticates fine
    // but authz denies: the signature proposes, the pin disposes.
    let stranger_token = dave.issue_jwt("dave-colleague-with-no-grant", &[]);
    let stranger = JwtVerifier::validate(&jwks, &stranger_token, Posture::Prepared)
        .expect("same peer issuer still authenticates");
    let err = admit(&shared_map.load(), &stranger, state_dir).unwrap_err();
    assert!(
        matches!(err, RppRejectReason::UnknownSub),
        "authenticated but unpinned ⇒ inert (UnknownSub), got {err:?}"
    );

    // ── Property 3 — JWKS(authn) ≠ pin(authz): unknown issuer ──────────
    // The rogue instance signs a token, but Casey never trusted its
    // JWKS. It fails at the authn door — before any authz decision.
    let rogue_token = rogue.issue_jwt("dave", &[]); // even claiming a pinned sub
    let err = JwtVerifier::validate(&jwks, &rogue_token, Posture::Prepared).unwrap_err();
    assert!(
        matches!(err, RppRejectReason::IssuerNotPinned),
        "untrusted issuer never authenticates, got {err:?}"
    );

    // ── Property 4 — sovereign reversibility via the pin (D6) ──────────
    // Casey retires the grant: `rm` the pin dir + reload. Dave still
    // authenticates (his JWKS is untouched) but falls back to
    // deny-by-default. Security increases as the bridge is dismantled.
    std::fs::remove_dir_all(state_dir.join("nucleons/casey-federates-dave-on-speck")).unwrap();
    let out = reload(&shared_map, state_dir, &image_init_in(state_dir));
    assert!(out.error.is_none());
    let still_auth = JwtVerifier::validate(&jwks, &dave_token, Posture::Prepared)
        .expect("authn unaffected by authz revocation");
    let err = admit(&shared_map.load(), &still_auth, state_dir).unwrap_err();
    assert!(
        matches!(err, RppRejectReason::UnknownSub),
        "pin removed ⇒ peer falls back to deny-by-default, got {err:?}"
    );
}

#[tokio::test]
async fn jwks_hot_reload_onboards_and_revokes_peer_without_reboot() {
    // The MVP-A code headline: the authn door (JWKS) is now hot-reloadable
    // on SIGHUP, symmetric with the authz door (the pin map). Onboarding a
    // federated peer no longer needs a reboot that would tear down
    // in-flight tmux workers (ADR-0023 D6; `reload::reload_jwks`).
    let td = tempfile::tempdir().unwrap();
    let state_dir = td.path();

    let dave = OidcMock::start_with(dave_peer_config()).await;
    let dave_token = dave.issue_jwt("dave", &[]);

    // Boot with NO peer JWKS staged → the live store is empty; Dave's
    // token cannot authenticate.
    let shared_jwks = SharedJwksStore::new(JwksStore::load(state_dir).unwrap());
    assert!(
        matches!(
            JwtVerifier::validate(&shared_jwks.load(), &dave_token, Posture::Prepared).unwrap_err(),
            RppRejectReason::IssuerNotPinned
        ),
        "no JWKS yet ⇒ peer cannot authenticate"
    );

    // Operator stages Dave's JWKS host-side and sends SIGHUP
    // (here: `reload_jwks` directly). No reboot.
    dave.write_jwks_file(state_dir).unwrap();
    let out = reload_jwks(&shared_jwks, state_dir);
    assert!(out.is_ok());
    assert_eq!(out.issuers_after, 1);
    assert!(out.keys_after >= 1);

    let validated = JwtVerifier::validate(&shared_jwks.load(), &dave_token, Posture::Prepared)
        .expect("peer authenticates after the hot JWKS reload");
    assert_eq!(validated.iss, dave.issuer());

    // Revoke the authn side too: `rm` the JWKS + reload. The peer can no
    // longer authenticate — the mirror gesture (D6).
    std::fs::remove_dir_all(state_dir.join("security/jwks")).unwrap();
    let out = reload_jwks(&shared_jwks, state_dir);
    assert!(out.is_ok());
    assert_eq!(out.issuers_after, 0);
    assert!(
        matches!(
            JwtVerifier::validate(&shared_jwks.load(), &dave_token, Posture::Prepared).unwrap_err(),
            RppRejectReason::IssuerNotPinned
        ),
        "JWKS removed ⇒ peer authentication revoked without reboot"
    );
}
