// SPDX-License-Identifier: Apache-2.0

//! Auto-registration hints for the neurion registry.
//!
//! When an external tool (cosmon, oxymake, …) operates inside a repo that
//! neurion does not yet know about, it appends a line to
//! `~/.local/share/neurion/auto-register.jsonl` (platform data dir).
//! On its next boot, `neurion-mcp` drains that file and upserts the entries
//! into the `repos` table.
//!
//! This module is I/O-free. It only defines:
//!
//! - [`AutoRegisterHint`], the serialized payload written by emitters;
//! - [`AUTO_REGISTER_FILENAME`], the canonical filename;
//! - [`AUTO_REGISTER_ENV`], the env override used by tests;
//! - [`is_excluded_repo_path`], the predicate that prevents recursive
//!   registration loops (git worktrees, `.git/` internals).
//!
//! All filesystem work happens in the caller (`cosmon-cli` to emit,
//! `neurion-mcp` to drain).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Canonical filename inside the neurion data directory.
pub const AUTO_REGISTER_FILENAME: &str = "auto-register.jsonl";

/// Environment variable that, when set, overrides the auto-register file
/// path. Primarily used by tests to keep hints out of the user's real
/// data directory.
pub const AUTO_REGISTER_ENV: &str = "NEURION_AUTO_REGISTER_FILE";

/// A pending repo registration appended to the auto-register JSONL file.
///
/// Each line of the file is one serialized `AutoRegisterHint`. The reader
/// (neurion-mcp on boot) upserts into `repos` keyed by `name`, so
/// duplicates are idempotent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoRegisterHint {
    /// Primary key for the `repos` table — the directory basename of
    /// `local_path`.
    pub name: String,
    /// Absolute path to the repo's working root (the directory containing
    /// the real `.git` directory, not a worktree).
    pub local_path: String,
    /// `git remote get-url origin`, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Free-form identifier for the emitter (e.g. `"cosmon:nucleate"`).
    /// Not written to the registry — kept for debugging / audit.
    pub source: String,
    /// RFC3339 timestamp when the hint was emitted.
    pub detected_at: String,
}

/// Return `true` if `path` is inside a context that must never be
/// auto-registered. This is the recursion guard: without it, a cosmon
/// worker operating inside `<repo>/.worktrees/task-*` would register its
/// own worktree path as a separate repo, and the next invocation in that
/// same worktree would do it again.
///
/// The rule is conservative — any path component equal to `.worktrees` or
/// `.git` disqualifies the whole path. We do NOT attempt to resolve
/// symlinks or read worktree metadata; callers should canonicalize their
/// input before calling this function if that matters.
pub fn is_excluded_repo_path(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == ".worktrees" || s == ".git"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn is_excluded_detects_worktree() {
        let p = PathBuf::from("/home/user/dev/cosmon/.worktrees/task-1");
        assert!(is_excluded_repo_path(&p));
    }

    #[test]
    fn is_excluded_detects_git_internals() {
        let p = PathBuf::from("/home/user/dev/cosmon/.git/worktrees/foo");
        assert!(is_excluded_repo_path(&p));
    }

    #[test]
    fn is_excluded_allows_normal_repo() {
        let p = PathBuf::from("/home/user/dev/cosmon");
        assert!(!is_excluded_repo_path(&p));
    }

    #[test]
    fn hint_roundtrips_through_json() {
        let hint = AutoRegisterHint {
            name: "cosmon".into(),
            local_path: "/home/user/dev/cosmon".into(),
            remote_url: Some("git@github.com:noogram/cosmon.git".into()),
            source: "cosmon:nucleate".into(),
            detected_at: "2026-04-13T00:00:00Z".into(),
        };
        let line = serde_json::to_string(&hint).unwrap();
        let parsed: AutoRegisterHint = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed, hint);
    }
}
