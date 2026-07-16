// SPDX-License-Identifier: AGPL-3.0-only

//! The concrete credential store (C5) — a struct wrapping a private backend
//! enum, never an open `pub trait`.
//!
//! The backend set (keyring / file / env) is closed and cosmon-owned, so the
//! contract rejects an open `pub trait CredentialStore`: a public trait freezes
//! every method signature forever for a capability no external crate needs
//! today. Instead this is a concrete [`CredentialStore`] with inherent methods
//! over a private [`Backend`]. Adding a backend (Vault, cloud KMS) is a new
//! private enum arm — a minor change — not a breaking trait-method addition.
//!
//! Store I/O is **synchronous**: the persistence is local and fast, and an
//! `async fn` in a store would kill object-safety and infect every caller. Only
//! the network refresh (in the future `oidc` module) is async.
//!
//! This module owns three of the C2 refresh-safety primitives; `oidc`
//! orchestrates them into the full loop:
//!
//! - **atomic write** — [`CredentialStore::store`] writes one blob under one
//!   key via `tmp + rename`, 0600, `O_NOFOLLOW`, with a post-open fstat
//!   permission check on read;
//! - **advisory single-writer lock** — [`CredentialStore::lock`] /
//!   [`CredentialStore::try_lock`] over a **sidecar lockfile** (separate from
//!   the secret, because the keyring backend has no file to lock);
//! - **cold-read** — [`CredentialStore::load`] returns `Ok(None)` for an absent
//!   credential (parse, don't validate), the read half of the compare-and-swap
//!   that `oidc` performs while holding the lock.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(target_os = "linux", test))]
#[cfg(any(all(target_os = "linux", not(target_env = "musl")), test))]
use std::time::Duration;

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::key::CredentialKey;
use super::secret::{SecretToken, StoredCredential};
use crate::config::ENV_TOKEN;
use crate::error::{CredentialStoreError, Error, Result};

/// Environment override forcing a specific backend for one invocation, e.g.
/// `COSMON_REMOTE_CRED_BACKEND=file` to bypass the keyring on a machine whose
/// keychain the operator does not want to touch. Accepted values: `keyring`,
/// `file`. Takes precedence over the runtime probe but *not* over
/// [`ENV_TOKEN`].
pub const ENV_CRED_BACKEND: &str = "COSMON_REMOTE_CRED_BACKEND";

/// The keyring "service" namespace. All slots share one service; the per-key
/// [`CredentialKey::storage_id`] is the keyring "account".
const KEYRING_SERVICE: &str = "cosmon-remote";

/// Wire schema version for the persisted blob. Bumped only on an incompatible
/// layout change; [`parse_blob`] fails **closed** on a newer version.
const CRED_SCHEMA: u32 = 1;

/// Process-local counter making concurrent temp-file names unique even within
/// one process (the pid alone is not enough for two threads).
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Which backend a [`CredentialStore`] resolved to — a diagnostic surfaced by
/// [`CredentialStore::backend_kind`] (used by `doctor` / `--json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// OS-native keyring (macOS Keychain / Windows Cred Manager / Linux Secret
    /// Service).
    Keyring,
    /// The 0600 fallback file under the config directory.
    File,
    /// A static bearer supplied via [`ENV_TOKEN`] (CI / smoke harness). Read
    /// only — writes are no-ops.
    Env,
}

impl BackendKind {
    /// A short lowercase label for `--json` / `doctor` output.
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Keyring => "keyring",
            BackendKind::File => "file",
            BackendKind::Env => "env",
        }
    }
}

/// The outcome of a [`CredentialStore::store`] — did the write reach durable
/// storage, or was it intentionally discarded by a read-only backend?
///
/// A bare `Ok(())` could not tell a refresh writer (persist-before-use, C2)
/// *"your rotated token is on disk"* from *"your rotated token evaporated"*:
/// the [`BackendKind::Env`] backend is read-only and `store` there is a
/// deliberate no-op. That made `store` a **silent-failure oracle** — a caller
/// in the refresh protocol saw `Ok(())` whether or not the credential was
/// persisted (adversarial review F4 #1). Returning this two-armed outcome makes
/// the discard distinguishable at the type level, so a caller can branch on
/// [`StoreOutcome::Discarded`] and fail loud rather than trust a write that
/// never happened.
///
/// Note this is **not** `#[must_use]`: the load-bearing guard against a
/// mis-wired refresh writer is the *runtime* gate in `oidc`
/// (`backend_kind() != Env` before any `store`), not a compile-time lint —
/// `oidc` checks this outcome explicitly at its two persist sites. The enum is
/// the branchable signal; the gate is the enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    /// The credential was durably written to the backing store (keyring set, or
    /// the 0600 file `tmp + rename`d and the parent directory fsynced).
    Persisted,
    /// The write was intentionally discarded because the resolved backend is
    /// read-only ([`BackendKind::Env`]). A refresh writer that reaches this has
    /// silently lost the rotated credential and must treat it as a failure.
    Discarded,
}

/// The active backend. **Private** — the closed set is the seam (C5).
enum Backend {
    Keyring,
    File,
    Env,
}

/// The credential store: persists `{access, refresh, expires_at}` keyed by
/// [`CredentialKey`].
///
/// Construct with [`CredentialStore::detect`] (the runtime probe). `root` is the
/// config directory that holds the fallback `credentials/` files **and** the
/// sidecar lockfiles — the lock lives on the filesystem regardless of backend,
/// because the keyring has no file to lock.
pub struct CredentialStore {
    backend: Backend,
    root: PathBuf,
}

impl CredentialStore {
    /// Resolve the backend by a **runtime probe**, never a compile-time feature
    /// gate (C3): one binary ships to both desktop and headless box. Precedence:
    ///
    /// 1. `$COSMON_REMOTE_TOKEN` set and non-empty → [`BackendKind::Env`]
    ///    (static bearer, CI).
    /// 2. `$COSMON_REMOTE_CRED_BACKEND` override (`keyring` | `file`).
    /// 3. keyring reachable (bounded probe — see [`keyring_backend_available`]) →
    ///    [`BackendKind::Keyring`].
    /// 4. otherwise the 0600 file → [`BackendKind::File`].
    ///
    /// The Linux headless case is detected in two stages so the 25-second
    /// Secret-Service connect hang is never triggered — the highest-probability
    /// field failure (kahneman-F1): first a **cheap env check** rules out the
    /// no-session-bus box, then — because a session bus is *not* the same thing
    /// as a live `org.freedesktop.secrets` provider — a **bounded real
    /// reachability probe** (sub-second, abandoned on timeout) confirms a
    /// provider actually answers before committing to Keyring. A bus with no
    /// secret provider (systemd `--user`, containers, SSH + `pam_systemd`)
    /// therefore degrades to the file backend instead of erroring or stalling
    /// on the first `load`/`store` (adversarial review F1).
    ///
    /// The explicit `$COSMON_REMOTE_CRED_BACKEND=keyring` override (step 2)
    /// bypasses the reachability probe on targets that compile a native keyring
    /// backend: an operator who names the backend gets it verbatim, and a
    /// genuine backend error surfaces rather than silently writing plaintext to
    /// the 0600 file against that stated choice. On targets with no native
    /// keyring backend (including Linux musl), that override is rejected rather
    /// than selecting keyring's process-local mock store.
    pub fn detect() -> Result<Self> {
        let root = default_root()?;
        Self::detect_at(root)
    }

    /// [`Self::detect`] against an explicit config root — the seam used by
    /// tests and by callers that pin the config directory.
    pub fn detect_at(root: PathBuf) -> Result<Self> {
        // 1. Env override — a static bearer, no refresh, no persistence.
        if env_nonempty(ENV_TOKEN) {
            // `$COSMON_REMOTE_TOKEN` set in a shell rc silently shadows an
            // otherwise-valid keyring/file credential (adversarial review
            // F4 #2), downgrading a refreshable identity to a static bearer
            // with no other signal. Emit that signal — once — so the override
            // is not invisible.
            warn_env_backend_shadows();
            return Ok(Self {
                backend: Backend::Env,
                root,
            });
        }
        let kind = resolve_backend_kind(
            backend_override()?,
            keyring_backend_available(),
            native_keyring_backend_supported(),
        )?;
        let backend = match kind {
            BackendKind::Keyring => Backend::Keyring,
            BackendKind::File => Backend::File,
            BackendKind::Env => Backend::Env,
        };
        Ok(Self { backend, root })
    }

    /// Build a store pinned to the file backend at `root` — for tests and for
    /// the explicit-file path.
    pub fn file_at(root: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::File,
            root: root.into(),
        }
    }

    /// Which backend this store resolved to.
    pub fn backend_kind(&self) -> BackendKind {
        match self.backend {
            Backend::Keyring => BackendKind::Keyring,
            Backend::File => BackendKind::File,
            Backend::Env => BackendKind::Env,
        }
    }

    /// Load the credential for `key`, or `Ok(None)` if none is stored
    /// (**cold-read**: absence is not an error — parse, don't validate).
    ///
    /// For [`BackendKind::Env`] this returns a static bearer built from
    /// [`ENV_TOKEN`] regardless of `key` (CI presents one token for whatever it
    /// runs). For the file backend, the read enforces the 0600 permission and
    /// no-symlink invariants ([`CredentialStoreError::InsecurePermissions`]).
    pub fn load(&self, key: &CredentialKey) -> Result<Option<StoredCredential>> {
        match self.backend {
            Backend::Env => Ok(env_static_credential()),
            Backend::Keyring => keyring_load(key),
            Backend::File => self.file_load(key),
        }
    }

    /// Persist `cred` for `key` **atomically**, replacing any prior blob, and
    /// report whether the write was durable ([`StoreOutcome::Persisted`]) or
    /// intentionally discarded ([`StoreOutcome::Discarded`]).
    ///
    /// The write is one blob under one key: file backend via `tmp + rename`,
    /// keyring backend via a single `set_password`.
    ///
    /// # Env write-contract (read carefully — this is a footgun seam)
    ///
    /// For [`BackendKind::Env`] this is a **no-op** and returns
    /// [`StoreOutcome::Discarded`]: the `$COSMON_REMOTE_TOKEN` bearer is static
    /// and read-only, there is nowhere to persist to. A caller in the refresh
    /// protocol (persist-before-use, C2) **must not** treat a `store` on an Env
    /// backend as a durable rotation — the returned `Discarded` is the signal
    /// that the credential was *not* saved. The safe contract is that `oidc`
    /// gates every refresh on `backend_kind() != Env` (and `has_refresh()`), so
    /// this arm is never reached on the hot path; the distinguishable outcome is
    /// the belt to that gate's braces (adversarial review F4 #1).
    ///
    /// Note the Env backend also **bypasses audience isolation** (C1): its
    /// [`load`](Self::load) returns the *same* bearer for every `key`, so the
    /// per-`(issuer, sub, aud)` slot separation that holds for the keyring/file
    /// backends does not hold in env mode (one token is presented to all
    /// audiences). This is intended for a CI/smoke harness that runs against a
    /// single audience; it is another reason a production refresh writer must
    /// not resolve to Env (adversarial review F4 #3).
    pub fn store(&self, key: &CredentialKey, cred: &StoredCredential) -> Result<StoreOutcome> {
        match self.backend {
            Backend::Env => Ok(StoreOutcome::Discarded),
            Backend::Keyring => keyring_store(key, cred).map(|()| StoreOutcome::Persisted),
            Backend::File => self.file_store(key, cred).map(|()| StoreOutcome::Persisted),
        }
    }

    /// Remove the credential for `key`. **Idempotent** — deleting an absent
    /// credential is `Ok(())`. No-op for [`BackendKind::Env`].
    pub fn delete(&self, key: &CredentialKey) -> Result<()> {
        match self.backend {
            Backend::Env => Ok(()),
            Backend::Keyring => keyring_delete(key),
            Backend::File => self.file_delete(key),
        }
    }

    /// Acquire the **exclusive advisory lock** for `key`, blocking until it is
    /// available. The returned [`CredentialLock`] releases the lock on drop.
    ///
    /// This is the anti-thundering-herd half of the C2 refresh protocol:
    /// `oidc`, holding this lock, re-reads the store, and either adopts a peer's
    /// freshly rotated token or performs exactly one refresh grant — never two
    /// in parallel per `(issuer, sub, aud)` per machine.
    pub fn lock(&self, key: &CredentialKey) -> Result<CredentialLock> {
        let file = self.open_lockfile(key)?;
        FileExt::lock_exclusive(&file).map_err(Error::Io)?;
        Ok(CredentialLock { file })
    }

    /// Try to acquire the exclusive advisory lock for `key` without blocking.
    /// Returns `Ok(None)` if a peer already holds it — the caller then reads
    /// fail-safe (never POSTs an unlocked refresh).
    pub fn try_lock(&self, key: &CredentialKey) -> Result<Option<CredentialLock>> {
        let file = self.open_lockfile(key)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(CredentialLock { file })),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    // --- file-backend internals ------------------------------------------

    fn credentials_dir(&self) -> PathBuf {
        self.root.join("credentials")
    }

    fn ensure_dir(&self) -> Result<PathBuf> {
        let dir = self.credentials_dir();
        fs::create_dir_all(&dir)?;
        harden_dir(&dir)?;
        Ok(dir)
    }

    fn file_path(&self, key: &CredentialKey) -> PathBuf {
        self.credentials_dir()
            .join(format!("{}.cred", key.storage_id()))
    }

    fn open_lockfile(&self, key: &CredentialKey) -> Result<File> {
        let dir = self.ensure_dir()?;
        let path = dir.join(format!("{}.lock", key.storage_id()));
        let file = open_lock(&path)?;
        Ok(file)
    }

    fn file_load(&self, key: &CredentialKey) -> Result<Option<StoredCredential>> {
        let path = self.file_path(key);
        if !path.exists() {
            return Ok(None);
        }
        // Reject a symlink before opening (no O_NOFOLLOW race window): a
        // widened/redirected credential file is not trusted.
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(CredentialStoreError::InsecurePermissions {
                path: path.display().to_string(),
            }
            .into());
        }
        let file = open_read(&path)?;
        check_permissions(&file, &path)?;
        let blob = Zeroizing::new(io::read_to_string(file)?);
        Ok(Some(parse_blob(&blob)?))
    }

    fn file_store(&self, key: &CredentialKey, cred: &StoredCredential) -> Result<()> {
        let dir = self.ensure_dir()?;
        let final_path = dir.join(format!("{}.cred", key.storage_id()));
        let blob = Zeroizing::new(serialize_blob(cred)?);

        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(
            ".{}.{}.{}.tmp",
            key.storage_id(),
            std::process::id(),
            seq
        ));

        // Write + fsync the temp file, then atomically rename it over the
        // final path. On any failure, best-effort remove the temp so we do not
        // leave `.tmp` litter.
        if let Err(e) = write_tmp(&tmp, blob.as_bytes())
            .and_then(|()| fs::rename(&tmp, &final_path).map_err(Error::Io))
        {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        // fsync the containing directory so the *rename* itself is crash-durable
        // (POSIX): `write_tmp` fsynced the blob's data, but a `rename` only
        // survives a power loss once the parent directory entry is flushed.
        // Without this, a crash immediately post-rename can lose the rename and
        // leave the OLD blob on disk — and under single-use refresh rotation the
        // resurrected dead {access,refresh} pair yields `invalid_grant` and a
        // spurious forced re-login on the next refresh (adversarial review F2).
        //
        // Best-effort: the rename already took effect, so a directory fsync the
        // filesystem refuses (e.g. `EINVAL` where dir durability is a no-op) must
        // not convert a store that succeeded into a failure. It only ever tightens
        // durability, never loosens it.
        let _ = fsync_dir(&dir);
        Ok(())
    }

    fn file_delete(&self, key: &CredentialKey) -> Result<()> {
        let path = self.file_path(key);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(e)),
        }
    }
}

/// An acquired advisory lock over a key's sidecar lockfile. The lock is
/// released when this guard is dropped.
#[must_use = "the lock is held only while this guard is alive"]
pub struct CredentialLock {
    file: File,
}

impl Drop for CredentialLock {
    fn drop(&mut self) {
        // Best-effort: the OS also releases the advisory lock when the fd
        // closes, so a failure here is not actionable.
        let _ = FileExt::unlock(&self.file);
    }
}

// --- blob (de)serialization ---------------------------------------------

/// The persisted wire form. `refresh_token` defaults so a blob written by a
/// future access-only variant still parses; unknown fields are tolerated (no
/// `deny_unknown_fields`) so a forward-compatible field does not hard-fail an
/// older binary — the `schema_version` gate is the real fail-closed guard.
///
/// `Zeroize`/`ZeroizeOnDrop` wipe the two plaintext token `String`s when the
/// wire struct falls out of scope. Without this the `.to_owned()` copies made
/// by [`serialize_blob`] (and the deserialized copies in [`parse_blob`]) would
/// be freed un-zeroized on every `store()` — i.e. every 15-minute refresh —
/// steadily seeding the freed heap with recent access+refresh tokens, the exact
/// heap-dump exposure the C6 zeroization discipline exists to shrink. The
/// non-secret `schema_version`/`expires_at` fields are `#[zeroize(skip)]`
/// (`DateTime<Utc>` has no `Zeroize` impl, and neither field is secret).
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct CredentialWire {
    #[serde(default = "default_schema")]
    #[zeroize(skip)]
    schema_version: u32,
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[zeroize(skip)]
    expires_at: DateTime<Utc>,
}

const fn default_schema() -> u32 {
    CRED_SCHEMA
}

fn serialize_blob(cred: &StoredCredential) -> Result<String> {
    let wire = CredentialWire {
        schema_version: CRED_SCHEMA,
        access_token: cred.access_token().expose().to_owned(),
        refresh_token: cred.refresh_token().expose().to_owned(),
        expires_at: cred.expires_at(),
    };
    serde_json::to_string(&wire).map_err(Error::Json)
}

fn parse_blob(blob: &str) -> Result<StoredCredential> {
    let mut wire: CredentialWire =
        serde_json::from_str(blob).map_err(|e| CredentialStoreError::Malformed {
            reason: e.to_string(),
        })?;
    if wire.schema_version > CRED_SCHEMA {
        return Err(CredentialStoreError::Malformed {
            reason: format!(
                "unsupported schema_version {} (this binary understands ≤ {CRED_SCHEMA}); \
                 re-run `login`",
                wire.schema_version
            ),
        }
        .into());
    }
    // `CredentialWire` implements `Drop` (via `ZeroizeOnDrop`), so the token
    // fields cannot be moved out by value — `mem::take` moves each plaintext
    // into its `SecretToken` and leaves an empty string for the drop-zeroize.
    Ok(StoredCredential::new(
        SecretToken::new(std::mem::take(&mut wire.access_token)),
        SecretToken::new(std::mem::take(&mut wire.refresh_token)),
        wire.expires_at,
    ))
}

// --- keyring backend -----------------------------------------------------

fn keyring_entry(key: &CredentialKey) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, &key.storage_id()).map_err(|e| backend_err(e).into())
}

fn keyring_load(key: &CredentialKey) -> Result<Option<StoredCredential>> {
    let entry = keyring_entry(key)?;
    match entry.get_password() {
        Ok(blob) => {
            let blob = Zeroizing::new(blob);
            Ok(Some(parse_blob(&blob)?))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(backend_err(e).into()),
    }
}

fn keyring_store(key: &CredentialKey, cred: &StoredCredential) -> Result<()> {
    let entry = keyring_entry(key)?;
    let blob = Zeroizing::new(serialize_blob(cred)?);
    entry.set_password(&blob).map_err(backend_err)?;
    Ok(())
}

fn keyring_delete(key: &CredentialKey) -> Result<()> {
    let entry = keyring_entry(key)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(backend_err(e).into()),
    }
}

fn backend_err(e: keyring::Error) -> CredentialStoreError {
    CredentialStoreError::Backend {
        source: Box::new(e),
    }
}

// --- env backend ---------------------------------------------------------

fn env_static_credential() -> Option<StoredCredential> {
    std::env::var(ENV_TOKEN).ok().and_then(|v| {
        if v.is_empty() {
            None
        } else {
            Some(StoredCredential::static_bearer(SecretToken::new(v)))
        }
    })
}

// --- backend selection helpers -------------------------------------------

fn default_root() -> Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| CredentialStoreError::Unavailable {
        reason: "could not resolve $XDG_CONFIG_HOME / config directory".to_owned(),
    })?;
    Ok(base.join("cosmon-remote"))
}

fn env_nonempty(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty())
}

/// Emit a one-line operator warning — **once per process** — that the static
/// env bearer is shadowing any stored credential and disables token refresh.
///
/// `$COSMON_REMOTE_TOKEN` set in a shell rc silently wins the backend-selection
/// precedence (step 1) over an otherwise-valid keyring/file credential, turning
/// a refreshable identity into a static bearer with no other signal (adversarial
/// review F4 #2). This is that signal. It is best-effort UX (like the browser-URL
/// prints in `oidc`), never on a hot path, and deduplicated via [`Once`] so a
/// process that probes the backend several times warns at most once.
///
/// [`Once`]: std::sync::Once
fn warn_env_backend_shadows() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        let name = ENV_TOKEN;
        eprintln!(
            "cosmon-remote: using the static bearer from ${name}; any stored keyring/file \
             credential is ignored and token refresh is disabled (unset ${name} to use \
             stored credentials)."
        );
    });
}

fn backend_override() -> Result<Option<BackendKind>> {
    match std::env::var(ENV_CRED_BACKEND) {
        Ok(v) if v.is_empty() => Ok(None),
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "keyring" => Ok(Some(BackendKind::Keyring)),
            "file" => Ok(Some(BackendKind::File)),
            other => Err(CredentialStoreError::Unavailable {
                reason: format!("unknown {ENV_CRED_BACKEND}={other:?}; accepted: keyring, file"),
            }
            .into()),
        },
        Err(_) => Ok(None),
    }
}

/// Resolve the selectable persistent backend without performing I/O.
///
/// This narrow seam keeps the musl safety rule testable: a `keyring` override
/// must not select the keyring crate's non-persistent mock backend when this
/// target did not compile a native keyring implementation.
fn resolve_backend_kind(
    backend_override: Option<BackendKind>,
    keyring_available: bool,
    native_keyring_supported: bool,
) -> Result<BackendKind> {
    match backend_override {
        Some(BackendKind::Keyring) if !native_keyring_supported => {
            Err(CredentialStoreError::Unavailable {
                reason: format!(
                    "{ENV_CRED_BACKEND}=keyring is unavailable on this target; use file instead"
                ),
            }
            .into())
        }
        Some(kind) => Ok(kind),
        None if keyring_available => Ok(BackendKind::Keyring),
        None => Ok(BackendKind::File),
    }
}

/// Whether this target includes a native, cross-process keyring backend.
///
/// The `keyring` crate falls back to a mock store when no feature selects a
/// platform implementation. That store is intentionally process-local, so it
/// cannot satisfy cosmon's refresh-credential persistence contract.
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "musl"))
))]
fn native_keyring_backend_supported() -> bool {
    true
}

/// See the supported-target implementation above.
#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "musl"))
)))]
fn native_keyring_backend_supported() -> bool {
    false
}

/// Probe for keyring reachability — bounded, never the open-ended 25-second
/// Secret-Service connect (C3, kahneman-F1).
///
/// - Linux: the Secret Service rides the D-Bus session bus. Two stages, cheap
///   before expensive:
///   1. **cheap env filter** — if neither `$DBUS_SESSION_BUS_ADDRESS` nor
///      `$XDG_RUNTIME_DIR/bus` is present the box is headless → `false`
///      immediately, no D-Bus traffic at all.
///   2. **bounded real reachability** — a session bus is *not* a Secret-Service
///      provider: `systemd --user`, most containers, and SSH + `pam_systemd`
///      create `$XDG_RUNTIME_DIR/bus` with **no** owner of
///      `org.freedesktop.secrets`. Trusting the bus's mere presence (the old
///      heuristic) committed to Keyring and then errored — or stalled ~25s on
///      D-Bus activation — on the first `load`/`store` (adversarial review F1).
///      So do one lightweight round-trip against the service, capped by
///      [`KEYRING_PROBE_TIMEOUT`] on a scratch thread that is *abandoned* (not
///      joined) on timeout. Commit to Keyring only if a live provider actually
///      answers within the budget.
/// - macOS / Windows: the native keychain is always available.
/// - Other targets: no supported backend → `false`.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn keyring_backend_available() -> bool {
    if !session_bus_present() {
        return false;
    }
    matches!(
        run_bounded(KEYRING_PROBE_TIMEOUT, keyring_secret_service_answers),
        Some(true)
    )
}

/// Cheap negative filter: is *any* D-Bus session bus reachable at all? A `false`
/// here means a headless box with no bus — skip the probe entirely.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn session_bus_present() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return true;
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Path::new(&runtime).join("bus").exists();
    }
    false
}

/// One lightweight Secret-Service round-trip used purely as a liveness probe.
///
/// Returns `true` iff a provider owns `org.freedesktop.secrets` and answers.
/// A `NoEntry` reply counts as **reachable** — the service responded, the
/// sentinel slot is simply empty (this is the "name-has-owner"-equivalent
/// signal). Any other error means no live provider (unowned name, transport
/// failure) → `false`. Never writes; reads a sentinel account only.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn keyring_secret_service_answers() -> bool {
    let entry = match keyring::Entry::new(KEYRING_SERVICE, KEYRING_PROBE_ACCOUNT) {
        Ok(e) => e,
        Err(_) => return false,
    };
    match entry.get_password() {
        Ok(_) => true,
        Err(keyring::Error::NoEntry) => true,
        Err(_) => false,
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keyring_backend_available() -> bool {
    true
}

#[cfg(any(
    all(target_os = "linux", target_env = "musl"),
    not(any(target_os = "linux", target_os = "macos", target_os = "windows"))
))]
fn keyring_backend_available() -> bool {
    false
}

/// Sentinel keyring account read by the reachability probe. It never holds a
/// real secret; a `NoEntry` reply against it is the "service is alive" signal.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
const KEYRING_PROBE_ACCOUNT: &str = "__cosmon_reachability_probe__";

/// Upper bound on the reachability probe. Long enough for a slow-but-present
/// Secret Service to answer (a local D-Bus round-trip is sub-millisecond),
/// short enough that a wedged D-Bus *activation* never reproduces the ~25s hang
/// the probe exists to prevent. Keeps the degrade path under the synthesis's
/// "<1s fallback" guard (delib-20260710-33b7).
// Gated to exactly its only user, `keyring_backend_available` (Linux-only).
// The broader `any(…, test)` gate left the const compiled-but-unused under a
// non-Linux `test` build (the D-Bus probe fn is absent there), which
// `clippy --all-targets` flags as a dead constant on macOS/Windows CI.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
const KEYRING_PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// Run `probe` on a scratch thread and wait at most `timeout` for its result.
///
/// Returns `Some(v)` if it finished in time, `None` on timeout. On timeout the
/// worker thread is **abandoned** (detached), never joined — the whole point is
/// to never block on a wedged D-Bus / Secret-Service call. A leaked thread
/// stuck on a dead socket is reaped when the process exits; that bounded-hang
/// tradeoff *is* the kahneman-F1 guarantee. `probe` must not have observable
/// side effects that outlive it (the reachability probe only reads a sentinel).
#[cfg(any(all(target_os = "linux", not(target_env = "musl")), test))]
pub(crate) fn run_bounded<T, F>(timeout: Duration, probe: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The receiver may already be gone (we timed out) — that send error is
        // expected and ignored; the thread then simply exits.
        let _ = tx.send(probe());
    });
    rx.recv_timeout(timeout).ok()
}

// --- filesystem primitives (0600, O_NOFOLLOW, fstat) ---------------------

/// Write `bytes` to a freshly created 0600 temp file and fsync it to disk.
fn write_tmp(tmp: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = open_new_0600(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// fsync `dir` so a directory-entry change (the `rename` in [`CredentialStore::file_store`])
/// is flushed to stable storage. Opening a directory read-only and calling
/// `fsync` on the fd is the POSIX-portable way to persist the entry; the caller
/// treats a failure as non-fatal (the rename already took effect). No-op on
/// non-unix, where opening a directory as a `File` is not portable and platform
/// stores own their own durability.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn open_new_0600(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL — refuses to follow a planted file
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)
        .map_err(Error::Io)
}

#[cfg(not(unix))]
fn open_new_0600(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(Error::Io)
}

#[cfg(unix)]
fn open_read(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(Error::Io)
}

#[cfg(not(unix))]
fn open_read(path: &Path) -> Result<File> {
    OpenOptions::new().read(true).open(path).map_err(Error::Io)
}

/// A best-effort create/open of the sidecar lockfile (0600 on unix). The
/// lockfile carries no secret — only the advisory lock — so following an
/// existing regular one is harmless; we still keep it tight.
///
/// **Symlink / FIFO hardening (F3).** Unlike its `create(true)` past, this
/// helper never follows a symlink and never blocks on a planted FIFO. An
/// attacker with write access to the `0700 credentials/` directory could
/// otherwise plant `<storage_id>.lock` as a symlink to a reader-less FIFO;
/// a `write(true)` open would then block *indefinitely*, hanging the very
/// module whose reason to exist is "never hang on the credential path" —
/// or, following a symlink to a victim-owned path, create/touch it.
///
/// We mirror the `create_new`-then-open-existing hardening of
/// [`open_new_0600`] / [`open_read`]:
/// 1. `create_new` (`O_CREAT | O_EXCL`) + `O_NOFOLLOW` — win the race by
///    creating a fresh regular file, refusing to clobber or follow any
///    planted entry.
/// 2. On `AlreadyExists` (the legitimate case: a peer created the lockfile
///    first), re-open the existing path with `O_NOFOLLOW | O_NONBLOCK` so a
///    symlink is rejected (`ELOOP`) and a FIFO open never blocks, then
///    `fstat` the fd and reject anything that is not a regular file.
#[cfg(unix)]
fn open_lock(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL — refuses to follow/clobber a plant
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // O_NONBLOCK guarantees the open of a planted reader-less FIFO
            // returns immediately instead of blocking; O_NOFOLLOW rejects a
            // symlink. flock semantics are unaffected (advisory locking is
            // governed by LOCK_NB, not the fd's O_NONBLOCK status).
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .map_err(Error::Io)?;
            ensure_regular_lockfile(&file, path)?;
            Ok(file)
        }
        Err(e) => Err(Error::Io(e)),
    }
}

/// Reject a lockfile that is not a regular file. A planted FIFO, socket, or
/// device node at the lock path could hang the open or subvert the advisory
/// lock; we detect it via `fstat` on the already-open fd (TOCTOU-free — the
/// check binds to the very inode we hold, not to a fresh path lookup).
#[cfg(unix)]
fn ensure_regular_lockfile(file: &File, path: &Path) -> Result<()> {
    if !file.metadata()?.file_type().is_file() {
        return Err(CredentialStoreError::InsecurePermissions {
            path: path.display().to_string(),
        }
        .into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn open_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(Error::Io)
}

/// Verify (via fstat on the open fd, not a fresh `stat`) that the credential
/// file grants no group/other permission bits. On non-unix this is a no-op
/// (Windows relies on the native Cred Manager / NTFS ACLs).
#[cfg(unix)]
fn check_permissions(file: &File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(CredentialStoreError::InsecurePermissions {
            path: path.display().to_string(),
        }
        .into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_file: &File, _path: &Path) -> Result<()> {
    Ok(())
}

/// Tighten the `credentials/` directory to 0700 on unix (best-effort).
#[cfg(unix)]
fn harden_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(dir)?.permissions();
    if perms.mode() & 0o077 != 0 {
        perms.set_mode(0o700);
        fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_dir(_dir: &Path) -> Result<()> {
    Ok(())
}
