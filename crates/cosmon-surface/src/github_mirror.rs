// SPDX-License-Identifier: AGPL-3.0-only

//! GitHub Issues local mirror — tracks projected state for 3-way sync.
//!
//! Each projected GitHub Issue gets a local JSON file that records what
//! was last sent. This enables:
//! - Offline inspection: see what's on GitHub without API calls
//! - 3-way merge: local mirror ↔ GitHub reality ↔ molecule source
//! - Idempotent updates: skip issues that haven't changed

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Mirror of a single GitHub Issue projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueMirror {
    /// The cosmon molecule ID this issue was projected from.
    pub molecule_id: String,
    /// GitHub issue number.
    pub issue_number: u64,
    /// GitHub repo (owner/repo).
    pub repo: String,
    /// Title that was projected.
    pub title: String,
    /// SHA-256 hash of the body that was projected.
    pub body_hash: String,
    /// GitHub issue state: "open" or "closed".
    pub state: String,
    /// Molecule kind at projection time.
    pub kind: String,
    /// Molecule status at projection time.
    pub status: String,
    /// When this was last projected.
    pub projected_at: String,
}

/// Root directory for GitHub mirrors.
fn mirror_dir(state_dir: &Path, repo: &str) -> PathBuf {
    let safe_repo = repo.replace('/', "-");
    state_dir.join("surfaces").join("github").join(safe_repo)
}

/// Path to a specific issue mirror.
fn mirror_path(state_dir: &Path, repo: &str, molecule_id: &str) -> PathBuf {
    mirror_dir(state_dir, repo).join(format!("{molecule_id}.json"))
}

/// Load an existing mirror for a molecule.
#[must_use]
pub fn load_mirror(state_dir: &Path, repo: &str, molecule_id: &str) -> Option<IssueMirror> {
    let path = mirror_path(state_dir, repo, molecule_id);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Save a mirror after projection.
///
/// # Errors
/// Returns an error if the file cannot be written.
pub fn save_mirror(
    state_dir: &Path,
    mirror: &IssueMirror,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = mirror_dir(state_dir, &mirror.repo);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", mirror.molecule_id));
    let json = serde_json::to_string_pretty(mirror)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Load all mirrors for a repo.
#[must_use]
pub fn load_all_mirrors(state_dir: &Path, repo: &str) -> HashMap<String, IssueMirror> {
    let dir = mirror_dir(state_dir, repo);
    let mut mirrors = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(mirror) = serde_json::from_str::<IssueMirror>(&content) {
                    mirrors.insert(mirror.molecule_id.clone(), mirror);
                }
            }
        }
    }
    mirrors
}

/// Compute SHA-256 hash of content.
#[must_use]
pub fn hash_content(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mirror_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = IssueMirror {
            molecule_id: "idea-20260407-abcd".to_string(),
            issue_number: 42,
            repo: "noogram/cosmon".to_string(),
            title: "💡 [idea] Test".to_string(),
            body_hash: "abc123".to_string(),
            state: "open".to_string(),
            kind: "idea".to_string(),
            status: "pending".to_string(),
            projected_at: "2026-04-07T20:00:00Z".to_string(),
        };
        save_mirror(tmp.path(), &mirror).unwrap();

        let loaded = load_mirror(tmp.path(), "noogram/cosmon", "idea-20260407-abcd").unwrap();
        assert_eq!(loaded.issue_number, 42);
        assert_eq!(loaded.title, "💡 [idea] Test");
    }

    #[test]
    fn test_load_all_mirrors() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            let mirror = IssueMirror {
                molecule_id: format!("mol-{i}"),
                issue_number: i,
                repo: "test/repo".to_string(),
                title: format!("Issue {i}"),
                body_hash: String::new(),
                state: "open".to_string(),
                kind: "task".to_string(),
                status: "pending".to_string(),
                projected_at: String::new(),
            };
            save_mirror(tmp.path(), &mirror).unwrap();
        }

        let all = load_all_mirrors(tmp.path(), "test/repo");
        assert_eq!(all.len(), 3);
    }
}
