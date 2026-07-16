// SPDX-License-Identifier: AGPL-3.0-only

//! JSON file-backed implementation of [`crate::ports::StatePort`].
//!
//! Writes are atomic: every `save` lands in a sibling `.tmp` file and then
//! atomically renames onto the target, so a supervisor killed mid-write
//! can never corrupt the state document. The next boot either sees the
//! previous complete snapshot or the new complete snapshot — never a torn
//! read.
//!
//! The same pattern `cosmon-scheduler::SchedulerState::save_atomic` uses,
//! deliberately: both resident processes write their single JSON state
//! file under `~/.cosmon/`, and both need to survive launchd kills without
//! operator intervention.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ports::{StateError, StatePort, SupervisorState};

/// File-backed [`StatePort`] writing JSON under the given path.
///
/// On `load`, a missing file is treated as "first boot" and yields
/// [`SupervisorState::default()`]. On `save`, parent directories are
/// created if missing (typical when `~/.cosmon/` has not yet been
/// touched), serialization is pretty-printed for operator diagnostics,
/// and the final write is atomic via rename.
#[derive(Debug, Clone)]
pub struct FileStatePort {
    path: PathBuf,
}

impl FileStatePort {
    /// Construct a port pointed at `path`.
    ///
    /// The path is not touched until the first `load` or `save` call, so
    /// tests can instantiate against a non-existent file and observe the
    /// first-boot branch.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Target path this port reads from / writes to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

impl StatePort for FileStatePort {
    fn load(&self) -> Result<SupervisorState, StateError> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| StateError::Serde(e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SupervisorState::default()),
            Err(e) => Err(StateError::Io(format!("read {}: {e}", self.path.display()))),
        }
    }

    fn save(&mut self, state: &SupervisorState) -> Result<(), StateError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| {
                    StateError::Io(format!("create_dir_all {}: {e}", parent.display()))
                })?;
            }
        }
        let raw =
            serde_json::to_string_pretty(state).map_err(|e| StateError::Serde(e.to_string()))?;
        let tmp = tmp_sibling(&self.path);
        fs::write(&tmp, raw)
            .map_err(|e| StateError::Io(format!("write {}: {e}", tmp.display())))?;
        fs::rename(&tmp, &self.path).map_err(|e| {
            StateError::Io(format!(
                "rename {} → {}: {e}",
                tmp.display(),
                self.path.display()
            ))
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChildStatus;
    use crate::ports::PersistedChild;
    use chrono::{TimeZone, Utc};

    #[test]
    fn load_missing_file_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("missing.json");
        let port = FileStatePort::new(&path);
        let state = port.load().expect("first boot ok");
        assert_eq!(state, SupervisorState::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("state.json");
        let mut port = FileStatePort::new(&path);

        let mut state = SupervisorState::default();
        state.children.insert(
            "tg-bot".into(),
            PersistedChild {
                name: "tg-bot".into(),
                status: ChildStatus::Running,
                pid: Some(4242),
                last_exit_code: None,
                last_spawn_at: Some(Utc.timestamp_opt(100, 0).unwrap()),
                last_exit_at: None,
                respawn_count: 2,
            },
        );
        port.save(&state).expect("save");

        let loaded = port.load().expect("load");
        assert_eq!(loaded, state);
    }

    #[test]
    fn save_is_atomic_via_tmp_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        let mut port = FileStatePort::new(&path);
        port.save(&SupervisorState::default()).unwrap();
        assert!(path.exists());
        let tmp_file = tmp_sibling(&path);
        assert!(
            !tmp_file.exists(),
            "tmp sibling should be gone after rename: {}",
            tmp_file.display()
        );
    }

    #[test]
    fn malformed_json_surfaces_serde_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("corrupt.json");
        fs::write(&path, "{not json").unwrap();
        let port = FileStatePort::new(&path);
        let err = port.load().expect_err("parse fails");
        assert!(matches!(err, StateError::Serde(_)));
    }
}
