// SPDX-License-Identifier: AGPL-3.0-only

//! JSON filesystem backend for the Cosmon `StateStore` trait.
//!
//! Layout:
//! ```text
//! <root>/
//!   fleet.json           (durable: who exists, what they own — tracked or
//!                         shared across residences)
//!   fleet.runtime.json   (live: host-specific worker repo paths, patrol
//!                         respawn counts — gitignored, never crosses a
//!                         residence boundary)
//!   ops/molecules/{id}/state.json
//! ```
//!
//! All writes are atomic: data is written to a `.tmp` sibling and then renamed
//! into place, so a crash mid-write never corrupts the primary file.
//!
//! # Durable / runtime split
//!
//! The fleet snapshot is a union of two kinds of fact: durable cognitive state
//! (what the operator has decided — worker identities, roles, assigned
//! molecules, freeze/thaw intent) and runtime process pointers (worktree paths,
//! patrol restart counters) that cannot survive a cross-host or cross-process
//! boundary. [`FileStore::save_fleet`] writes the first kind to `fleet.json`
//! and the second kind to `fleet.runtime.json`; [`FileStore::load_fleet`] reads
//! both and merges them back into the in-memory [`Fleet`]. A legacy
//! monolithic `fleet.json` (pre-split) still deserializes correctly because
//! the runtime fields remain `#[serde(default)]` on [`cosmon_state::WorkerData`]
//! — the next save splits them out.

#![forbid(unsafe_code)]

pub mod cas;
pub mod event;
pub mod presence_store;
pub mod resolve;

pub use presence_store::PresenceStore;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// Re-export the resolution function for convenience.
pub use resolve::{
    resolve_cluster_config_path, resolve_config_path, resolve_config_path_from,
    resolve_formulas_dir, resolve_formulas_dir_from, resolve_state_dir, resolve_state_dir_from,
    resolve_state_dir_with_origin, walk_up_find_cosmon_dir_from, StateDirOrigin,
};

use cosmon_core::config::ProjectConfig;
use cosmon_core::error::CosmonError;
use cosmon_core::id::{MoleculeId, ProjectId, WorkerId};
use cosmon_core::paths::CosmonPath;
use cosmon_state::{Fleet, FleetGuard, MoleculeData, MoleculeFilter, StateStore, TrunkGuard};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// JSON-file-backed implementation of [`StateStore`].
#[derive(Debug, Clone)]
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    /// Create a new `FileStore` rooted at the given directory.
    ///
    /// The directory (and parents) will be created on the first write if missing.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Derive the project root from the state directory.
    ///
    /// The state directory is `.cosmon/state/`, so the project root is two
    /// levels up. Returns `None` if the ancestry chain is too short (e.g.,
    /// in test environments with a flat temp directory).
    #[must_use]
    pub fn project_root(&self) -> Option<PathBuf> {
        // .cosmon/state/ → .cosmon/ → project root
        self.root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    fn fleet_path(&self) -> PathBuf {
        // Decoded from the write-path taxonomy (B7 collapse,
        // delib-20260607-aec8) — `self.root` is the state root, so the layout
        // is never re-stated here.
        self.root.join(CosmonPath::Fleet.rel())
    }

    /// Path to the runtime-only overlay (`fleet.runtime.json`).
    ///
    /// Holds worker fields whose meaning is tied to the current host's process
    /// table — currently `repo` (worktree path) and `restart_count` (patrol
    /// respawn counter). Gitignored; absent by default on cold clone.
    fn fleet_runtime_path(&self) -> PathBuf {
        self.root.join(CosmonPath::FleetRuntime.rel())
    }

    /// Fleet-scoped molecule directory: `fleets/{fleet}/molecules/{id}/`.
    #[must_use]
    pub fn molecule_dir(&self, id: &MoleculeId) -> PathBuf {
        // Search across all fleet directories to find this molecule.
        // This allows load_molecule to work without knowing the fleet.
        let fleets_root = self.fleets_root();
        if fleets_root.is_dir() {
            if let Ok(entries) = fs::read_dir(&fleets_root) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join("molecules").join(id.as_str());
                    if candidate.is_dir() {
                        return candidate;
                    }
                }
            }
        }
        // Fallback: legacy flat layout or new default fleet.
        let legacy = self.root.join("ops/molecules").join(id.as_str());
        if legacy.is_dir() {
            return legacy;
        }
        // Default to "default" fleet for new molecules.
        fleets_root
            .join("default")
            .join("molecules")
            .join(id.as_str())
    }

    fn molecule_path(&self, id: &MoleculeId) -> PathBuf {
        self.molecule_dir(id).join("state.json")
    }

    fn fleets_root(&self) -> PathBuf {
        self.root.join("fleets")
    }

    fn molecules_root(&self) -> PathBuf {
        self.root.join("ops/molecules")
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(CosmonPath::FleetLock.rel())
    }

    /// Path to the trunk-write lock file (ADR-110 Phase 1 /
    /// invariant **I1 WRITER-UNIQUE**).
    ///
    /// Distinct from [`Self::lock_path`] (`fleet.lock`) — that one serialises
    /// fleet-state JSON writes; this one serialises *git-trunk* writes (merge
    /// onto main, post-merge hook, frontier write) so that two concurrent
    /// `cs done` processes never race on the shared cosmon main checkout.
    ///
    /// Sibling of `fleet.lock` under `<state_dir>/` so both share the same
    /// `.cosmon/state/.gitignore` perimeter and survive every legitimate
    /// state-dir relocation.
    fn trunk_lock_path(&self) -> PathBuf {
        self.root.join(CosmonPath::TrunkLock.rel())
    }

    /// Acquire an exclusive lock on the **cosmon main trunk** and return an
    /// RAII guard that releases the lock on drop.
    ///
    /// Models invariant **I1 WRITER-UNIQUE** from ADR-110: at any instant
    /// *at most one* worker holds the write token
    /// on the cosmon `main` branch. Operations that must wrap themselves in
    /// this lock include `cs done` (merge), `cs stitch <root>` (DAG-respecting
    /// landing), `cs land` (future rename of `cs done`), and any future
    /// command that performs `git switch` / `git merge` / `git push` on the
    /// trunk checkout.
    ///
    /// # Lock order (deadlock-freedom)
    ///
    /// The trunk lock is the **outer** lock. The fleet lock
    /// ([`Self::with_fleet_lock`]) is **inner**: any code path that needs both
    /// acquires `trunk` first, never the reverse. `cs stitch` holds the trunk
    /// lock **alone** (it rewrites no fleet/molecule state), and `cs done`
    /// drops the trunk lock *before* its terminal fleet-purge — so no path
    /// ever holds `fleet ⊃ trunk`. This is the total lock order proven
    /// deadlock-free by the TLA+ model `smithy/docs/formal/MCStitch.tla`
    /// (Coffman *circular-wait* broken by a single global order). The
    /// `MCStitchDeadlock.cfg` config documents the inversion this avoids.
    ///
    /// # Concurrency semantics
    ///
    /// 1. Try a *non-blocking* acquire ([`fs2::FileExt::try_lock_exclusive`]).
    /// 2. If another process holds the lock:
    ///    - Read the holder hint from the lock file (PID + command label) and
    ///      print a single line to stderr so the operator can see what they're
    ///      waiting on.
    ///    - When `COSMON_TRUNK_LOCK_NONBLOCKING=1` is set, return
    ///      [`CosmonError::LockFailed`] immediately — used by integration
    ///      tests and operator scripts that want fast-fail with a retry hint.
    ///    - Otherwise block via [`fs2::FileExt::lock_exclusive`] until the
    ///      holder releases (RAII drop on the holder's [`File`]).
    /// 3. Once acquired, rewrite the lock file with our own holder hint so a
    ///    third process arriving mid-flight can report *us* as the writer.
    ///
    /// # Holder hint format
    ///
    /// Plain text, one line per field (`pid=...`, `cmd=...`, `started_at=...`,
    /// `host=...`). Intentionally not JSON: the file is short, human-greppable
    /// from `cat .cosmon/state/trunk.lock`, and survives partial reads on a
    /// stale lock (e.g. a previous holder that crashed before the OS released
    /// the advisory lock).
    ///
    /// # Errors
    ///
    /// - [`CosmonError::LockFailed`] if the lock cannot be acquired (either
    ///   because `COSMON_TRUNK_LOCK_NONBLOCKING=1` was set and the lock is
    ///   held, or because the underlying flock syscall fails for a reason
    ///   other than contention).
    pub fn acquire_trunk_lock(&self, cmd_hint: &str) -> Result<TrunkLockGuard, CosmonError> {
        let path = self.trunk_lock_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| CosmonError::LockFailed {
                path: path.display().to_string(),
                reason: format!("failed to create lock directory: {e}"),
            })?;
        }

        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| CosmonError::LockFailed {
                path: path.display().to_string(),
                reason: format!("failed to open lock file: {e}"),
            })?;

        match lock_file.try_lock_exclusive() {
            Ok(()) => {
                // Acquired without contention — fall through to holder-info write.
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Lock is held by another process. Read its holder hint
                // (best-effort) so we can tell the operator *who* they're
                // waiting on, then either fast-fail (env override) or block.
                let holder =
                    read_trunk_lock_holder(&path).unwrap_or_else(|| "<unknown>".to_owned());
                if std::env::var("COSMON_TRUNK_LOCK_NONBLOCKING").as_deref() == Ok("1") {
                    return Err(CosmonError::LockFailed {
                        path: path.display().to_string(),
                        reason: format!(
                            "trunk lock held by {holder}; retry once it releases or unset COSMON_TRUNK_LOCK_NONBLOCKING"
                        ),
                    });
                }
                eprintln!("cosmon: waiting for trunk lock held by {holder} (cs cmd: {cmd_hint})",);
                lock_file
                    .lock_exclusive()
                    .map_err(|e| CosmonError::LockFailed {
                        path: path.display().to_string(),
                        reason: format!("failed to acquire trunk lock: {e}"),
                    })?;
            }
            Err(e) => {
                return Err(CosmonError::LockFailed {
                    path: path.display().to_string(),
                    reason: format!("failed to probe trunk lock: {e}"),
                });
            }
        }

        // Stamp our own holder info so a third arrival sees who's writing.
        // Non-fatal: a write failure here only weakens observability, not
        // correctness — the advisory lock is already held.
        let _ = write_trunk_lock_holder(&path, cmd_hint);

        Ok(TrunkLockGuard {
            file: Some(lock_file),
            path,
        })
    }

    /// Closure-wrapping convenience over [`Self::acquire_trunk_lock`].
    ///
    /// The error type `E` is generic so callers can return `CosmonError`,
    /// `anyhow::Error`, or any other type constructible from `CosmonError`.
    /// Any error returned by `f` is propagated unchanged; the lock is
    /// always released via the [`TrunkLockGuard`] RAII drop on the way out.
    ///
    /// # Errors
    ///
    /// Returns `E` if the trunk lock cannot be acquired (via
    /// `From<CosmonError>` — see [`Self::acquire_trunk_lock`]) or if `f`
    /// itself returns an error.
    pub fn with_trunk_lock<F, T, E>(&self, cmd_hint: &str, f: F) -> Result<T, E>
    where
        F: FnOnce(&Self) -> Result<T, E>,
        E: From<CosmonError>,
    {
        let _guard = self.acquire_trunk_lock(cmd_hint)?;
        f(self)
    }

    /// Acquire an exclusive lock on fleet state and run a read-modify-write cycle.
    ///
    /// The lock is held for the duration of `f`. This prevents concurrent `cs`
    /// commands from clobbering each other's state changes. Uses `flock` (advisory
    /// file lock) which is safe across processes on the same machine.
    ///
    /// The error type `E` is generic so callers can return `CosmonError`,
    /// `anyhow::Error`, or any other type that can be constructed from a
    /// `CosmonError` (lock acquisition failure).
    ///
    /// # Errors
    ///
    /// Returns `E` if the lock cannot be acquired (via `From<CosmonError>`) or
    /// if `f` fails.
    pub fn with_fleet_lock<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Self) -> Result<T, E>,
        E: From<CosmonError>,
    {
        // Hold the guard for the duration of `f`; it releases the flock on
        // drop at the end of this scope.
        let _guard = self.acquire_fleet_lock()?;
        f(self)
    }

    /// Acquire an exclusive lock on fleet state and return an RAII guard.
    ///
    /// The lock is held until the returned [`FleetLockGuard`] drops. This is
    /// the guard-form primitive behind both the [`Self::with_fleet_lock`]
    /// closure convenience and the object-safe
    /// [`cosmon_state::StateStore::lock_fleet`] port method (ADR-131
    /// Decision 2). Uses `flock` (advisory file lock), safe across processes on
    /// the same machine.
    ///
    /// The fleet lock is the **inner** lock in the trunk ⊃ fleet order (see
    /// [`Self::acquire_trunk_lock`] § lock order).
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] if the lock directory or file cannot
    /// be created, or the `flock` syscall fails.
    pub fn acquire_fleet_lock(&self) -> Result<FleetLockGuard, CosmonError> {
        // Ensure the lock directory exists.
        if let Some(parent) = self.lock_path().parent() {
            fs::create_dir_all(parent).map_err(|e| CosmonError::StateStore {
                reason: format!("failed to create lock directory: {e}"),
            })?;
        }
        let lock_file = File::create(self.lock_path()).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to create lock file: {e}"),
        })?;
        lock_file
            .lock_exclusive()
            .map_err(|e| CosmonError::StateStore {
                reason: format!("failed to acquire fleet lock: {e}"),
            })?;
        Ok(FleetLockGuard {
            file: Some(lock_file),
        })
    }
}

/// RAII guard returned by [`FileStore::acquire_fleet_lock`]. The advisory
/// `flock` on `fleet.lock` is released when the guard drops.
///
/// The **inner** lock in the trunk ⊃ fleet order (see
/// [`FileStore::acquire_trunk_lock`] § lock order). Implements
/// [`cosmon_state::FleetGuard`] so it can be returned through the object-safe
/// [`cosmon_state::StateStore::lock_fleet`] port (ADR-131 Decision 2).
#[must_use = "fleet lock is released when the guard drops; bind to a local to keep it"]
#[derive(Debug)]
pub struct FleetLockGuard {
    /// `None` only during drop.
    file: Option<File>,
}

impl Drop for FleetLockGuard {
    fn drop(&mut self) {
        // The advisory flock is released when the file handle drops.
        drop(self.file.take());
    }
}

impl FleetGuard for FleetLockGuard {}

impl TrunkGuard for TrunkLockGuard {}

/// RAII guard returned by [`FileStore::acquire_trunk_lock`]. The lock is
/// released — and the holder hint cleared — when the guard drops.
///
/// Held by `cs done`, `cs stitch <root>` (and future `cs land`) for the
/// duration of any operation that mutates the cosmon main checkout (merge
/// onto main, post-merge hook, frontier write). It is the **outer** lock in
/// the trunk ⊃ fleet order (see [`FileStore::acquire_trunk_lock`] § lock
/// order).
#[must_use = "trunk lock is released when the guard drops; bind to a local to keep it"]
#[derive(Debug)]
pub struct TrunkLockGuard {
    /// `None` only during drop.
    file: Option<File>,
    path: PathBuf,
}

impl Drop for TrunkLockGuard {
    fn drop(&mut self) {
        // Best-effort: clear the holder hint *before* releasing the flock,
        // so a third process that arrives just after we drop the file sees
        // an empty holder rather than a stale "pid=12345 (cs done X)" line.
        let _ = clear_trunk_lock_holder(&self.path);
        // The flock is released when `self.file` drops below.
        drop(self.file.take());
    }
}

/// Write a holder hint into the trunk lock file (truncating any prior
/// content). Best-effort — failures are not fatal because the load-bearing
/// guarantee is the advisory `flock`, not the on-disk hint.
fn write_trunk_lock_holder(path: &Path, cmd_hint: &str) -> std::io::Result<()> {
    let pid = std::process::id();
    let host = hostname_best_effort();
    let now = chrono::Utc::now().to_rfc3339();
    let body = format!("pid={pid}\ncmd={cmd_hint}\nstarted_at={now}\nhost={host}\n");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()
}

/// Truncate the holder hint on release. Best-effort.
fn clear_trunk_lock_holder(path: &Path) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.write_all(b"")?;
    f.sync_all()
}

/// Read the current holder hint as a single human-readable line. Returns
/// `None` if the file is empty / unreadable / malformed — the caller falls
/// back to a generic `<unknown>` label so the wait message still surfaces.
fn read_trunk_lock_holder(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut pid = None;
    let mut cmd = None;
    for line in trimmed.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            pid = Some(v.to_owned());
        } else if let Some(v) = line.strip_prefix("cmd=") {
            cmd = Some(v.to_owned());
        }
    }
    match (pid, cmd) {
        (Some(p), Some(c)) => Some(format!("pid {p} ({c})")),
        (Some(p), None) => Some(format!("pid {p}")),
        (None, Some(c)) => Some(c),
        (None, None) => Some(trimmed.to_owned()),
    }
}

fn hostname_best_effort() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Load the project configuration from `.cosmon/config.toml`.
///
/// Returns the default configuration if the file does not exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be parsed.
pub fn load_project_config(config_path: &Path) -> Result<ProjectConfig, CosmonError> {
    if !config_path.exists() {
        return Ok(ProjectConfig::default());
    }
    let content = fs::read_to_string(config_path).map_err(|e| CosmonError::StateStore {
        reason: format!("failed to read config: {e}"),
    })?;
    ProjectConfig::parse(&content).map_err(|e| CosmonError::StateStore {
        reason: format!("failed to parse config.toml: {e}"),
    })
}

/// Load the project configuration and require a valid `project_id`.
///
/// Combines [`load_project_config`] with [`ProjectConfig::require_project_id`]
/// in a single call. This is the canonical entry point for any command that
/// needs the project identity — there is no silent fallback.
///
/// # Errors
///
/// Returns an error if the config file cannot be parsed or if `project_id`
/// is missing from the `[project]` section.
pub fn resolve_project_id(config_path: &Path) -> Result<ProjectId, CosmonError> {
    let config = load_project_config(config_path)?;
    config
        .require_project_id()
        .cloned()
        .map_err(|reason| CosmonError::StateStore { reason })
}

/// Resolve a fleet-scoped tmux socket name from the project `config.toml`.
///
/// Returns the configured `project_id` when present (already unique per
/// project — format `<dirname>-<hash4>`). For legacy projects without an
/// explicit `project_id` the socket is derived from the project root via
/// [`ProjectId::generate`], so two uninitialized cosmon trees on the same
/// host never share a tmux server. A generic `"cosmon"` is returned only
/// when the project root cannot be located at all (e.g. running outside
/// a `.cosmon/` tree).
///
/// This is the enforcement point for the sibling-isolation invariant:
/// every cosmon invocation passes
/// `-L <fleet-socket>` to tmux so `tmux ls` on the default server
/// shows nothing.
#[must_use]
pub fn resolve_tmux_socket_name(config_path: &Path) -> String {
    if let Ok(cfg) = load_project_config(config_path) {
        if let Some(pid) = cfg.project.project_id {
            return pid.to_string();
        }
    }
    // Walk up from the config path to find the project root (parent of `.cosmon/`).
    let project_root = config_path
        .parent() // e.g. `.cosmon/`
        .and_then(Path::parent); // e.g. project root
    match project_root {
        Some(root) if !root.as_os_str().is_empty() => ProjectId::generate(root).to_string(),
        _ => "cosmon".to_owned(),
    }
}

/// Write `data` to `path` atomically via a `.tmp` sibling + rename.
///
/// Creates parent directories on demand. Visible to sibling modules in
/// this crate (e.g. [`PresenceStore`]) so every on-disk primitive
/// shares the same "no torn files" guarantee.
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> Result<(), CosmonError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Fleet durable / runtime split (task-20260420-90cf)
// ---------------------------------------------------------------------------

/// Per-worker runtime overlay: host-specific fields that cannot cross a
/// residence boundary (worktree path, patrol respawn counter).
#[derive(Debug, Default, Serialize, Deserialize)]
struct RuntimeWorker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    restart_count: u32,
}

/// Top-level shape of `fleet.runtime.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RuntimeFleet {
    #[serde(default)]
    workers: HashMap<WorkerId, RuntimeWorker>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Project a [`Fleet`] into the two on-disk halves.
///
/// Returns `(durable_json, runtime_json)` — ready for `atomic_write`. The
/// durable half keeps the full worker object minus the runtime keys; the
/// runtime half is a compact map `{ workers: { <id>: { repo, restart_count } } }`.
/// Workers with no non-default runtime data still appear in the runtime file
/// (as an empty object) only if they carry data worth persisting; an empty
/// overlay is serialized as `{"workers":{}}` so the file is always present
/// after the first write and round-trips cleanly.
fn split_fleet_for_persist(fleet: &Fleet) -> Result<(String, String), CosmonError> {
    let mut durable_val: serde_json::Value = serde_json::to_value(fleet)?;
    let mut runtime = RuntimeFleet::default();

    if let Some(workers) = durable_val
        .get_mut("workers")
        .and_then(serde_json::Value::as_object_mut)
    {
        for (worker_id_str, worker_val) in workers.iter_mut() {
            // Pre-parse the key so we can stash the overlay under a typed key.
            let wid = WorkerId::new(worker_id_str).map_err(|e| CosmonError::StateStore {
                reason: format!("invalid worker id in fleet.json: {e}"),
            })?;
            let repo = take_string_field(worker_val, "repo");
            let restart_count = take_u32_field(worker_val, "restart_count");
            if repo.is_some() || restart_count != 0 {
                runtime.workers.insert(
                    wid,
                    RuntimeWorker {
                        repo,
                        restart_count,
                    },
                );
            }
        }
    }

    let durable = serde_json::to_string_pretty(&durable_val)?;
    let runtime_json = serde_json::to_string_pretty(&runtime)?;
    Ok((durable, runtime_json))
}

/// Remove a string field from a JSON object and return it if present.
fn take_string_field(v: &mut serde_json::Value, key: &str) -> Option<String> {
    let obj = v.as_object_mut()?;
    match obj.remove(key)? {
        serde_json::Value::String(s) => Some(s),
        // Treat explicit `null` as "no repo" — legacy fleet.json files may
        // store it that way; the durable view should not re-serialize it.
        serde_json::Value::Null => None,
        other => {
            // Unexpected shape — preserve the value rather than silently
            // dropping it so a human can notice. Put it back.
            obj.insert(key.to_owned(), other);
            None
        }
    }
}

/// Remove a u32 field from a JSON object and return the value (default 0).
fn take_u32_field(v: &mut serde_json::Value, key: &str) -> u32 {
    let Some(obj) = v.as_object_mut() else {
        return 0;
    };
    match obj.remove(key) {
        Some(serde_json::Value::Number(n)) => {
            n.as_u64().and_then(|x| u32::try_from(x).ok()).unwrap_or(0)
        }
        // Unexpected shape — put it back and report 0.
        Some(other) => {
            obj.insert(key.to_owned(), other);
            0
        }
        None => 0,
    }
}

/// Overlay `fleet.runtime.json` (if present) onto `fleet`, setting each
/// worker's `repo` and `restart_count` from the runtime file. Missing file,
/// orphan worker ids, and parse errors on a partially written file are all
/// tolerated: the overlay is advisory — the worker's state.json and the
/// transport backend are authoritative for runtime liveness.
fn merge_runtime_overlay(fleet: &mut Fleet, path: &Path) -> Result<(), CosmonError> {
    if !path.exists() {
        return Ok(());
    }
    let data = fs::read_to_string(path)?;
    let runtime: RuntimeFleet = match serde_json::from_str(&data) {
        Ok(rt) => rt,
        Err(_) => return Ok(()), // ignore corrupt/partial files; next save rebuilds.
    };
    for (wid, rw) in runtime.workers {
        if let Some(worker) = fleet.workers.get_mut(&wid) {
            worker.repo = rw.repo;
            worker.restart_count = rw.restart_count;
        }
        // Orphan entries (runtime without durable counterpart) are ignored —
        // the operator-visible roster comes from fleet.json.
    }
    Ok(())
}

fn matches_filter(mol: &MoleculeData, filter: &MoleculeFilter) -> bool {
    if let Some(ref fleet) = filter.fleet {
        if mol.fleet_id != *fleet {
            return false;
        }
    }
    if let Some(ref kind) = filter.kind {
        if mol.kind.as_ref() != Some(kind) {
            return false;
        }
    }
    if let Some(ref status) = filter.status {
        if mol.status != *status {
            return false;
        }
    }
    if let Some(ref worker) = filter.worker {
        if mol.assigned_worker.as_ref() != Some(worker) {
            return false;
        }
    }
    if let Some(ref formula) = filter.formula {
        if mol.formula_id != *formula {
            return false;
        }
    }
    if let Some(ref project) = filter.project {
        if mol.project_id.as_ref() != Some(project) {
            return false;
        }
    }
    if let Some(ref text) = filter.search_text {
        let needle = text.to_lowercase();
        let mut found = false;
        // Search molecule ID
        if mol.id.as_str().to_lowercase().contains(&needle) {
            found = true;
        }
        // Search formula ID
        if !found && mol.formula_id.as_str().to_lowercase().contains(&needle) {
            found = true;
        }
        // Search fleet ID
        if !found && mol.fleet_id.as_str().to_lowercase().contains(&needle) {
            found = true;
        }
        // Search assigned worker
        if !found {
            if let Some(ref w) = mol.assigned_worker {
                if w.as_str().to_lowercase().contains(&needle) {
                    found = true;
                }
            }
        }
        // Search molecule kind (Debug gives PascalCase variant name)
        if !found {
            if let Some(kind) = mol.kind {
                if format!("{kind:?}").to_lowercase().contains(&needle) {
                    found = true;
                }
            }
        }
        // Search all variable values
        if !found {
            for v in mol.variables.values() {
                if v.to_lowercase().contains(&needle) {
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return false;
        }
    }
    if !filter.tag_globs.is_empty() {
        let any = filter
            .tag_globs
            .iter()
            .any(|pat| mol.tags.iter().any(|t| t.matches_glob(pat)));
        if !any {
            return false;
        }
    }
    true
}

impl StateStore for FileStore {
    fn load_fleet(&self) -> Result<Fleet, CosmonError> {
        let path = self.fleet_path();
        if !path.exists() {
            return Ok(Fleet::default());
        }
        let data = fs::read_to_string(&path)?;
        let mut fleet: Fleet = serde_json::from_str(&data)?;
        // Phase-1 WorkerRole migration (delib-20260414-2ab2): legacy
        // entries without `worker_role` default to `Cognition`; promote
        // them to `Runtime` when id prefix or `AgentRole::Runtime` say so.
        fleet.reconcile_worker_roles();
        // Overlay runtime fields from the live half when present (ADR note
        // in docs/architectural-invariants.md §fleet-runtime-split). A
        // legacy monolithic fleet.json still carries these fields inline,
        // so skipping the overlay is safe; the next save splits them out.
        merge_runtime_overlay(&mut fleet, &self.fleet_runtime_path())?;
        Ok(fleet)
    }

    fn save_fleet(&self, fleet: &Fleet) -> Result<(), CosmonError> {
        let (durable, runtime) = split_fleet_for_persist(fleet)?;
        atomic_write(&self.fleet_path(), durable.as_bytes())?;
        atomic_write(&self.fleet_runtime_path(), runtime.as_bytes())
    }

    fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
        let path = self.molecule_path(id);
        if !path.exists() {
            return Err(CosmonError::MoleculeNotFound(id.clone()));
        }
        let data = fs::read_to_string(&path)?;
        let mol: MoleculeData = serde_json::from_str(&data)?;
        Ok(mol)
    }

    fn save_molecule(&self, id: &MoleculeId, data: &MoleculeData) -> Result<(), CosmonError> {
        let json = serde_json::to_string_pretty(data)?;
        // Write to the fleet-scoped state path, decoded from the write-path
        // taxonomy (B7 collapse, delib-20260607-aec8) so the writer emits the
        // path rather than re-stating the `fleets/<fleet>/molecules/<id>/state.json`
        // layout beside the taxonomy.
        let path = self.root.join(
            CosmonPath::MoleculeState {
                fleet: &data.fleet_id,
                id,
            }
            .rel(),
        );
        atomic_write(&path, json.as_bytes())
    }

    fn list_molecules(&self, filter: &MoleculeFilter) -> Result<Vec<MoleculeData>, CosmonError> {
        let mut results = Vec::new();

        // Scan fleet-scoped directories: fleets/{fleet}/molecules/{id}/state.json
        let fleets_root = self.fleets_root();
        if fleets_root.is_dir() {
            for fleet_entry in fs::read_dir(&fleets_root)?.flatten() {
                let mols_dir = fleet_entry.path().join("molecules");
                if !mols_dir.is_dir() {
                    continue;
                }
                Self::scan_molecules_dir(&mols_dir, filter, &mut results)?;
            }
        }

        // Also scan legacy flat layout: ops/molecules/{id}/state.json
        let legacy_root = self.molecules_root();
        if legacy_root.is_dir() {
            Self::scan_molecules_dir(&legacy_root, filter, &mut results)?;
        }

        // Sort for deterministic output. `fs::read_dir` does not guarantee any
        // ordering (POSIX is silent, and macOS HFS+/APFS returns entries in
        // hash order). Without this sort, every `cs reconcile` would produce
        // a spurious diff on STATUS.md, breaking idempotency and polluting
        // git history with no-op reorderings.
        results.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

        Ok(results)
    }

    fn molecule_dir(&self, id: &MoleculeId) -> PathBuf {
        // Delegate to the inherent method so existing concrete callers
        // (`FileStore::molecule_dir`) keep working unchanged while the port
        // gains the same capability for `&dyn StateStore` handlers.
        FileStore::molecule_dir(self, id)
    }

    fn project_root(&self) -> Option<PathBuf> {
        // Delegate to the inherent method (same rationale as `molecule_dir`):
        // concrete `FileStore::project_root` callers are unchanged while the
        // port gains the capability for `&dyn StateStore` handlers.
        FileStore::project_root(self)
    }

    fn lock_fleet(&self) -> Result<Box<dyn FleetGuard + '_>, CosmonError> {
        // Delegate to the inherent `flock` guard (ADR-131 Decision 2): the
        // closure-bounded `with_fleet_lock` becomes lexical `let _g =
        // store.lock_fleet()?;` at the call sites, while the mechanism stays
        // the advisory file lock.
        Ok(Box::new(self.acquire_fleet_lock()?))
    }

    fn lock_trunk(&self, cmd_hint: &str) -> Result<Box<dyn TrunkGuard + '_>, CosmonError> {
        // Delegate to the inherent trunk guard (already RAII): the port form
        // returns it boxed behind `dyn TrunkGuard`.
        Ok(Box::new(self.acquire_trunk_lock(cmd_hint)?))
    }
}

impl FileStore {
    /// Scan a molecules directory and collect matching molecules.
    fn scan_molecules_dir(
        dir: &Path,
        filter: &MoleculeFilter,
        results: &mut Vec<MoleculeData>,
    ) -> Result<(), CosmonError> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let state_path = entry.path().join("state.json");
            if !state_path.exists() {
                continue;
            }
            let data = fs::read_to_string(&state_path)?;
            let mol: MoleculeData = serde_json::from_str(&data)?;
            if matches_filter(&mol, filter) {
                results.push(mol);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Path portability helpers
// ---------------------------------------------------------------------------

/// Make `path` relative to `base` if it is a descendant.
///
/// Like git, cosmon stores paths relative to the project root so that
/// projects can be moved, renamed, or cloned without breaking state files.
/// If `path` is not under `base`, the original path string is returned
/// unchanged (graceful fallback for `--workdir /other/place`).
#[must_use]
pub fn make_relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base).map_or_else(
        |_| path.to_string_lossy().into_owned(),
        |rel| rel.to_string_lossy().into_owned(),
    )
}

/// Resolve a worker repo path that may be relative or absolute.
///
/// Relative paths are joined to `project_root`. Absolute paths (legacy
/// fleet.json files written before the portability fix) are returned as-is
/// for backward compatibility.
#[must_use]
pub fn resolve_repo_path(repo: &str, project_root: &Path) -> PathBuf {
    let p = Path::new(repo);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::AgentId;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::worker::WorkerStatus;
    use cosmon_state::{Fleet, MoleculeData, MoleculeFilter, RepoData, StateStore, WorkerData};
    use tempfile::TempDir;

    use super::*;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        (tmp, store)
    }

    fn sample_fleet() -> Fleet {
        let mut fleet = Fleet::default();
        let wid = WorkerId::new("w-1").unwrap();
        fleet.workers.insert(
            wid.clone(),
            WorkerData::new(
                wid,
                AgentId::new("agent-1").unwrap(),
                AgentRole::Implementation,
                Clearance::Write,
                WorkerStatus::Stopped,
            )
            .with_repo("cosmon"),
        );
        fleet.repos.insert(
            "cosmon".to_owned(),
            RepoData {
                name: "cosmon".to_owned(),
                path: "/tmp/cosmon".to_owned(),
            },
        );
        fleet
    }

    fn mol_id(suffix: &str) -> MoleculeId {
        MoleculeId::new(format!("cs-20260401-{suffix}")).unwrap()
    }

    fn sample_molecule(suffix: &str, status: MoleculeStatus, worker: Option<&str>) -> MoleculeData {
        MoleculeData {
            id: mol_id(suffix),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("formula-1").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: worker.map(|w| WorkerId::new(w).unwrap()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 3,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn test_fleet_roundtrip() {
        let (_tmp, store) = make_store();
        let fleet = sample_fleet();
        store.save_fleet(&fleet).unwrap();
        let loaded = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), loaded.workers.len());
        assert_eq!(fleet.repos.len(), loaded.repos.len());
        assert!(loaded.workers.contains_key(&WorkerId::new("w-1").unwrap()));
    }

    #[test]
    fn test_molecule_roundtrip() {
        let (_tmp, store) = make_store();
        let mol = sample_molecule("abc1", MoleculeStatus::Running, Some("w-1"));
        let id = mol.id.clone();
        store.save_molecule(&id, &mol).unwrap();
        let loaded = store.load_molecule(&id).unwrap();
        assert_eq!(loaded.id, mol.id);
        assert_eq!(loaded.status, MoleculeStatus::Running);
        assert_eq!(loaded.assigned_worker, mol.assigned_worker);
    }

    #[test]
    fn test_list_molecules_filter_by_status() {
        let (_tmp, store) = make_store();
        let active = sample_molecule("aaaa", MoleculeStatus::Running, None);
        let completed = sample_molecule("bbbb", MoleculeStatus::Completed, None);
        let collapsed = sample_molecule("cccc", MoleculeStatus::Collapsed, None);
        store.save_molecule(&active.id, &active).unwrap();
        store.save_molecule(&completed.id, &completed).unwrap();
        store.save_molecule(&collapsed.id, &collapsed).unwrap();

        let filter = MoleculeFilter {
            status: Some(MoleculeStatus::Running),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, mol_id("aaaa"));
    }

    #[test]
    fn test_list_molecules_filter_by_worker() {
        let (_tmp, store) = make_store();
        let m1 = sample_molecule("dddd", MoleculeStatus::Running, Some("w-alpha"));
        let m2 = sample_molecule("eeee", MoleculeStatus::Running, Some("w-beta"));
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();

        let filter = MoleculeFilter {
            worker: Some(WorkerId::new("w-alpha").unwrap()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].assigned_worker.as_ref().unwrap().as_str(),
            "w-alpha"
        );
    }

    #[test]
    fn test_missing_directory_created() {
        let (_tmp, store) = make_store();
        let fleet = sample_fleet();
        store.save_fleet(&fleet).unwrap();
        assert!(store.fleet_path().exists());

        let mol = sample_molecule("deep", MoleculeStatus::Running, None);
        store.save_molecule(&mol.id, &mol).unwrap();
        assert!(store.molecule_path(&mol.id).exists());
    }

    #[test]
    fn test_atomic_write_survives_crash() {
        let (_tmp, store) = make_store();

        // Simulate a .tmp file left behind from a previous crashed write
        let mol_id = mol_id("crsh");
        let mol_dir = store.molecule_dir(&mol_id);
        fs::create_dir_all(&mol_dir).unwrap();
        let tmp_path = mol_dir.join("state.json.tmp");
        fs::write(&tmp_path, b"corrupted partial write").unwrap();

        // A new save should overwrite the stale .tmp and produce a valid file
        let mol = sample_molecule("crsh", MoleculeStatus::Running, None);
        store.save_molecule(&mol_id, &mol).unwrap();

        let loaded = store.load_molecule(&mol_id).unwrap();
        assert_eq!(loaded.id, mol_id);

        // The .tmp file should have been cleaned up by the rename
        assert!(!tmp_path.exists());
    }

    #[test]
    fn test_load_fleet_returns_default_when_missing() {
        let (_tmp, store) = make_store();
        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.is_empty());
        assert!(fleet.repos.is_empty());
    }

    #[test]
    fn test_load_molecule_not_found() {
        let (_tmp, store) = make_store();
        let id = mol_id("nope");
        let err = store.load_molecule(&id).unwrap_err();
        assert!(matches!(err, CosmonError::MoleculeNotFound(_)));
    }

    #[test]
    fn test_list_molecules_empty_dir() {
        let (_tmp, store) = make_store();
        let results = store.list_molecules(&MoleculeFilter::default()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_text_matches_variable_values() {
        let (_tmp, store) = make_store();
        let mut mol = sample_molecule("s001", MoleculeStatus::Running, None);
        mol.variables
            .insert("topic".to_owned(), "fix logging bug".to_owned());
        store.save_molecule(&mol.id, &mol).unwrap();

        let filter = MoleculeFilter {
            search_text: Some("logging".to_owned()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, mol_id("s001"));
    }

    #[test]
    fn test_search_text_matches_formula_id() {
        let (_tmp, store) = make_store();
        let mol = sample_molecule("s002", MoleculeStatus::Running, None);
        store.save_molecule(&mol.id, &mol).unwrap();

        let filter = MoleculeFilter {
            search_text: Some("formula-1".to_owned()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_text_matches_fleet_id() {
        let (_tmp, store) = make_store();
        let mol = sample_molecule("s003", MoleculeStatus::Running, None);
        store.save_molecule(&mol.id, &mol).unwrap();

        let filter = MoleculeFilter {
            search_text: Some("default".to_owned()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_text_matches_worker() {
        let (_tmp, store) = make_store();
        let mol = sample_molecule("s004", MoleculeStatus::Running, Some("w-polecat"));
        store.save_molecule(&mol.id, &mol).unwrap();

        let filter = MoleculeFilter {
            search_text: Some("polecat".to_owned()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_text_matches_kind() {
        use cosmon_core::kind::MoleculeKind;

        let (_tmp, store) = make_store();
        let mut mol = sample_molecule("s005", MoleculeStatus::Running, None);
        mol.kind = Some(MoleculeKind::Issue);
        store.save_molecule(&mol.id, &mol).unwrap();

        let filter = MoleculeFilter {
            search_text: Some("issue".to_owned()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_text_case_insensitive() {
        let (_tmp, store) = make_store();
        let mut mol = sample_molecule("s006", MoleculeStatus::Running, None);
        mol.variables
            .insert("topic".to_owned(), "URGENT Bug Fix".to_owned());
        store.save_molecule(&mol.id, &mol).unwrap();

        let filter = MoleculeFilter {
            search_text: Some("urgent".to_owned()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_text_no_match() {
        let (_tmp, store) = make_store();
        let mol = sample_molecule("s007", MoleculeStatus::Running, None);
        store.save_molecule(&mol.id, &mol).unwrap();

        let filter = MoleculeFilter {
            search_text: Some("nonexistent".to_owned()),
            ..Default::default()
        };
        let results = store.list_molecules(&filter).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_make_relative_descendant() {
        let base = Path::new("/projects/cosmon");
        let path = Path::new("/projects/cosmon/.worktrees/task-1234");
        assert_eq!(make_relative(path, base), ".worktrees/task-1234");
    }

    #[test]
    fn test_make_relative_not_descendant() {
        let base = Path::new("/projects/cosmon");
        let path = Path::new("/other/place");
        assert_eq!(make_relative(path, base), "/other/place");
    }

    #[test]
    fn test_resolve_repo_path_relative() {
        let root = Path::new("/projects/cosmon");
        let resolved = resolve_repo_path(".worktrees/task-1234", root);
        assert_eq!(
            resolved,
            PathBuf::from("/projects/cosmon/.worktrees/task-1234")
        );
    }

    #[test]
    fn test_resolve_repo_path_absolute_legacy() {
        let root = Path::new("/projects/cosmon");
        let resolved = resolve_repo_path("/absolute/old/path", root);
        assert_eq!(resolved, PathBuf::from("/absolute/old/path"));
    }

    #[test]
    fn test_project_root_from_state_dir() {
        // Simulate .cosmon/state/ layout
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();

        let store = FileStore::new(&state_dir);
        let root = store.project_root();
        assert_eq!(root.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn test_resolve_tmux_socket_name_uses_configured_project_id() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        std::fs::create_dir_all(&cosmon_dir).unwrap();
        let config_path = cosmon_dir.join("config.toml");
        std::fs::write(&config_path, "[project]\nproject_id = \"my-proj-abcd\"\n").unwrap();

        assert_eq!(resolve_tmux_socket_name(&config_path), "my-proj-abcd");
    }

    #[test]
    fn test_resolve_tmux_socket_name_derives_from_path_when_unconfigured() {
        // Two distinct project roots must resolve to distinct socket names,
        // preventing the sibling-isolation hazard surfaced in delib-20260414-6d73.
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        let cfg_a = tmp_a.path().join(".cosmon").join("config.toml");
        let cfg_b = tmp_b.path().join(".cosmon").join("config.toml");
        // Config files do not exist — exercises the fallback path.

        let a = resolve_tmux_socket_name(&cfg_a);
        let b = resolve_tmux_socket_name(&cfg_b);

        assert_ne!(a, b, "distinct project roots must map to distinct sockets");
        assert_ne!(a, "cosmon", "fallback must not be the shared 'cosmon' name");
        assert_ne!(b, "cosmon");
    }

    #[test]
    fn test_load_fleet_migrates_legacy_runtime_worker() {
        // A fleet.json produced by a pre-ADR-040 build has no `worker_role`
        // field on worker entries. `FileStore::load_fleet` must back-fill the
        // role from the id-prefix heuristic — this is the migration surface
        // promised by the phase-1 briefing, and it has to happen transparently
        // on every load (no human `cs reconcile` required).
        use cosmon_core::worker::WorkerRole;
        let (_tmp, store) = make_store();
        let fleet_path = store.fleet_path();
        // Hand-crafted legacy JSON with no `worker_role` in either entry.
        let legacy_json = r#"{
            "workers": {
                "runtime-mission-42ab": {
                    "id": "runtime-mission-42ab",
                    "agent_id": "runtime",
                    "role": "implementation",
                    "clearance": "write",
                    "status": "active",
                    "desired": "running",
                    "repo": null,
                    "current_molecule": null,
                    "updated_at": "2026-04-14T10:00:00Z"
                },
                "quartz-abcd": {
                    "id": "quartz-abcd",
                    "agent_id": "polecat",
                    "role": "implementation",
                    "clearance": "write",
                    "status": "active",
                    "desired": "running",
                    "repo": null,
                    "current_molecule": null,
                    "updated_at": "2026-04-14T10:00:00Z"
                }
            },
            "repos": {}
        }"#;
        std::fs::create_dir_all(store.root.as_path()).unwrap();
        std::fs::write(&fleet_path, legacy_json).unwrap();
        let fleet = store.load_fleet().unwrap();
        let rt = fleet
            .workers
            .get(&WorkerId::new("runtime-mission-42ab").unwrap())
            .expect("runtime worker present");
        let cog = fleet
            .workers
            .get(&WorkerId::new("quartz-abcd").unwrap())
            .expect("cognition worker present");
        assert_eq!(rt.worker_role, WorkerRole::Runtime);
        assert_eq!(cog.worker_role, WorkerRole::Cognition);
    }

    // ── fleet.json durable / fleet.runtime.json live split (task-20260420-90cf) ──

    fn fleet_with_runtime_fields() -> Fleet {
        let mut fleet = Fleet::default();
        let wid = WorkerId::new("w-split").unwrap();
        let mut worker = WorkerData::new(
            wid.clone(),
            AgentId::new("agent-1").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        )
        .with_repo(".worktrees/task-split");
        worker.restart_count = 3;
        fleet.workers.insert(wid, worker);
        fleet
    }

    #[test]
    fn test_save_fleet_writes_both_files() {
        let (_tmp, store) = make_store();
        store.save_fleet(&fleet_with_runtime_fields()).unwrap();
        assert!(
            store.fleet_path().exists(),
            "durable fleet.json must be written"
        );
        assert!(
            store.fleet_runtime_path().exists(),
            "runtime fleet.runtime.json must be written"
        );
    }

    #[test]
    fn test_durable_fleet_json_omits_runtime_fields() {
        let (_tmp, store) = make_store();
        store.save_fleet(&fleet_with_runtime_fields()).unwrap();
        let durable = fs::read_to_string(store.fleet_path()).unwrap();
        assert!(
            !durable.contains("\"repo\""),
            "fleet.json must not carry repo: {durable}"
        );
        assert!(
            !durable.contains("\"restart_count\""),
            "fleet.json must not carry restart_count: {durable}"
        );
    }

    #[test]
    fn test_runtime_fleet_json_carries_repo_and_restart() {
        let (_tmp, store) = make_store();
        store.save_fleet(&fleet_with_runtime_fields()).unwrap();
        let runtime = fs::read_to_string(store.fleet_runtime_path()).unwrap();
        assert!(runtime.contains("w-split"), "runtime missing id: {runtime}");
        assert!(runtime.contains(".worktrees/task-split"), "repo missing");
        assert!(
            runtime.contains("\"restart_count\""),
            "restart_count missing"
        );
    }

    #[test]
    fn test_fleet_roundtrip_preserves_runtime_fields() {
        let (_tmp, store) = make_store();
        let before = fleet_with_runtime_fields();
        store.save_fleet(&before).unwrap();
        let after = store.load_fleet().unwrap();
        let wid = WorkerId::new("w-split").unwrap();
        let worker = after.workers.get(&wid).expect("worker survived");
        assert_eq!(worker.repo.as_deref(), Some(".worktrees/task-split"));
        assert_eq!(worker.restart_count, 3);
    }

    #[test]
    fn test_load_fleet_tolerates_missing_runtime_file_cold_start() {
        // Legacy monolithic fleet.json with repo + restart_count inline —
        // classic pre-split layout. Loader must read the runtime fields
        // straight from fleet.json (serde defaults keep them in the struct)
        // without requiring fleet.runtime.json to exist.
        let (_tmp, store) = make_store();
        let legacy_json = r#"{
            "workers": {
                "quartz-legacy": {
                    "id": "quartz-legacy",
                    "agent_id": "polecat",
                    "role": "implementation",
                    "clearance": "write",
                    "status": "active",
                    "desired": "running",
                    "repo": ".worktrees/legacy-flavor",
                    "current_molecule": null,
                    "updated_at": "2026-04-14T10:00:00Z",
                    "restart_count": 7
                }
            },
            "repos": {}
        }"#;
        fs::create_dir_all(store.root.as_path()).unwrap();
        fs::write(store.fleet_path(), legacy_json).unwrap();
        assert!(!store.fleet_runtime_path().exists());

        let fleet = store.load_fleet().unwrap();
        let worker = fleet
            .workers
            .get(&WorkerId::new("quartz-legacy").unwrap())
            .unwrap();
        assert_eq!(worker.repo.as_deref(), Some(".worktrees/legacy-flavor"));
        assert_eq!(worker.restart_count, 7);
    }

    #[test]
    fn test_legacy_fleet_json_migrates_on_next_save() {
        // After a legacy monolithic load, the first save must split the
        // runtime fields out so the next residence-crossing snapshot is
        // already clean. Idempotent: repeating the save yields the same
        // two files.
        let (_tmp, store) = make_store();
        let legacy_json = r#"{
            "workers": {
                "quartz-legacy": {
                    "id": "quartz-legacy",
                    "agent_id": "polecat",
                    "role": "implementation",
                    "clearance": "write",
                    "status": "active",
                    "desired": "running",
                    "repo": ".worktrees/legacy",
                    "current_molecule": null,
                    "updated_at": "2026-04-14T10:00:00Z",
                    "restart_count": 2
                }
            },
            "repos": {}
        }"#;
        fs::create_dir_all(store.root.as_path()).unwrap();
        fs::write(store.fleet_path(), legacy_json).unwrap();

        let fleet = store.load_fleet().unwrap();
        store.save_fleet(&fleet).unwrap();

        let durable = fs::read_to_string(store.fleet_path()).unwrap();
        let runtime = fs::read_to_string(store.fleet_runtime_path()).unwrap();
        assert!(!durable.contains("\"repo\""), "repo leaked to durable half");
        assert!(
            !durable.contains("\"restart_count\""),
            "restart_count leaked to durable half"
        );
        assert!(runtime.contains(".worktrees/legacy"));
        assert!(runtime.contains("\"restart_count\""));

        // Idempotence: save again, byte-identical durable output.
        let durable_before = durable;
        store.save_fleet(&fleet).unwrap();
        let durable_after = fs::read_to_string(store.fleet_path()).unwrap();
        assert_eq!(durable_before, durable_after);
    }

    #[test]
    fn test_orphan_runtime_file_alone_yields_empty_fleet() {
        // fleet.runtime.json without a durable counterpart is meaningless —
        // the operator roster lives in fleet.json. Load must ignore the
        // orphan overlay and return a default (empty) Fleet.
        let (_tmp, store) = make_store();
        fs::create_dir_all(store.root.as_path()).unwrap();
        let runtime_only = r#"{
            "workers": {
                "ghost-worker": {
                    "repo": ".worktrees/ghost",
                    "restart_count": 99
                }
            }
        }"#;
        fs::write(store.fleet_runtime_path(), runtime_only).unwrap();
        assert!(!store.fleet_path().exists());

        let fleet = store.load_fleet().unwrap();
        assert!(
            fleet.workers.is_empty(),
            "orphan runtime must not resurrect"
        );
    }

    #[test]
    fn test_runtime_overlay_ignores_unknown_worker_ids() {
        // If the runtime file mentions a worker id that no longer exists
        // in fleet.json (e.g. a worker removed from the durable narrative),
        // the overlay must be silently skipped for that id — not a hard
        // error, since the runtime file is advisory.
        let (_tmp, store) = make_store();
        store.save_fleet(&fleet_with_runtime_fields()).unwrap();

        // Inject an orphan runtime entry next to the real one.
        let stale_runtime = r#"{
            "workers": {
                "w-split": {
                    "repo": ".worktrees/task-split",
                    "restart_count": 3
                },
                "ghost-worker": {
                    "repo": ".worktrees/ghost",
                    "restart_count": 10
                }
            }
        }"#;
        fs::write(store.fleet_runtime_path(), stale_runtime).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
        assert!(!fleet
            .workers
            .contains_key(&WorkerId::new("ghost-worker").unwrap()));
    }

    #[test]
    fn test_corrupt_runtime_file_is_tolerated() {
        // A partially written fleet.runtime.json (e.g. crash mid-write
        // despite atomic rename, or hand-edit gone wrong) must not brick
        // the load path. Loader falls back to durable-only; next save
        // overwrites the bad file with a clean overlay.
        let (_tmp, store) = make_store();
        store.save_fleet(&fleet_with_runtime_fields()).unwrap();
        fs::write(store.fleet_runtime_path(), b"{ not valid json").unwrap();
        let fleet = store.load_fleet().unwrap();
        let wid = WorkerId::new("w-split").unwrap();
        // repo/restart_count come from the overlay normally — with corrupt
        // overlay they fall back to what fleet.json carries (nothing here).
        let worker = fleet.workers.get(&wid).unwrap();
        assert!(worker.repo.is_none());
        assert_eq!(worker.restart_count, 0);
    }

    // -------------------------------------------------------------------
    // Trunk lock tests (delib-20260523-a682 / ADR-110 Phase 1 Commit 1)
    // -------------------------------------------------------------------

    #[test]
    fn trunk_lock_acquire_writes_holder_and_clears_on_drop() {
        let (_tmp, store) = make_store();
        let path = store.trunk_lock_path();

        {
            let _guard = store.acquire_trunk_lock("cs done test-mol").unwrap();
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(body.contains("pid="), "holder body missing pid: {body:?}");
            assert!(
                body.contains("cmd=cs done test-mol"),
                "holder body missing cmd: {body:?}"
            );
            assert!(
                body.contains("started_at="),
                "holder body missing started_at"
            );
        }

        // After drop, the holder hint is truncated so a third arrival doesn't
        // see a phantom writer claim.
        let body_after = std::fs::read_to_string(&path).unwrap();
        assert!(
            body_after.trim().is_empty(),
            "holder body should be cleared after drop, got: {body_after:?}"
        );
    }

    #[test]
    fn trunk_lock_with_trunk_lock_runs_closure_under_lock() {
        let (_tmp, store) = make_store();
        let observed = std::cell::Cell::new(false);
        let res: Result<(), CosmonError> = store.with_trunk_lock("cs done with-test", |_| {
            observed.set(true);
            Ok(())
        });
        assert!(res.is_ok());
        assert!(
            observed.get(),
            "closure was not invoked under the trunk lock"
        );
        // Holder cleared after closure returns.
        let body = std::fs::read_to_string(store.trunk_lock_path()).unwrap();
        assert!(body.trim().is_empty());
    }

    #[test]
    fn trunk_lock_propagates_closure_error_and_releases_lock() {
        let (_tmp, store) = make_store();
        let res: Result<(), CosmonError> = store.with_trunk_lock("cs done err-test", |_| {
            Err(CosmonError::Runtime {
                reason: "synthetic failure".to_owned(),
            })
        });
        assert!(res.is_err(), "closure error must propagate out");
        // Re-acquiring after the failure must succeed — the lock must have
        // been released on the error path (RAII drop).
        let _g = store.acquire_trunk_lock("cs done after-err").unwrap();
    }

    #[test]
    fn trunk_lock_serialises_concurrent_acquirers() {
        // Two threads, two distinct FileStore handles sharing the same
        // state dir. Thread A acquires first and holds for ~200ms; thread B
        // attempts to acquire while A holds. B must block (not race past A)
        // and observe an elapsed wait >= the hold duration. This models the
        // molecule's *« 2 workers concurrents → un wait, un passe, pas de
        // contamination »* on the in-process side; cross-process behaviour
        // (`std::process::Command`) is exercised by the integration test
        // at `tests/trunk_lock_concurrent.rs`.
        //
        // `flock(2)` on macOS and Linux (kernel ≥ 2.6.12) is per-FD, so two
        // distinct `OpenOptions::open` calls in the same process do serialise
        // correctly — no need to fork.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::{Duration, Instant};

        let (tmp, _store) = make_store();
        let path: PathBuf = tmp.path().to_path_buf();
        let hold = Duration::from_millis(200);

        let a_acquired = Arc::new(AtomicBool::new(false));
        let a_acquired_clone = Arc::clone(&a_acquired);
        let path_a = path.clone();
        let handle_a = thread::spawn(move || {
            let store_a = FileStore::new(&path_a);
            let guard = store_a.acquire_trunk_lock("cs done A").unwrap();
            a_acquired_clone.store(true, Ordering::SeqCst);
            thread::sleep(hold);
            drop(guard);
        });

        // Spin until A has acquired (deterministic ordering).
        while !a_acquired.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }

        let path_b = path.clone();
        let handle_b = thread::spawn(move || {
            let store_b = FileStore::new(&path_b);
            let start = Instant::now();
            let _guard = store_b.acquire_trunk_lock("cs done B").unwrap();
            start.elapsed()
        });

        handle_a.join().unwrap();
        let elapsed_b = handle_b.join().unwrap();

        // B must have waited at least ~most of the hold window. We allow a
        // 50ms slack for scheduling jitter / spin overhead but require well
        // more than zero — a race-past would land in single-digit ms.
        assert!(
            elapsed_b >= Duration::from_millis(120),
            "B did not wait for A's lock: elapsed {elapsed_b:?}, expected >= 120ms"
        );
    }

    #[test]
    fn trunk_lock_path_is_sibling_of_fleet_lock() {
        // Invariant: both locks live under the same `<state_dir>/` so they
        // share gitignore + state-dir relocation semantics.
        let (_tmp, store) = make_store();
        assert_eq!(
            store.trunk_lock_path().parent(),
            store.lock_path().parent(),
            "trunk.lock and fleet.lock must be siblings"
        );
        assert_ne!(
            store.trunk_lock_path(),
            store.lock_path(),
            "trunk.lock and fleet.lock must be distinct files"
        );
    }
}
