// SPDX-License-Identifier: AGPL-3.0-only

//! Scheduler state — the answer to "when did each patrol last fire?"
//!
//! One JSON file at the path given by `scheduler.state_file`
//! (default `~/.cosmon/scheduler.state.json`) keyed by patrol name. The
//! scheduler reads it at the start of every tick and rewrites it
//! **atomically** (write-to-`.tmp` + `rename`) after the dispatch pass.
//! Atomicity matters because a launchd-killed scheduler must never leave
//! a truncated file that the next tick would treat as "no patrol has
//! ever fired".
//!
//! ## Schema (v1)
//!
//! ```json
//! {
//!   "version": 1,
//!   "patrols": {
//!     "executor-pulse": {
//!       "last_fired_at": "2026-04-18T12:34:00Z",
//!       "last_exit_code": 0,
//!       "last_pid": 42017,
//!       "fire_count": 128
//!     }
//!   }
//! }
//! ```
//!
//! ## Forward-compat
//!
//! - `version: 1` is parsed loosely — unknown fields are accepted (future-
//!   compatible on read), so an old scheduler reading a newer state file
//!   degrades gracefully.
//! - Missing file ⇒ empty state (first boot).
//! - Malformed file ⇒ hard error surfaced to the operator; we never
//!   silently reset state because that would mask a bug behind a mass
//!   refire.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level state document for the scheduler.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SchedulerState {
    /// Schema version. Always `1` for this crate release.
    #[serde(default = "default_version")]
    pub version: u32,

    /// Per-patrol state keyed by [`crate::config::Patrol::name`].
    #[serde(default)]
    pub patrols: BTreeMap<String, PatrolState>,
}

fn default_version() -> u32 {
    1
}

/// Everything the scheduler remembers about one patrol across ticks.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PatrolState {
    /// Timestamp (UTC) of the most recent successful dispatch, `None` if
    /// the patrol has never fired. Used by the interval cadence gate and
    /// by cron "don't re-fire within the same minute" de-duplication.
    #[serde(default)]
    pub last_fired_at: Option<DateTime<Utc>>,

    /// Exit code of the last synchronous (`dispatch = "wait"`) run, if
    /// any. For `dispatch = "detached"` runs this stays `None` because
    /// the scheduler returns before the child terminates.
    #[serde(default)]
    pub last_exit_code: Option<i32>,

    /// PID of the most recently spawned child. Informational — useful
    /// for operators debugging a stuck patrol via `ps`.
    #[serde(default)]
    pub last_pid: Option<u32>,

    /// Monotonic counter of successful dispatches. Useful for
    /// `cs scheduler status` to answer "has X fired at all lately?"
    /// without parsing timestamps.
    #[serde(default)]
    pub fire_count: u64,

    /// Timestamp (UTC) at which the `[patrol.sunset]` rule converged and the
    /// sunset action was executed. `Some` means "this patrol has been
    /// sunsetted; do not evaluate cadence or fire it again". The field is
    /// write-once per patrol lifetime — the idempotence fence that prevents
    /// a double-unload when the scheduler ticks before the launchd unload
    /// has propagated.
    #[serde(default)]
    pub sunset_decided_at: Option<DateTime<Utc>>,
}

/// Errors surfaced while loading or saving scheduler state.
#[derive(Debug, Error)]
pub enum StateError {
    /// Filesystem I/O failure reading or writing the state file (or its
    /// parent directory).
    #[error("scheduler state I/O error: {0}")]
    Io(#[from] io::Error),

    /// The state file exists but is not valid JSON matching the schema.
    /// Never silently ignored — operator must decide whether to repair
    /// by hand or delete the file.
    #[error("scheduler state parse error: {0}")]
    Parse(#[from] serde_json::Error),
}

impl SchedulerState {
    /// Load the state file from `path`. A missing file yields a
    /// default (empty) state — that is the "first boot" path.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Io`] for filesystem failures other than
    /// `NotFound`, or [`StateError::Parse`] for malformed JSON.
    pub fn load_or_default(path: &Path) -> Result<Self, StateError> {
        match fs::read_to_string(path) {
            Ok(raw) => Ok(serde_json::from_str(&raw)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StateError::Io(e)),
        }
    }

    /// Save atomically to `path`: write the serialized JSON to a sibling
    /// `.tmp` file and then `rename` onto the target. On POSIX rename
    /// is atomic on the same filesystem, so a reader either sees the
    /// old complete file or the new complete file — never a torn one.
    ///
    /// Creates the parent directory if missing (typical on first boot
    /// where `~/.cosmon/` has not been created yet).
    ///
    /// # Errors
    ///
    /// Returns [`StateError::Io`] for directory creation, temp-file
    /// write, or rename failures; [`StateError::Parse`] if serialization
    /// fails (should not happen in practice — shapes are owned data).
    pub fn save_atomic(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let tmp = tmp_sibling(path);
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(&tmp, raw)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Mutable borrow of the patrol entry, inserting a default if
    /// absent. The common path during a tick.
    pub fn patrol_mut(&mut self, name: &str) -> &mut PatrolState {
        self.patrols.entry(name.to_owned()).or_default()
    }
}

/// Sibling `.tmp` path: `foo/bar.json` → `foo/bar.json.tmp`. Kept in
/// the same directory so `rename` stays intra-filesystem (atomic).
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn load_missing_file_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("missing.json");
        let state = SchedulerState::load_or_default(&path).expect("missing is ok");
        assert_eq!(state.version, 0, "default uses derived Default → 0");
        assert!(state.patrols.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("state.json");

        let fired = Utc.with_ymd_and_hms(2026, 4, 18, 12, 34, 0).unwrap();
        let mut state = SchedulerState {
            version: 1,
            patrols: BTreeMap::new(),
        };
        state.patrols.insert(
            "executor-pulse".to_owned(),
            PatrolState {
                last_fired_at: Some(fired),
                last_exit_code: Some(0),
                last_pid: Some(42017),
                fire_count: 7,
                sunset_decided_at: None,
            },
        );

        state.save_atomic(&path).expect("save ok");

        let loaded = SchedulerState::load_or_default(&path).expect("load ok");
        assert_eq!(loaded.version, 1);
        let entry = loaded.patrols.get("executor-pulse").expect("entry present");
        assert_eq!(entry.last_fired_at, Some(fired));
        assert_eq!(entry.last_exit_code, Some(0));
        assert_eq!(entry.last_pid, Some(42017));
        assert_eq!(entry.fire_count, 7);
    }

    #[test]
    fn save_is_atomic_via_tmp_sibling() {
        // Smoke test: after a save, only the final file exists (tmp
        // sibling removed by rename).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        let state = SchedulerState::default();
        state.save_atomic(&path).unwrap();
        assert!(path.exists());
        let tmp_file = tmp_sibling(&path);
        assert!(
            !tmp_file.exists(),
            "tmp sibling {} should be gone after rename",
            tmp_file.display()
        );
    }

    #[test]
    fn malformed_json_surfaces_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("corrupt.json");
        fs::write(&path, "{not json").unwrap();
        let err = SchedulerState::load_or_default(&path).expect_err("parse fails");
        assert!(matches!(err, StateError::Parse(_)));
    }

    #[test]
    fn patrol_mut_inserts_default() {
        let mut state = SchedulerState::default();
        let entry = state.patrol_mut("fresh");
        entry.fire_count = 1;
        assert_eq!(state.patrols.get("fresh").unwrap().fire_count, 1);
    }

    #[test]
    fn sunset_decided_at_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sunset.json");
        let decided = Utc.with_ymd_and_hms(2026, 4, 19, 15, 0, 0).unwrap();

        let mut state = SchedulerState::default();
        state.patrols.insert(
            "u2-probe".to_owned(),
            PatrolState {
                last_fired_at: None,
                last_exit_code: None,
                last_pid: None,
                fire_count: 0,
                sunset_decided_at: Some(decided),
            },
        );
        state.save_atomic(&path).expect("save ok");

        let loaded = SchedulerState::load_or_default(&path).expect("load ok");
        assert_eq!(
            loaded.patrols.get("u2-probe").unwrap().sunset_decided_at,
            Some(decided)
        );
    }

    #[test]
    fn unknown_future_fields_are_accepted() {
        // Forward-compat: a newer scheduler may add fields; older
        // readers must not choke.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("future.json");
        fs::write(
            &path,
            r#"{
                "version": 2,
                "patrols": {
                    "p": { "last_fired_at": null, "future_field": "ignored" }
                },
                "new_top_level": "also-ignored"
            }"#,
        )
        .unwrap();
        let loaded = SchedulerState::load_or_default(&path).expect("tolerant load");
        assert_eq!(loaded.version, 2);
        assert!(loaded.patrols.contains_key("p"));
    }
}
