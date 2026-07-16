// SPDX-License-Identifier: AGPL-3.0-only

//! The env (`$COSMON_REMOTE_TOKEN`) backend of the credential-store.
//!
//! Lives in an integration test — not the module's `#[cfg(test)]` block —
//! because it mutates a process-global env var, which requires an `unsafe`
//! block the credential module forbids (`#![forbid(unsafe_code)]`). This binary
//! runs a single env-touching test, so there is no intra-file race.

use cosmon_remote::config::ENV_TOKEN;
use cosmon_remote::credential::{BackendKind, CredentialKey, CredentialStore, StoreOutcome};

#[test]
fn env_backend_yields_a_static_non_refreshable_bearer() {
    let tmp = tempfile::TempDir::new().unwrap();
    // SAFETY: single-threaded test, one env var, restored before return.
    unsafe {
        std::env::set_var(ENV_TOKEN, "ci-bearer");
    }
    let store = CredentialStore::detect_at(tmp.path().to_path_buf()).unwrap();
    assert_eq!(store.backend_kind(), BackendKind::Env);

    let key = CredentialKey::new("https://forge.example", "operator", "cs-rpp-adapter");
    let got = store.load(&key).unwrap().expect("env bearer present");
    assert_eq!(got.access_token().expose(), "ci-bearer");
    assert!(!got.has_refresh(), "env bearer carries no refresh token");
    assert!(
        !got.is_expired(chrono::Utc::now()),
        "env bearer never expires"
    );

    // Writes are no-ops on the read-only env backend — and the no-op is now a
    // *distinguishable* outcome (`Discarded`), not a silent `Ok(())`, so a
    // refresh writer can tell "persisted" from "silently dropped" (F4 #1).
    let dummy = cosmon_remote::StoredCredential::new(
        cosmon_remote::SecretToken::new("x"),
        cosmon_remote::SecretToken::new("y"),
        chrono::Utc::now(),
    );
    assert_eq!(
        store.store(&key, &dummy).unwrap(),
        StoreOutcome::Discarded,
        "an Env-backed store must report the write was discarded, not persisted"
    );

    // Audience isolation does NOT hold in env mode: the same static bearer is
    // returned for every distinct audience (F4 #3) — the documented caveat that
    // keeps a production refresh writer off the Env backend.
    let other_aud = CredentialKey::new("https://forge.example", "operator", "claude-web");
    let got_other = store.load(&other_aud).unwrap().expect("env bearer present");
    assert_eq!(
        got_other.access_token().expose(),
        "ci-bearer",
        "env backend presents one token to all audiences (isolation bypassed)"
    );

    unsafe {
        std::env::remove_var(ENV_TOKEN);
    }
}
