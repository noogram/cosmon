// SPDX-License-Identifier: AGPL-3.0-only

//! Gate slots: at most N full gates at once (ADR-190 D1, noogram/cosmon#214).
//!
//! A slot is a kernel `flock` on `<state dir>/gate-slots/<pool>/<k>.lock`, held
//! by the process that runs the gate for the life of that run. The kernel
//! releases it when the last copy of the descriptor closes. By default only
//! the holder retains it, so no reaper is needed. The state directory is the
//! **main checkout's**, resolved by the
//! same walk-up the trunk lock uses: two worktrees of one galaxy share one set
//! of slot files, which is what makes N a machine-wide bound rather than a
//! per-worktree one.
//!
//! # Lock order
//!
//! `slot → trunk → fleet`. A slot is the outermost lock; the only permitted
//! nesting is slot → trunk. `cs done` never takes a slot, so a landing is never
//! queued behind a gate.
//!
//! Pools are independent directories: a holder of one pool never blocks
//! another. Slot and queue files carry display records. The flock itself
//! answers whether a slot or queue entry is held.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read as _, Write as _};
use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use cosmon_core::error::CosmonError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::FileStore;

/// Directory under the state directory that holds every pool.
pub const GATE_SLOTS_DIR: &str = "gate-slots";

/// How long a blocked acquirer sleeps between two sweeps of the pool.
///
/// One `flock` cannot wait on any of N files, so a waiter sweeps with
/// non-blocking attempts. A tenth of a second is small against a gate that
/// lasts minutes and keeps an idle waiter off the CPU.
const SWEEP_INTERVAL: Duration = Duration::from_millis(100);
const QUEUE_PREFIX: &str = "queued-";
const RECORD_LIMIT: u64 = 4 * 1024 * 1024;

/// Display metadata for a gate invocation. The process IDs are descriptive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateRunRecord {
    /// PID of the `cs gates slot` supervisor.
    pub pid: u32,
    /// Process group of the command, once it has started.
    pub pgid: Option<u32>,
    /// Name of the pool the invocation requested.
    pub pool: String,
    /// Command and arguments passed to `cs gates slot`.
    pub command: Vec<String>,
    /// Time the command began, or the queue entry time while waiting.
    pub started_at: DateTime<Utc>,
    /// Most recent observed output, if any.
    pub last_output_at: Option<DateTime<Utc>>,
    /// Whether the supervisor can observe both output streams.
    pub output_observed: bool,
}

/// A numbered slot that is held or retains a record from an earlier run.
#[derive(Debug)]
pub struct GateSlotObservation {
    /// Index of the on-disk slot.
    pub index: usize,
    /// Whether an exclusive flock was observed on the slot.
    pub held: bool,
    /// Parsed holder metadata, if the wrapper published a readable record.
    pub record: Option<GateRunRecord>,
}

/// Display snapshot of one on-disk gate pool.
#[derive(Debug, Default)]
pub struct GatePoolObservation {
    /// Held slots and stale slot records, in slot order.
    pub slots: Vec<GateSlotObservation>,
    /// Locked queue records, oldest first.
    pub queued: Vec<GateRunRecord>,
}

/// RAII claim on one slot of a pool. The `flock` is released when the kernel
/// closes the last copy of the locked descriptor.
#[must_use = "the slot is released when the guard drops; bind it to a local to keep it"]
#[derive(Debug)]
pub struct GateSlotGuard {
    /// Keeps the open file description, and so the lock, alive.
    file: File,
    index: usize,
    path: PathBuf,
}

impl GateSlotGuard {
    /// Index `k` of the slot held, in `0..count`.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Path of the slot's lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Publish display metadata on the locked inode without moving its offset.
    ///
    /// # Errors
    ///
    /// Returns a serialization or filesystem error if the record cannot be written.
    pub fn write_record(&self, record: &GateRunRecord) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(record)?;
        self.file
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        self.file.write_all_at(&bytes, 0)?;
        self.file.set_len(bytes.len() as u64)
    }
}

// Borrow the locked open file description so an opt-in launcher can make its
// copy inheritable in the child, without changing the parent's descriptor.
impl std::os::fd::AsFd for GateSlotGuard {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// Directory of one pool under `state_dir`.
///
/// # Errors
///
/// [`CosmonError::LockFailed`] when `pool` is empty or would escape the
/// `gate-slots` directory (a path separator, `.` or `..`): the pool name comes
/// from project configuration and must name a directory, never a path.
pub fn pool_dir(state_dir: &Path, pool: &str) -> Result<PathBuf, CosmonError> {
    if pool.is_empty() || pool == "." || pool == ".." || pool.contains(['/', '\\', '\0']) {
        return Err(CosmonError::LockFailed {
            path: state_dir.join(GATE_SLOTS_DIR).display().to_string(),
            reason: format!("invalid gate-slot pool name {pool:?}"),
        });
    }
    Ok(state_dir.join(GATE_SLOTS_DIR).join(pool))
}

fn slot_path(dir: &Path, k: usize) -> PathBuf {
    dir.join(format!("{k}.lock"))
}

fn lock_err(path: &Path, reason: String) -> CosmonError {
    CosmonError::LockFailed {
        path: path.display().to_string(),
        reason,
    }
}

/// One non-blocking sweep over slots `0..count`.
fn sweep(dir: &Path, count: usize) -> Result<Option<GateSlotGuard>, CosmonError> {
    fs::create_dir_all(dir)
        .map_err(|e| lock_err(dir, format!("failed to create gate-slot directory: {e}")))?;
    for k in 0..count {
        let path = slot_path(dir, k);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| lock_err(&path, format!("failed to open slot file: {e}")))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                file.set_len(0).map_err(|e| {
                    lock_err(&path, format!("failed to clear old slot record: {e}"))
                })?;
                return Ok(Some(GateSlotGuard {
                    file,
                    index: k,
                    path,
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(lock_err(&path, format!("failed to probe slot: {e}"))),
        }
    }
    Ok(None)
}

/// Keep a queue record locked and visible until the returned file is dropped.
///
/// Each waiter has its own inode. An abandoned record is ignored by observers
/// and removed by a subsequent queue registration in the same pool.
///
/// # Errors
///
/// Returns a lock or filesystem error if the entry cannot be published.
pub fn register_gate_waiter(
    state_dir: &Path,
    record: &GateRunRecord,
) -> Result<tempfile::NamedTempFile, CosmonError> {
    let dir = pool_dir(state_dir, &record.pool)?;
    let register = || -> std::io::Result<_> {
        fs::create_dir_all(&dir)?;
        for entry in fs::read_dir(&dir)?.flatten().filter(is_queue_entry) {
            if let Ok(file) = File::open(entry.path()) {
                if FileExt::try_lock_shared(&file).is_ok() {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        let mut entry = tempfile::Builder::new().tempfile_in(&dir)?;
        entry.as_file().try_lock_exclusive()?;
        serde_json::to_writer(&mut entry, record)?;
        entry.flush()?;
        let mut name = std::ffi::OsString::from(QUEUE_PREFIX);
        name.push(
            entry
                .path()
                .file_name()
                .ok_or(std::io::ErrorKind::InvalidData)?,
        );
        let (file, path) = entry.into_parts();
        let published = dir.join(name);
        path.persist_noclobber(&published)?;
        Ok(tempfile::NamedTempFile::from_parts(
            file,
            tempfile::TempPath::try_from_path(published)?,
        ))
    };
    register().map_err(|e| lock_err(&dir, format!("failed to publish gate queue record: {e}")))
}

fn is_queue_entry(entry: &fs::DirEntry) -> bool {
    entry
        .file_name()
        .to_string_lossy()
        .starts_with(QUEUE_PREFIX)
}

fn read_record(file: &File, pool: &str) -> Option<GateRunRecord> {
    let record: GateRunRecord =
        serde_json::from_reader(BufReader::new(file.take(RECORD_LIMIT))).ok()?;
    (record.pool == pool).then_some(record)
}

fn probe_held(file: &File) -> std::io::Result<bool> {
    match FileExt::try_lock_shared(file) {
        Ok(()) => {
            FileExt::unlock(file)?;
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(e) => Err(e),
    }
}

/// Read all on-disk gate pools without creating state or acquiring a slot.
///
/// The numbers are a point-in-time observation. A malformed or absent record
/// can hide a command's metadata, while the flock still reports occupancy.
///
/// # Errors
///
/// Returns a filesystem error if the pool directory or a lock cannot be read.
pub fn observe_gate_pools(
    state_dir: &Path,
) -> Result<BTreeMap<String, GatePoolObservation>, CosmonError> {
    let root = state_dir.join(GATE_SLOTS_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(lock_err(&root, format!("failed to read gate pools: {e}"))),
    };
    let mut pools = BTreeMap::new();
    for entry in entries.flatten() {
        let Some(pool) = entry.file_name().into_string().ok() else {
            continue;
        };
        if pool_dir(state_dir, &pool).is_ok() && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            pools.insert(pool.clone(), observe_pool(&entry.path(), &pool)?);
        }
    }
    Ok(pools)
}

fn observe_pool(dir: &Path, pool: &str) -> Result<GatePoolObservation, CosmonError> {
    let entries =
        fs::read_dir(dir).map_err(|e| lock_err(dir, format!("failed to read gate pool: {e}")))?;
    let mut observation = GatePoolObservation::default();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let index = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".lock"))
            .and_then(|index| index.parse().ok());
        let queued = is_queue_entry(&entry);
        if index.is_none() && !queued {
            continue;
        }
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(lock_err(&path, format!("failed to open gate record: {e}"))),
        };
        let record = read_record(&file, pool);
        let held = probe_held(&file)
            .map_err(|e| lock_err(&path, format!("failed to probe gate record: {e}")))?;
        if let Some(index) = index {
            if held || record.is_some() {
                observation.slots.push(GateSlotObservation {
                    index,
                    held,
                    record,
                });
            }
        } else if let Some(record) = record.filter(|_| held) {
            observation.queued.push(record);
        }
    }
    observation.slots.sort_by_key(|slot| slot.index);
    observation
        .queued
        .sort_by_key(|record| (record.started_at, record.pid));
    Ok(observation)
}

/// Take a free slot of `pool` under `state_dir` if there is one, without waiting.
///
/// # Errors
///
/// [`CosmonError::LockFailed`] for an invalid pool name or a failing syscall.
/// A pool whose `count` slots are all held is `Ok(None)`, not an error.
pub fn try_acquire_gate_slot_in(
    state_dir: &Path,
    pool: &str,
    count: usize,
) -> Result<Option<GateSlotGuard>, CosmonError> {
    sweep(&pool_dir(state_dir, pool)?, count)
}

/// Take a slot of `pool` under `state_dir`, blocking until one is free.
///
/// # Errors
///
/// [`CosmonError::LockFailed`] for an invalid pool name, a `count` of zero
/// (a pool with no slot would block forever), or a failing syscall.
pub fn acquire_gate_slot_in(
    state_dir: &Path,
    pool: &str,
    count: usize,
) -> Result<GateSlotGuard, CosmonError> {
    let dir = pool_dir(state_dir, pool)?;
    if count == 0 {
        return Err(lock_err(
            &dir,
            "a gate-slot pool of size 0 admits no run".to_owned(),
        ));
    }
    loop {
        if let Some(guard) = sweep(&dir, count)? {
            return Ok(guard);
        }
        std::thread::sleep(SWEEP_INTERVAL);
    }
}

/// How many slots of `pool` are held by a live process right now.
///
/// Probes every `*.lock` file of the pool with a non-blocking `flock` and
/// releases it at once. An absent pool has no holder. The probe is the kernel's
/// own answer, so a holder that was killed is not counted.
///
/// # Errors
///
/// [`CosmonError::LockFailed`] for an invalid pool name or an unreadable pool
/// directory (other than one that does not exist).
pub fn live_gate_slot_holders_in(state_dir: &Path, pool: &str) -> Result<usize, CosmonError> {
    let dir = pool_dir(state_dir, pool)?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(lock_err(&dir, format!("failed to read pool: {e}"))),
    };
    let mut held = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "lock") {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = FileExt::unlock(&file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => held += 1,
            Err(_) => {}
        }
    }
    Ok(held)
}

impl FileStore {
    /// Take a slot of `pool` (of `count` slots), blocking until one is free.
    ///
    /// The slot files live under this store's state directory, which for a
    /// store opened from any worktree of a galaxy is the main checkout's. See
    /// the module documentation for the lock order: a slot is acquired before
    /// the trunk lock, never inside it.
    ///
    /// # Errors
    ///
    /// See [`acquire_gate_slot_in`].
    pub fn acquire_gate_slot(
        &self,
        pool: &str,
        count: usize,
    ) -> Result<GateSlotGuard, CosmonError> {
        acquire_gate_slot_in(&self.root, pool, count)
    }

    /// Non-blocking variant of [`Self::acquire_gate_slot`].
    ///
    /// # Errors
    ///
    /// See [`try_acquire_gate_slot_in`].
    pub fn try_acquire_gate_slot(
        &self,
        pool: &str,
        count: usize,
    ) -> Result<Option<GateSlotGuard>, CosmonError> {
        try_acquire_gate_slot_in(&self.root, pool, count)
    }

    /// Number of live holders of `pool`. See [`live_gate_slot_holders_in`].
    ///
    /// # Errors
    ///
    /// See [`live_gate_slot_holders_in`].
    pub fn live_gate_slot_holders(&self, pool: &str) -> Result<usize, CosmonError> {
        live_gate_slot_holders_in(&self.root, pool)
    }
}
