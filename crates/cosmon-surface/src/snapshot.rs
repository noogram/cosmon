// SPDX-License-Identifier: AGPL-3.0-only

//! Surface projection snapshots — tracks the last projected state.
//!
//! Like git's index (staging area), the snapshot records what was last
//! projected to each surface. This enables 3-way conflict detection:
//!
//! ```text
//! Last projected (snapshot)
//!        ├── Current file content  → if different: human edited
//!        └── Current rendered content → if different: source changed
//!
//! Both different = CONFLICT (needs cognitive reconciliation)
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Snapshot of the last projection — stored at `.cosmon/state/surfaces.snapshot.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectionSnapshot {
    /// Per-surface hash of last projected content.
    pub surfaces: HashMap<String, SurfaceSnapshot>,
}

/// Snapshot for a single surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceSnapshot {
    /// SHA-256 hash of the content that was last written to this surface.
    pub content_hash: String,
    /// Timestamp of last projection.
    pub projected_at: String,
}

/// Result of comparing current state against the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceDivergence {
    /// Surface matches snapshot — no changes anywhere.
    UpToDate,
    /// Source changed but surface untouched → safe to overwrite.
    SourceChanged,
    /// Surface was edited by human but source unchanged → human wins.
    SurfaceEdited,
    /// Both changed → conflict, needs resolution.
    Conflict,
    /// No snapshot exists (first projection) → safe to write.
    NeverProjected,
}

impl SurfaceDivergence {
    /// Emoji for display.
    #[must_use]
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::UpToDate => "✅",
            Self::SourceChanged => "🔄",
            Self::SurfaceEdited => "✏️",
            Self::Conflict => "⚠️",
            Self::NeverProjected => "📝",
        }
    }

    /// Whether it's safe to overwrite the surface.
    #[must_use]
    pub fn safe_to_write(&self) -> bool {
        matches!(
            self,
            Self::UpToDate | Self::SourceChanged | Self::NeverProjected
        )
    }
}

/// Compute SHA-256 hash of content.
fn hash_content(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Detect divergence for a surface.
///
/// - `snapshot_hash`: hash from last projection (None if never projected)
/// - `current_file`: current content of the surface file on disk
/// - `new_rendered`: what we would write now from source state
#[must_use]
pub fn detect_divergence(
    snapshot_hash: Option<&str>,
    current_file: &str,
    new_rendered: &str,
) -> SurfaceDivergence {
    let Some(snapshot) = snapshot_hash else {
        return SurfaceDivergence::NeverProjected;
    };

    let file_hash = hash_content(current_file);
    let rendered_hash = hash_content(new_rendered);

    let file_matches_snapshot = file_hash == snapshot;
    let rendered_matches_snapshot = rendered_hash == snapshot;

    match (file_matches_snapshot, rendered_matches_snapshot) {
        (true, true) => SurfaceDivergence::UpToDate,
        (true, false) => SurfaceDivergence::SourceChanged,
        (false, true) => SurfaceDivergence::SurfaceEdited,
        (false, false) => SurfaceDivergence::Conflict,
    }
}

/// Path to the snapshot file.
#[must_use]
pub fn snapshot_path(state_dir: &Path) -> PathBuf {
    state_dir.join("surfaces.snapshot.json")
}

/// Load the snapshot from disk.
#[must_use]
pub fn load_snapshot(state_dir: &Path) -> ProjectionSnapshot {
    let path = snapshot_path(state_dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Save the snapshot to disk.
///
/// # Errors
/// Returns an error if the file cannot be written.
pub fn save_snapshot(
    state_dir: &Path,
    snapshot: &ProjectionSnapshot,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = snapshot_path(state_dir);
    let json = serde_json::to_string_pretty(snapshot)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Record a surface projection in the snapshot.
pub fn record_projection(snapshot: &mut ProjectionSnapshot, surface_path: &str, content: &str) {
    snapshot.surfaces.insert(
        surface_path.to_string(),
        SurfaceSnapshot {
            content_hash: hash_content(content),
            projected_at: chrono::Utc::now().to_rfc3339(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_up_to_date() {
        let content = "# Status\nAll good.";
        let hash = hash_content(content);
        assert_eq!(
            detect_divergence(Some(&hash), content, content),
            SurfaceDivergence::UpToDate
        );
    }

    #[test]
    fn test_source_changed() {
        let old = "# Status\nOld.";
        let hash = hash_content(old);
        let new_rendered = "# Status\nNew molecule added.";
        assert_eq!(
            detect_divergence(Some(&hash), old, new_rendered),
            SurfaceDivergence::SourceChanged
        );
    }

    #[test]
    fn test_surface_edited() {
        let original = "# Status\nOriginal.";
        let hash = hash_content(original);
        let edited = "# Status\nOriginal.\n\n## Added by human";
        assert_eq!(
            detect_divergence(Some(&hash), edited, original),
            SurfaceDivergence::SurfaceEdited
        );
    }

    #[test]
    fn test_conflict() {
        let original = "# Status\nOriginal.";
        let hash = hash_content(original);
        let edited = "# Status\nHuman edit.";
        let new_rendered = "# Status\nSource changed.";
        assert_eq!(
            detect_divergence(Some(&hash), edited, new_rendered),
            SurfaceDivergence::Conflict
        );
    }

    #[test]
    fn test_never_projected() {
        assert_eq!(
            detect_divergence(None, "", "new content"),
            SurfaceDivergence::NeverProjected
        );
    }
}
