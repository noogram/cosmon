// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem backend for the presence registry (C-PRESENCE-CORE).
//!
//! Each live session owns exactly one file:
//! `<state_root>/presence/<session_id>.json`. Writes are atomic via a
//! `.tmp` sibling + rename, so a reader scanning mid-write either sees
//! the previous snapshot or the new one — never a partial file.
//!
//! The accompanying `<session_id>.log` file (whisper pull-channel) and
//! `<session_id>.seek` pointer are owned by the whisper/presence-poll
//! path; this module only knows about the `.json` snapshot. Naming lives
//! in `log_path` so both sides share a typed path helper (closing the
//! string-level contract flagged in `cmd/presence.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_core::error::CosmonError;
use cosmon_core::id::SessionId;
use cosmon_core::paths::CosmonPath;
use cosmon_core::presence::Presence;

use crate::atomic_write;

/// Directory name for presence files, relative to the state root.
const PRESENCE_DIR: &str = "presence";

/// File-backed presence registry. Stateless; every call is a pure
/// function of the on-disk layout.
///
/// All three presence path families (`<sid>.json` snapshot, `<sid>.log`
/// whisper channel, `<sid>.seek` pointer) are **decoded** from
/// [`CosmonPath`] rather than hand-joined,
/// so this store and the write-path taxonomy cannot drift.
#[derive(Debug, Clone)]
pub struct PresenceStore {
    /// The cosmon **state root** (`.cosmon/state/`). All presence paths are
    /// decoded relative to this via [`CosmonPath::rel`].
    state_root: PathBuf,
    /// Cached presence directory (`<state_root>/presence/`), kept because the
    /// borrowing [`Self::dir`] accessor must hand out a `&Path`.
    root: PathBuf,
}

impl PresenceStore {
    /// Construct a store over the given cosmon **state root**
    /// (`.cosmon/state/`); presence files live under its `presence/` subdir.
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        let state_root = state_root.into();
        let root = state_root.join(PRESENCE_DIR);
        Self { state_root, root }
    }

    /// Return the directory all presence files live in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.root
    }

    /// Path to the JSON snapshot file for a session.
    #[must_use]
    pub fn snapshot_path(&self, sid: &SessionId) -> PathBuf {
        self.state_root
            .join(CosmonPath::PresenceSnapshot { session: sid }.rel())
    }

    /// Path to the whisper-pull log for a session.
    ///
    /// This is the sibling of the `.json` snapshot — same session id,
    /// different extension. Exposed here so writers (`cs whisper
    /// --to-session`) and readers (`cs presence poll`) never reinvent
    /// the layout.
    #[must_use]
    pub fn log_path(&self, sid: &SessionId) -> PathBuf {
        self.state_root
            .join(CosmonPath::PresenceLog { session: sid }.rel())
    }

    /// Path to the whisper read-offset pointer for a session.
    ///
    /// The sibling `<sid>.seek` of the snapshot/log pair. Decoded from
    /// [`CosmonPath`] so the whisper poll path and this store share one
    /// layout source instead of re-joining `"{sid}.seek"` independently.
    #[must_use]
    pub fn seek_path(&self, sid: &SessionId) -> PathBuf {
        self.state_root
            .join(CosmonPath::PresenceSeek { session: sid }.rel())
    }

    /// Atomically write (or overwrite) the presence file for the
    /// session identified by `presence.session_id`.
    ///
    /// The containing directory is created lazily on first write.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] if serialisation or the
    /// atomic rename fails.
    pub fn upsert(&self, presence: &Presence) -> Result<(), CosmonError> {
        let json = serde_json::to_string_pretty(presence).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to serialise presence: {e}"),
        })?;
        let path = self.snapshot_path(&presence.session_id);
        atomic_write(&path, json.as_bytes())
    }

    /// Scan the presence directory and return every parseable snapshot.
    ///
    /// A file that fails to parse (corrupt JSON, missing field,
    /// partially written snapshot that survived atomic rename) is
    /// skipped silently — the registry must not wedge because one peer
    /// wrote garbage. The caller can filter for liveness via
    /// [`Presence::is_live`].
    ///
    /// Order of the returned vector is filesystem-dependent and
    /// therefore unstable across platforms; callers that need a
    /// deterministic order should sort by `session_id`.
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] for filesystem errors other
    /// than "directory does not exist" (which yields an empty vector —
    /// a cold cosmon tree has no live peers).
    pub fn scan(&self) -> Result<Vec<Presence>, CosmonError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|e| CosmonError::StateStore {
            reason: format!("failed to read presence dir {}: {e}", self.root.display()),
        })? {
            let entry = entry.map_err(|e| CosmonError::StateStore {
                reason: format!("read_dir entry failed: {e}"),
            })?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(data) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(p) = serde_json::from_str::<Presence>(&data) {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Remove snapshots whose heartbeat is older than
    /// [`cosmon_core::presence::STALE_AFTER`] AND whose pid is no
    /// longer alive.
    ///
    /// Returns the number of files unlinked. Idempotent: a second call
    /// with nothing new to remove returns `0`. Both conditions must
    /// hold — a stale heartbeat alone does not delete (the session may
    /// be paused in a debugger; the pid is still around), and a live
    /// pid alone does not delete (a long-running shell might share the
    /// pid slot with a dead session, so freshness still gates
    /// everything).
    ///
    /// # Errors
    ///
    /// Returns [`CosmonError::StateStore`] for filesystem errors that
    /// prevent scanning. Individual `unlink` failures after the probe
    /// are swallowed and do not abort the sweep — the next `gc` call
    /// will retry.
    pub fn gc(&self) -> Result<usize, CosmonError> {
        let presences = self.scan()?;
        let now = Utc::now();
        let mut removed = 0usize;
        for p in presences {
            if !p.is_live(now) && !pid_is_alive(p.pid) {
                let snap = self.snapshot_path(&p.session_id);
                if fs::remove_file(&snap).is_ok() {
                    removed += 1;
                }
                // Best-effort: unlink the companion log + seek so the
                // directory does not accumulate orphans after a crash.
                let log = self.log_path(&p.session_id);
                let seek = self.seek_path(&p.session_id);
                let _ = fs::remove_file(&log);
                let _ = fs::remove_file(&seek);
            }
        }
        Ok(removed)
    }
}

/// Probe whether `pid` names a live process on this host without
/// sending a real signal.
///
/// Uses the standard `kill(pid, 0)` trick (same pattern as
/// `cosmon-daemon-supervisor::adapters::tokio_process::pid_is_alive`).
/// Returns `false` on `ESRCH` (pid not found), `false` for any other
/// error (permission-denied etc. — "not ours to worry about"), and
/// `false` for pids that do not fit in `i32`.
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    let Ok(pid_i32) = i32::try_from(pid) else {
        return false;
    };
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid_i32), None),
        Ok(())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration};
    use cosmon_core::id::MoleculeId;
    use cosmon_core::presence::STALE_AFTER;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, PresenceStore) {
        let tmp = TempDir::new().unwrap();
        let store = PresenceStore::new(tmp.path());
        (tmp, store)
    }

    fn sample(sid: &str, heartbeat: DateTime<chrono::Utc>, pid: u32) -> Presence {
        Presence {
            session_id: SessionId::new(sid).unwrap(),
            galaxy: "cosmon".to_owned(),
            cwd: PathBuf::from("/tmp/proj"),
            pid,
            started_at: heartbeat,
            heartbeat_at: heartbeat,
            current_molecule: None,
            headline: "idle".to_owned(),
            tty: None,
        }
    }

    #[test]
    fn upsert_then_scan_roundtrips() {
        let (_tmp, store) = make_store();
        let p = sample("sid-alpha", Utc::now(), std::process::id());
        store.upsert(&p).unwrap();
        let loaded = store.scan().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id, p.session_id);
        assert_eq!(loaded[0].galaxy, "cosmon");
    }

    #[test]
    fn upsert_overwrites_existing_file() {
        let (_tmp, store) = make_store();
        let mut p = sample("sid-over", Utc::now(), std::process::id());
        store.upsert(&p).unwrap();
        p.headline = "new headline".to_owned();
        store.upsert(&p).unwrap();
        let loaded = store.scan().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].headline, "new headline");
    }

    #[test]
    fn scan_on_missing_dir_is_empty() {
        let tmp = TempDir::new().unwrap();
        let store = PresenceStore::new(tmp.path().join("does-not-exist"));
        assert!(store.scan().unwrap().is_empty());
    }

    #[test]
    fn scan_ignores_non_json_and_malformed() {
        let (_tmp, store) = make_store();
        let p = sample("sid-good", Utc::now(), std::process::id());
        store.upsert(&p).unwrap();
        fs::create_dir_all(store.dir()).unwrap();
        fs::write(store.dir().join("not-presence.txt"), "hello").unwrap();
        fs::write(store.dir().join("bad.json"), "{ not valid json").unwrap();
        let loaded = store.scan().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].session_id.as_str(), "sid-good");
    }

    #[test]
    fn gc_removes_stale_dead_pids() {
        let (_tmp, store) = make_store();
        // Dead pid + stale heartbeat → must be collected.
        let old = Utc::now() - STALE_AFTER - Duration::minutes(1);
        let stale = sample("sid-stale", old, 999_999_999);
        store.upsert(&stale).unwrap();

        // Fresh heartbeat + same bogus pid → stays (freshness wins).
        let fresh = sample("sid-fresh", Utc::now(), 999_999_999);
        store.upsert(&fresh).unwrap();

        // Stale heartbeat + live pid (our own process) → stays (pid wins).
        let live = sample("sid-live", old, std::process::id());
        store.upsert(&live).unwrap();

        let removed = store.gc().unwrap();
        assert_eq!(removed, 1, "only the dead+stale snapshot should go");

        let remaining: Vec<_> = store
            .scan()
            .unwrap()
            .into_iter()
            .map(|p| p.session_id)
            .collect();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().any(|s| s.as_str() == "sid-fresh"));
        assert!(remaining.iter().any(|s| s.as_str() == "sid-live"));
    }

    #[test]
    fn gc_is_idempotent() {
        let (_tmp, store) = make_store();
        let old = Utc::now() - STALE_AFTER - Duration::minutes(1);
        store.upsert(&sample("sid-a", old, 999_999_999)).unwrap();
        let first = store.gc().unwrap();
        let second = store.gc().unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 0);
    }

    #[test]
    fn gc_on_empty_dir_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let store = PresenceStore::new(tmp.path().join("never-written"));
        assert_eq!(store.gc().unwrap(), 0);
    }

    #[test]
    fn log_and_snapshot_paths_share_session_id() {
        let (_tmp, store) = make_store();
        let sid = SessionId::new("session-xyz").unwrap();
        let snap = store.snapshot_path(&sid);
        let log = store.log_path(&sid);
        assert_eq!(snap.file_name().unwrap(), "session-xyz.json");
        assert_eq!(log.file_name().unwrap(), "session-xyz.log");
        assert_eq!(snap.parent(), log.parent());
    }

    #[test]
    fn current_molecule_survives_roundtrip() {
        let (_tmp, store) = make_store();
        let mut p = sample("sid-mol", Utc::now(), std::process::id());
        p.current_molecule = Some(MoleculeId::new("task-20260424-c51a").unwrap());
        store.upsert(&p).unwrap();
        let loaded = store.scan().unwrap();
        assert_eq!(loaded[0].current_molecule, p.current_molecule);
    }

    #[test]
    fn pid_is_alive_detects_self() {
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn pid_is_alive_rejects_unused_pid() {
        assert!(!pid_is_alive(999_999_999));
    }
}
