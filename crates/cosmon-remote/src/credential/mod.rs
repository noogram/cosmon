// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

//! Secure credential-store for `cosmon-remote` (delib-20260710-33b7, Child 1).
//!
//! This module persists the OAuth credential triple
//! `{access_token, refresh_token, expires_at}` per **claim identity**, so a
//! login lasts (a monthly browser flow) while access tokens rotate silently
//! every 15 minutes. It is the foundation the `oidc` login/refresh flow
//! (Child 2) builds on: this module owns the *storage + isolation + lock +
//! atomic-write primitives*; `oidc` owns the *refresh orchestration protocol*
//! that calls them.
//!
//! # The contract, in one screen
//!
//! - **[`CredentialKey`] (C1) — the audience-isolation mechanism.** Keyed on
//!   `(issuer, sub, aud)` with `aud == client_id`. aud=A and aud=B land in
//!   physically distinct slots; presenting one audience's token to the other is
//!   structurally impossible because the lookup never retrieves it. Fields are
//!   private so the tuple can widen without breaking signatures.
//! - **[`CredentialStore`] (C5) — a concrete struct over a private backend
//!   enum, not an open `pub trait`.** Inherent methods: [`CredentialStore::detect`]
//!   (runtime probe) / [`CredentialStore::load`] (`Ok(None)` for cold) /
//!   [`CredentialStore::store`] (atomic; returns a [`StoreOutcome`] so a
//!   read-only Env write is a *distinguishable* discard, not a silent no-op —
//!   adversarial review F4) / [`CredentialStore::delete`] (idempotent) /
//!   [`CredentialStore::backend_kind`] (diagnostic) / [`CredentialStore::lock`]
//!   (the sidecar advisory lock).
//! - **Backend selection is a runtime probe (C3), never a feature gate.** One
//!   binary → keyring on desktop, 0600 file on a headless box. The Linux
//!   headless case is detected in two stages — a cheap env check rules out the
//!   no-session-bus box, then a bounded real reachability probe confirms a live
//!   `org.freedesktop.secrets` provider actually answers (a session bus is not
//!   a secret provider) — so the 25-second Secret-Service hang is never
//!   triggered and a bus-without-provider degrades to file (adversarial F1).
//! - **Atomic write + advisory lock (C2 primitives).** `tmp + rename`, 0600,
//!   `O_NOFOLLOW`, fstat perm check; a sidecar `flock` lockfile the refresh
//!   loop uses to stay single-writer.
//! - **Secret hygiene (C6).** [`SecretToken`] zeroizes on drop, redacts its
//!   `Debug`, and exposes plaintext through one accessor — with the honest
//!   caveat that zeroize is not a containment boundary.
//! - **Error taxonomy (C4).** [`crate::CredentialStoreError`] is an own
//!   `#[non_exhaustive]` enum; foreign backend errors are captured opaquely.

mod key;
mod secret;
mod store;

pub use key::CredentialKey;
pub use secret::{SecretToken, StoredCredential};
pub use store::{BackendKind, CredentialLock, CredentialStore, StoreOutcome, ENV_CRED_BACKEND};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use proptest::prelude::*;
    use tempfile::TempDir;

    fn ts(secs: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    fn cred(access: &str, refresh: &str, expires: i64) -> StoredCredential {
        StoredCredential::new(
            SecretToken::new(access),
            SecretToken::new(refresh),
            ts(expires),
        )
    }

    fn same(a: &StoredCredential, b: &StoredCredential) -> bool {
        a.access_token().expose() == b.access_token().expose()
            && a.refresh_token().expose() == b.refresh_token().expose()
            && a.expires_at() == b.expires_at()
    }

    fn key(aud: &str) -> CredentialKey {
        CredentialKey::new("https://forge.example", "operator", aud)
    }

    // --- store roundtrip ---------------------------------------------------

    #[test]
    fn test_file_store_roundtrips_the_triple() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        let c = cred("access-abc", "refresh-xyz", 1_800_000_000);
        store.store(&k, &c).unwrap();
        let got = store.load(&k).unwrap().expect("credential present");
        assert!(same(&c, &got));
        assert_eq!(store.backend_kind(), BackendKind::File);
    }

    #[test]
    fn test_file_store_reports_persisted_outcome() {
        // The File backend actually writes to disk, so its `store` reports the
        // distinguishable `Persisted` arm — the complement of the Env backend's
        // `Discarded` (exercised in `tests/credential_env.rs`). Together they
        // make the silent-no-op oracle (F4 #1) impossible: every caller can tell
        // a durable write from a discarded one.
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        assert_eq!(
            store.store(&k, &cred("a", "r", 1)).unwrap(),
            StoreOutcome::Persisted
        );
    }

    #[test]
    fn test_load_absent_credential_is_ok_none() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        assert!(store.load(&key("never-written")).unwrap().is_none());
    }

    #[test]
    fn test_delete_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        // Deleting an absent credential is fine.
        store.delete(&k).unwrap();
        store.store(&k, &cred("a", "r", 1)).unwrap();
        store.delete(&k).unwrap();
        assert!(store.load(&k).unwrap().is_none());
        // And again — still fine.
        store.delete(&k).unwrap();
    }

    #[test]
    fn test_store_overwrites_atomically_in_place() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        store.store(&k, &cred("first", "r1", 100)).unwrap();
        store.store(&k, &cred("second", "r2", 200)).unwrap();
        let got = store.load(&k).unwrap().unwrap();
        assert_eq!(got.access_token().expose(), "second");
        assert_eq!(got.expires_at(), ts(200));
    }

    /// F2 regression: `store` fsyncs the containing `credentials/` directory
    /// after the `tmp + rename`, so the rename itself is crash-durable (POSIX —
    /// a rename only survives a power loss once the parent directory entry is
    /// flushed; `write_tmp` only fsynced the blob's *data*). We cannot simulate a
    /// power loss in a unit test, but we pin that the durability-hardened write
    /// path stays a clean roundtrip with no leftover `.tmp` litter — the parent
    /// fsync must never turn a store that took effect into a failure.
    #[test]
    fn test_store_persists_with_parent_dir_fsync_and_no_tmp_litter() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        store
            .store(&k, &cred("rotated", "refresh-new", 300))
            .unwrap();
        // The rename landed the credential, readable back byte-identical.
        assert_eq!(
            store.load(&k).unwrap().unwrap().access_token().expose(),
            "rotated"
        );
        // No `.tmp` file survived the rename in the credentials directory.
        let dir = tmp.path().join("credentials");
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "no .tmp litter after a durable store");
    }

    // --- audience isolation (C1) — the load-bearing test -------------------

    #[test]
    fn test_audience_isolation_distinct_slots() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let a = key("cs-rpp-adapter"); // audience A
        let b = key("claude-web"); // audience B
        store.store(&a, &cred("token-A", "refresh-A", 100)).unwrap();
        // The B slot was never written → it must be empty. Presenting A's token
        // to B is structurally impossible: the lookup for B does not find A.
        assert!(store.load(&b).unwrap().is_none());
        // A is still exactly A.
        assert_eq!(
            store.load(&a).unwrap().unwrap().access_token().expose(),
            "token-A"
        );
    }

    #[test]
    fn test_sub_isolation_distinct_slots() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let operator = CredentialKey::new("https://forge.example", "operator", "cs-rpp-adapter");
        let avatar = CredentialKey::new("https://forge.example", "avatar", "cs-rpp-adapter");
        store.store(&operator, &cred("op", "r", 1)).unwrap();
        assert!(store.load(&avatar).unwrap().is_none());
    }

    // --- permission / symlink hardening (C2, turing-T6) --------------------

    #[cfg(unix)]
    #[test]
    fn test_written_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        store.store(&k, &cred("a", "r", 1)).unwrap();
        let path = tmp
            .path()
            .join("credentials")
            .join(format!("{}.cred", k.storage_id()));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "credential file must be 0600");
    }

    #[cfg(unix)]
    #[test]
    fn test_insecure_permissions_rejected_on_load() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        store.store(&k, &cred("a", "r", 1)).unwrap();
        let path = tmp
            .path()
            .join("credentials")
            .join(format!("{}.cred", k.storage_id()));
        // Widen the bits — a load must now refuse.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        let err = store.load(&k).unwrap_err();
        assert!(
            matches!(
                err,
                crate::Error::Credential(crate::CredentialStoreError::InsecurePermissions { .. })
            ),
            "expected InsecurePermissions, got {err:?}"
        );
    }

    // --- malformed / schema fail-closed (C4) -------------------------------

    #[test]
    fn test_malformed_blob_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        let dir = tmp.path().join("credentials");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.cred", k.storage_id()));
        std::fs::write(&path, b"this is not json").unwrap();
        tighten_0600(&path);
        let err = store.load(&k).unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Credential(crate::CredentialStoreError::Malformed { .. })
        ));
    }

    #[test]
    fn test_future_schema_version_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        let dir = tmp.path().join("credentials");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.cred", k.storage_id()));
        std::fs::write(
            &path,
            br#"{"schema_version":999,"access_token":"a","refresh_token":"r","expires_at":"2030-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        tighten_0600(&path);
        let err = store.load(&k).unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Credential(crate::CredentialStoreError::Malformed { .. })
        ));
    }

    /// Tighten a hand-written fixture file to 0600 so the malformed-parse path
    /// is reached rather than the (correct) insecure-permissions rejection. A
    /// no-op on non-unix, where the permission check itself is a no-op.
    fn tighten_0600(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms).unwrap();
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    // --- advisory lock primitive (C2) --------------------------------------

    #[test]
    fn test_lock_excludes_a_concurrent_try_lock() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let k = key("cs-rpp-adapter");
        let held = store.lock(&k).unwrap();
        // A second store at the same root cannot try_lock the same key.
        let store2 = CredentialStore::file_at(tmp.path());
        assert!(store2.try_lock(&k).unwrap().is_none());
        drop(held);
        // Once released, try_lock succeeds.
        assert!(store2.try_lock(&k).unwrap().is_some());
    }

    #[test]
    fn test_lock_is_per_key_not_global() {
        let tmp = TempDir::new().unwrap();
        let store = CredentialStore::file_at(tmp.path());
        let _held_a = store.lock(&key("aud-a")).unwrap();
        // A different key's lock is independent.
        let store2 = CredentialStore::file_at(tmp.path());
        assert!(store2.try_lock(&key("aud-b")).unwrap().is_some());
    }

    /// F3 regression: a `<storage_id>.lock` planted as a **symlink** (the vector
    /// an attacker uses to point at a reader-less FIFO and hang the module, or
    /// at a victim path to touch it) must be rejected — never followed. Before
    /// the fix, `open_lock` used `create(true)` with no `O_NOFOLLOW` and would
    /// open through the symlink. Now `create_new + O_NOFOLLOW` refuses it.
    #[cfg(unix)]
    #[test]
    fn test_planted_symlink_lockfile_is_refused_not_followed() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let k = key("cs-rpp-adapter");

        // Materialise the credentials dir the store will use, then plant the
        // lock path as a symlink to an unrelated (victim) file.
        let cred_dir = tmp.path().join("credentials");
        std::fs::create_dir_all(&cred_dir).unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, b"do not touch").unwrap();
        let lock_path = cred_dir.join(format!("{}.lock", k.storage_id()));
        symlink(&victim, &lock_path).unwrap();

        let store = CredentialStore::file_at(tmp.path());
        // The lock acquisition refuses the symlink instead of following it —
        // no hang, no write through to the victim.
        assert!(
            store.lock(&k).is_err(),
            "a symlinked lockfile must be rejected, not followed"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"do not touch",
            "the victim behind the symlink must be untouched"
        );
    }

    // The env backend (static bearer, `$COSMON_REMOTE_TOKEN`) is exercised in
    // `tests/credential_env.rs` — it mutates a process-global env var, which
    // needs an `unsafe` block this module forbids.

    // --- bounded reachability probe (F1 fix) -------------------------------

    // The keyring reachability probe (`keyring_backend_available` on Linux)
    // caps a Secret-Service round-trip via `run_bounded`, so a bus-present /
    // provider-absent box degrades to file instead of hanging ~25s. The
    // real D-Bus probe cannot be exercised deterministically in CI, but the
    // *bound* — the never-hang guarantee — is the load-bearing part, and it
    // is a plain timeout on a scratch thread. These tests pin that mechanism.

    #[test]
    fn test_run_bounded_returns_value_when_fast() {
        let got = store::run_bounded(std::time::Duration::from_secs(5), || 42);
        assert_eq!(got, Some(42));
    }

    #[test]
    fn test_run_bounded_times_out_and_does_not_hang() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        // The probe sleeps far longer than the budget → the caller must give
        // up at the budget, not block for the full sleep.
        let got = store::run_bounded(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(30));
            true
        });
        let elapsed = start.elapsed();
        assert_eq!(got, None, "a probe over budget must yield None");
        assert!(
            elapsed < Duration::from_secs(5),
            "run_bounded must return at the budget, not wait out the probe (took {elapsed:?})"
        );
    }

    /// Linux musl intentionally builds `keyring` without a native backend.
    /// Its fallback is a process-local mock store, so autodetection must use
    /// the durable file backend and an explicit keyring request must fail.
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    #[test]
    fn musl_uses_file_by_default_and_rejects_keyring_override() {
        assert_eq!(
            store::resolve_backend_kind(None, false, store::native_keyring_backend_supported())
                .unwrap(),
            BackendKind::File
        );
        assert!(
            store::resolve_backend_kind(
                Some(BackendKind::Keyring),
                false,
                store::native_keyring_backend_supported(),
            )
            .is_err(),
            "musl must not select keyring's process-local mock store"
        );
    }

    // --- expiry semantics --------------------------------------------------

    #[test]
    fn test_expiry_is_inclusive_at_the_instant() {
        let c = cred("a", "r", 1_000);
        assert!(c.is_expired(ts(1_000)), "expired exactly at expires_at");
        assert!(c.is_expired(ts(1_001)));
        assert!(!c.is_expired(ts(999)));
    }

    #[test]
    fn test_is_expired_within_leeway() {
        let c = cred("a", "r", 1_000);
        // 30s before expiry, with a 60s leeway → considered expiring.
        assert!(c.is_expired_within(ts(970), Duration::seconds(60)));
        // 120s before expiry, with a 60s leeway → still fresh.
        assert!(!c.is_expired_within(ts(880), Duration::seconds(60)));
    }

    // --- proptest: the invariants (C9) -------------------------------------

    proptest! {
        /// Roundtrip: whatever we store, we load back byte-identical.
        #[test]
        fn prop_store_load_roundtrip(
            access in ".{0,200}",
            refresh in ".{0,200}",
            secs in 0i64..4_000_000_000,
            aud in "[a-z0-9-]{1,40}",
        ) {
            let tmp = TempDir::new().unwrap();
            let store = CredentialStore::file_at(tmp.path());
            let k = key(&aud);
            let c = cred(&access, &refresh, secs);
            store.store(&k, &c).unwrap();
            let got = store.load(&k).unwrap().unwrap();
            prop_assert_eq!(got.access_token().expose(), access.as_str());
            prop_assert_eq!(got.refresh_token().expose(), refresh.as_str());
            prop_assert_eq!(got.expires_at(), ts(secs));
        }

        /// Storage-id injectivity: distinct `(issuer, sub, aud)` tuples map to
        /// distinct slots (BLAKE3 — collisions negligible).
        #[test]
        fn prop_distinct_keys_distinct_storage_ids(
            i1 in "[a-z]{1,20}", s1 in "[a-z]{1,20}", a1 in "[a-z]{1,20}",
            i2 in "[a-z]{1,20}", s2 in "[a-z]{1,20}", a2 in "[a-z]{1,20}",
        ) {
            let k1 = CredentialKey::new(&i1, &s1, &a1);
            let k2 = CredentialKey::new(&i2, &s2, &a2);
            if (i1, s1, a1) == (i2, s2, a2) {
                prop_assert_eq!(k1.storage_id(), k2.storage_id());
            } else {
                prop_assert_ne!(k1.storage_id(), k2.storage_id());
            }
        }

        /// Audience isolation under arbitrary distinct audiences: a token
        /// written under audience A is never returned for a different B.
        #[test]
        fn prop_audience_isolation(
            aud_a in "[a-z0-9-]{1,30}",
            aud_b in "[a-z0-9-]{1,30}",
            token in ".{1,50}",
        ) {
            prop_assume!(aud_a != aud_b);
            let tmp = TempDir::new().unwrap();
            let store = CredentialStore::file_at(tmp.path());
            store.store(&key(&aud_a), &cred(&token, "r", 1)).unwrap();
            prop_assert!(store.load(&key(&aud_b)).unwrap().is_none());
        }

        /// Expiry is a pure, monotone comparison against the stored instant.
        #[test]
        fn prop_expiry_monotone(expires in 0i64..4_000_000_000, now in 0i64..4_000_000_000) {
            let c = cred("a", "r", expires);
            prop_assert_eq!(c.is_expired(ts(now)), now >= expires);
        }
    }
}
