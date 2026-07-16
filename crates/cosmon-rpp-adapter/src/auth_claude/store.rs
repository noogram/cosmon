// SPDX-License-Identifier: AGPL-3.0-only

//! Disk-backed session store. One JSON file per session under
//! `<state_dir>/auth-sessions/<session_id>.json`. Sessions survive a
//! process restart so an in-flight auth flow is recoverable.

use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::auth_claude::state::AuthSession;

/// Errors returned by [`SessionStore`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Session with this id does not exist.
    #[error("session not found")]
    NotFound,
    /// Underlying filesystem I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation error.
    #[error("serde_json: {0}")]
    Json(#[from] serde_json::Error),
    /// Session id is malformed (would escape the store dir).
    #[error("invalid session id")]
    InvalidSessionId,
}

/// Disk-backed session store contract. Implementations must be
/// `Send + Sync` so they can live inside `Arc<dyn SessionStore>`.
pub trait SessionStore: Send + Sync + Debug {
    /// Persist a new or updated session.
    fn upsert(&self, session: &AuthSession) -> Result<(), StoreError>;
    /// Load a session by id. Returns [`StoreError::NotFound`] if absent.
    fn load(&self, session_id: &str) -> Result<AuthSession, StoreError>;
    /// Delete a session (idempotent — missing is success).
    fn delete(&self, session_id: &str) -> Result<(), StoreError>;
    /// Count currently-open sessions (`INIT` or `AWAITING_USER_APPROVAL`).
    /// Used for rate limiting in §8 of the spec.
    fn count_open(&self) -> Result<usize, StoreError>;
}

/// Filesystem-backed implementation. The directory is created on
/// construction; per-session writes go through `tempfile + rename` for
/// atomicity.
#[derive(Debug)]
pub struct FilesystemSessionStore {
    root: PathBuf,
    // Coarse write lock — auth sessions are not high-volume (a
    // container sees ~10/hour at most) so contention is negligible
    // and we avoid the sled/fjall dependency surface.
    write_lock: Mutex<()>,
}

impl FilesystemSessionStore {
    /// Create a store rooted at `<state_dir>/auth-sessions/`. Creates
    /// the directory if absent.
    pub fn new(state_dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = state_dir.into().join("auth-sessions");
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            write_lock: Mutex::new(()),
        })
    }

    fn session_path(&self, session_id: &str) -> Result<PathBuf, StoreError> {
        // Reject path-escape attempts: session ids must match the
        // canonical shape `auth-<digits>-<hex>` and contain neither
        // `/`, `\`, nor `.` traversal.
        if session_id.is_empty()
            || session_id.contains('/')
            || session_id.contains('\\')
            || session_id.contains("..")
            || !session_id.starts_with("auth-")
        {
            return Err(StoreError::InvalidSessionId);
        }
        Ok(self.root.join(format!("{session_id}.json")))
    }
}

impl SessionStore for FilesystemSessionStore {
    fn upsert(&self, session: &AuthSession) -> Result<(), StoreError> {
        let path = self.session_path(&session.session_id)?;
        let payload = serde_json::to_vec_pretty(session)?;
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Atomic replace: write to a sibling tmp file, then rename.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &payload)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn load(&self, session_id: &str) -> Result<AuthSession, StoreError> {
        let path = self.session_path(session_id)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    fn delete(&self, session_id: &str) -> Result<(), StoreError> {
        let path = self.session_path(session_id)?;
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn count_open(&self) -> Result<usize, StoreError> {
        let mut count = 0usize;
        let dir = match std::fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        for entry in dir {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Ok(session) = serde_json::from_slice::<AuthSession>(&bytes) {
                use crate::auth_claude::state::AuthState;
                if matches!(
                    session.state,
                    AuthState::Init | AuthState::AwaitingUserApproval
                ) {
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_claude::state::AuthState;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn dummy_session(id: &str) -> AuthSession {
        AuthSession::new(
            id.to_owned(),
            chrono::Utc::now(),
            chrono::Duration::minutes(15),
        )
    }

    #[test]
    fn upsert_load_roundtrip() {
        let dir = tmp();
        let store = FilesystemSessionStore::new(dir.path()).unwrap();
        let s = dummy_session("auth-20260519-aaaaaa");
        store.upsert(&s).unwrap();
        let loaded = store.load("auth-20260519-aaaaaa").unwrap();
        assert_eq!(loaded.session_id, s.session_id);
        assert_eq!(loaded.state, AuthState::Init);
    }

    #[test]
    fn load_missing_returns_not_found() {
        let dir = tmp();
        let store = FilesystemSessionStore::new(dir.path()).unwrap();
        let err = store.load("auth-20260519-zzzzzz").unwrap_err();
        assert!(matches!(err, StoreError::NotFound));
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tmp();
        let store = FilesystemSessionStore::new(dir.path()).unwrap();
        store.delete("auth-20260519-zzzzzz").unwrap(); // not present, OK
        let s = dummy_session("auth-20260519-bbbbbb");
        store.upsert(&s).unwrap();
        store.delete("auth-20260519-bbbbbb").unwrap();
        assert!(matches!(
            store.load("auth-20260519-bbbbbb"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn count_open_only_counts_init_and_awaiting() {
        let dir = tmp();
        let store = FilesystemSessionStore::new(dir.path()).unwrap();
        let s1 = dummy_session("auth-20260519-111111");
        store.upsert(&s1).unwrap();
        let mut s2 = dummy_session("auth-20260519-222222");
        s2.state = AuthState::AwaitingUserApproval;
        store.upsert(&s2).unwrap();
        let mut s3 = dummy_session("auth-20260519-333333");
        s3.state = AuthState::Completed;
        store.upsert(&s3).unwrap();
        let mut s4 = dummy_session("auth-20260519-444444");
        s4.state = AuthState::Failed;
        store.upsert(&s4).unwrap();
        assert_eq!(store.count_open().unwrap(), 2);
    }

    #[test]
    fn rejects_path_traversal_session_id() {
        let dir = tmp();
        let store = FilesystemSessionStore::new(dir.path()).unwrap();
        assert!(matches!(
            store.load("auth-../../etc/passwd"),
            Err(StoreError::InvalidSessionId)
        ));
        assert!(matches!(
            store.load("not-an-auth-id"),
            Err(StoreError::InvalidSessionId)
        ));
    }
}
