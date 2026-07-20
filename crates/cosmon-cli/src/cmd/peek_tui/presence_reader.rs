// SPDX-License-Identifier: AGPL-3.0-only

//! Presence reader — best-effort scan of `.cosmon/state/presence/<sid>.json`.
//!
//! The presence core owns the canonical schema and the
//! `cs presence ping|ls|gc` verbs. This module is a **local reader stub** so
//! `cs peek` can show a presence header strip before the core type lands. It
//! reads whatever fields it can parse and degrades silently on anything it
//! does not recognise — there is no schema enforcement here, by design.
//!
//! When `cosmon-core::presence::Presence` lands, this file should be
//! replaced by a thin adapter over that type (the JSON layout below is the
//! defined contract).
//!
//! File layout (per `.cosmon/state/presence/<sid>.json`):
//!
//! ```json
//! {
//!   "sid": "cosmon-main-1b2c",
//!   "heartbeat_at": "2026-04-24T16:00:00Z",
//!   "galaxy": "cosmon",
//!   "cwd": "~/galaxies/cosmon",
//!   "pid": 12345,
//!   "current_molecule": "task-20260424-abcd",
//!   "headline": "implementing C-PEEK-ENSEMBLE"
//! }
//! ```

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};

/// A single live Claude session, as reported by its presence file.
///
/// All optional fields degrade gracefully when the on-disk JSON is missing
/// or malformed — the reader never panics or surfaces an error to the TUI.
#[derive(Debug, Clone)]
pub(crate) struct PresenceEntry {
    pub(crate) sid: String,
    pub(crate) galaxy: Option<String>,
    pub(crate) headline: Option<String>,
    #[allow(dead_code)]
    pub(crate) current_molecule: Option<String>,
    pub(crate) heartbeat_at: Option<DateTime<Utc>>,
}

impl PresenceEntry {
    /// Seconds since the last heartbeat, if known.
    pub(crate) fn age(&self, now: DateTime<Utc>) -> Option<i64> {
        self.heartbeat_at
            .map(|h| now.signed_duration_since(h).num_seconds().max(0))
    }

    /// Stale if heartbeat is older than 3 min (per briefing acceptance).
    pub(crate) fn is_stale(&self, now: DateTime<Utc>) -> bool {
        self.age(now)
            .is_some_and(|s| s > Duration::minutes(3).num_seconds())
    }
}

/// Scan the presence directory and return every parseable entry.
///
/// Returns an empty vector if the directory does not exist. Never errors:
/// presence is advisory and must not block the TUI.
pub(crate) fn scan(state_dir: &Path) -> Vec<PresenceEntry> {
    let dir = state_dir.join("presence");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Some(p) = parse_file(&path) {
            out.push(p);
        }
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.heartbeat_at));
    out
}

/// Scan presence across every `.cosmon/state/` under `~/galaxies/*/`.
///
/// Mirrors `cs peek --all` semantics: same trick (enumerate galaxies), same
/// best-effort silence on error.
pub(crate) fn scan_all_galaxies() -> Vec<PresenceEntry> {
    let galaxies_root: PathBuf = match dirs::home_dir() {
        Some(h) => h.join("galaxies"),
        None => return Vec::new(),
    };
    let Ok(read) = std::fs::read_dir(&galaxies_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let state = entry.path().join(".cosmon").join("state");
        if state.exists() {
            out.extend(scan(&state));
        }
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.heartbeat_at));
    out
}

fn parse_file(path: &Path) -> Option<PresenceEntry> {
    let body = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let obj = v.as_object()?;
    let sid = obj.get("sid").and_then(|s| s.as_str()).map_or_else(
        || {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("<sid?>")
                .to_owned()
        },
        str::to_owned,
    );
    Some(PresenceEntry {
        sid,
        galaxy: obj
            .get("galaxy")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        headline: obj
            .get("headline")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        current_molecule: obj
            .get("current_molecule")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        heartbeat_at: obj
            .get("heartbeat_at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn scan_returns_empty_when_dir_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(scan(tmp.path()).is_empty());
    }

    #[test]
    fn scan_parses_valid_presence_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("presence");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("cosmon-main-1b2c.json"),
            r#"{"sid":"cosmon-main-1b2c","galaxy":"cosmon","headline":"synthesising","heartbeat_at":"2026-04-24T16:00:00Z"}"#,
        )
        .unwrap();
        let out = scan(tmp.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sid, "cosmon-main-1b2c");
        assert_eq!(out[0].galaxy.as_deref(), Some("cosmon"));
    }

    #[test]
    fn scan_ignores_malformed_files() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("presence");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.json"), "not json").unwrap();
        assert!(scan(tmp.path()).is_empty());
    }

    #[test]
    fn is_stale_true_past_three_minutes() {
        let now = Utc::now();
        let p = PresenceEntry {
            sid: "x".into(),
            galaxy: None,
            headline: None,
            current_molecule: None,
            heartbeat_at: Some(now - Duration::minutes(5)),
        };
        assert!(p.is_stale(now));
    }

    #[test]
    fn is_stale_false_within_three_minutes() {
        let now = Utc::now();
        let p = PresenceEntry {
            sid: "x".into(),
            galaxy: None,
            headline: None,
            current_molecule: None,
            heartbeat_at: Some(now - Duration::seconds(30)),
        };
        assert!(!p.is_stale(now));
    }
}
