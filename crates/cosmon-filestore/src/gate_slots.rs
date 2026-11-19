// SPDX-License-Identifier: AGPL-3.0-only

//! Gate slots: at most N full gates at once (ADR-190 D1, noogram/cosmon#214).
//!
//! A slot is a kernel `flock` on `<state dir>/gate-slots/<pool>/<k>.lock`, held
//! by the process that runs the gate for the life of that run. The kernel
//! releases it when the holder dies, so a crashed gate frees its slot without
//! any reaper. The state directory is the **main checkout's**, resolved by the
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
//! another. The slot files carry no content; liveness is the flock itself, never
//! a pid read from a file.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cosmon_core::error::CosmonError;
use fs2::FileExt;

use crate::FileStore;

/// Directory under the state directory that holds every pool.
pub const GATE_SLOTS_DIR: &str = "gate-slots";

/// How long a blocked acquirer sleeps between two sweeps of the pool.
///
/// One `flock` cannot wait on any of N files, so a waiter sweeps with
/// non-blocking attempts. A tenth of a second is small against a gate that
/// lasts minutes and keeps an idle waiter off the CPU.
const SWEEP_INTERVAL: Duration = Duration::from_millis(100);

/// RAII claim on one slot of a pool. The `flock` is released when the guard
/// drops, or by the kernel when the holding process dies.
#[must_use = "the slot is released when the guard drops; bind it to a local to keep it"]
#[derive(Debug)]
pub struct GateSlotGuard {
    /// Keeps the open file description, and so the lock, alive.
    _file: File,
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
            .open(&path)
            .map_err(|e| lock_err(&path, format!("failed to open slot file: {e}")))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                return Ok(Some(GateSlotGuard {
                    _file: file,
                    index: k,
                    path,
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(lock_err(&path, format!("failed to probe slot: {e}"))),
        }
    }
    Ok(None)
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
