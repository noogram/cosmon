// SPDX-License-Identifier: Apache-2.0

//! Session log discovery on the filesystem.
//!
//! Claude Code stores session logs at `~/.claude/projects/{encoded-path}/{uuid}.jsonl`.
//! This module walks that directory tree and returns lightweight [`SessionPath`]
//! handles that can be filtered before committing to a full parse.

use std::path::{Path, PathBuf};

use crate::energy::SessionId;

use crate::error::ClaudionError;
use crate::types::SessionPath;

/// The default base path where Claude Code stores session logs.
///
/// Returns `$HOME/.claude/projects`.
///
/// # Panics
///
/// Panics if the `HOME` environment variable is not set.
#[must_use]
pub fn default_base_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable not set");
    PathBuf::from(home).join(".claude").join("projects")
}

/// Discover all session JSONL files under a base path.
///
/// Walks `{base_path}/{project-dir}/` looking for `*.jsonl` files.
/// Returns a [`SessionPath`] for each discovered log, sorted by path.
///
/// # Errors
///
/// Returns [`ClaudionError::Io`] if the base path cannot be read.
/// Returns [`ClaudionError::NoSessions`] if no JSONL files are found.
pub fn discover_sessions(base_path: impl AsRef<Path>) -> Result<Vec<SessionPath>, ClaudionError> {
    let base = base_path.as_ref();

    if !base.is_dir() {
        return Err(ClaudionError::NoSessions {
            path: base.to_path_buf(),
        });
    }

    let mut sessions = Vec::new();

    let projects = std::fs::read_dir(base).map_err(|e| ClaudionError::Io {
        path: base.to_path_buf(),
        source: e,
    })?;

    for project_entry in projects {
        let project_entry = project_entry.map_err(|e| ClaudionError::Io {
            path: base.to_path_buf(),
            source: e,
        })?;

        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        let project_name = project_entry.file_name().to_string_lossy().to_string();

        let Ok(entries) = std::fs::read_dir(&project_path) else {
            continue; // Skip unreadable directories.
        };

        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();

            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            let Ok(session_id) = SessionId::new(stem.to_string()) else {
                continue;
            };

            sessions.push(SessionPath {
                path: path.clone(),
                session_id,
                project: project_name.clone(),
            });
        }
    }

    if sessions.is_empty() {
        return Err(ClaudionError::NoSessions {
            path: base.to_path_buf(),
        });
    }

    sessions.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(sessions)
}

/// Discover sessions for a specific project.
///
/// Filters the base discovery to only return sessions whose project name
/// contains the given substring (case-sensitive).
///
/// # Errors
///
/// Same as [`discover_sessions`].
pub fn discover_project_sessions(
    base_path: impl AsRef<Path>,
    project_filter: &str,
) -> Result<Vec<SessionPath>, ClaudionError> {
    let all = discover_sessions(base_path)?;
    let filtered: Vec<_> = all
        .into_iter()
        .filter(|s| s.project.contains(project_filter))
        .collect();

    if filtered.is_empty() {
        return Err(ClaudionError::NoSessions {
            path: PathBuf::from(format!("(filtered: {project_filter})")),
        });
    }

    Ok(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_discover_sessions_in_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("-Users-test-project");
        fs::create_dir(&project_dir).unwrap();

        // Create two mock JSONL files.
        fs::write(
            project_dir.join("aaaa-bbbb-cccc.jsonl"),
            r#"{"type":"user"}"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("dddd-eeee-ffff.jsonl"),
            r#"{"type":"user"}"#,
        )
        .unwrap();
        // Non-JSONL file should be ignored.
        fs::write(project_dir.join("notes.txt"), "ignored").unwrap();

        let sessions = discover_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].project, "-Users-test-project");
    }

    #[test]
    fn test_discover_empty_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = discover_sessions(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_discover_project_filter() {
        let dir = tempfile::tempdir().unwrap();

        let p1 = dir.path().join("-Users-test-cosmon");
        let p2 = dir.path().join("-Users-test-democorp");
        fs::create_dir(&p1).unwrap();
        fs::create_dir(&p2).unwrap();

        fs::write(p1.join("sess1.jsonl"), "{}").unwrap();
        fs::write(p2.join("sess2.jsonl"), "{}").unwrap();

        let sessions = discover_project_sessions(dir.path(), "cosmon").unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].project.contains("cosmon"));
    }
}
