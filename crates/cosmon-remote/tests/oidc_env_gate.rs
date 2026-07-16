// SPDX-License-Identifier: AGPL-3.0-only

//! The oidc-side defense against the read-only Env backend (adversarial review
//! F4). `login` and the silent refresh must never resolve to a
//! `$COSMON_REMOTE_TOKEN` static bearer and then silently discard the credential
//! they mint or rotate.
//!
//! A **single** test in a dedicated binary: it mutates the process-global
//! `$COSMON_REMOTE_TOKEN` (which needs an `unsafe` block the credential module
//! forbids), so — like `credential_env.rs` — it runs one env-touching test with
//! no intra-file race, and a separate binary keeps that mutation off the other
//! env test.

use std::time::Duration;

use chrono::Utc;
use cosmon_remote::config::ENV_TOKEN;
use cosmon_remote::credential::{BackendKind, CredentialKey, CredentialStore};
use cosmon_remote::oidc::{self, OidcEndpoints, RefreshConfig, RefreshRotation, TokenState};
use cosmon_remote::Error;

fn endpoints() -> OidcEndpoints {
    OidcEndpoints::new(
        "https://forge.example",
        "https://forge.example/authorize",
        "https://forge.example/token",
        "cs-rpp-adapter",
        "http://127.0.0.1:0/callback",
        vec!["openid".to_owned()],
    )
}

#[tokio::test]
async fn oidc_flow_is_defended_against_the_read_only_env_backend() {
    let tmp = tempfile::TempDir::new().unwrap();
    // SAFETY: the single env-touching test in this binary, restored before return.
    unsafe {
        std::env::set_var(ENV_TOKEN, "ci-bearer");
    }
    let store = CredentialStore::detect_at(tmp.path().to_path_buf()).unwrap();
    assert_eq!(store.backend_kind(), BackendKind::Env);

    let http = reqwest::Client::new();

    // (1) `login` must refuse the Env backend at step 0 — before the loopback
    // listener binds or the browser opens — because the freshly minted
    // credential could not be persisted (F4 #1). `open` must never be called.
    let login_outcome = oidc::login(
        &http,
        &store,
        &endpoints(),
        "operator",
        Duration::from_secs(1),
        |_url| panic!("login must reject the Env backend before opening a browser"),
    )
    .await;
    assert!(
        matches!(
            login_outcome,
            Err(Error::Oidc(oidc::OidcError::CredentialNotPersisted))
        ),
        "login on a read-only Env backend must fail loud with CredentialNotPersisted, \
         got {login_outcome:?}"
    );

    // (2) The silent-refresh seam must resolve the static bearer with ZERO
    // network and never reach the refresh path (which would `store()` into a
    // discard): no mock server is wired, so any network attempt would error.
    let key = CredentialKey::new("https://forge.example", "operator", "cs-rpp-adapter");
    let cfg = RefreshConfig {
        token_endpoint: "https://forge.example/token".to_owned(),
        client_id: "cs-rpp-adapter".to_owned(),
        rotation: RefreshRotation::Rotating,
    };
    let state = oidc::ensure_token(
        &http,
        &store,
        &key,
        &cfg,
        Utc::now(),
        chrono::Duration::seconds(60),
    )
    .await
    .expect("env bearer resolves without network");
    match state {
        TokenState::Valid(tok) => assert_eq!(tok.expose(), "ci-bearer"),
        TokenState::NeedsLogin => panic!("static env bearer must resolve valid, not NeedsLogin"),
    }

    unsafe {
        std::env::remove_var(ENV_TOKEN);
    }
}
