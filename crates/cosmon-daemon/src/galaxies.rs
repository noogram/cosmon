// SPDX-License-Identifier: AGPL-3.0-only

//! Galaxy discovery — scans `<root>/<galaxy>/.cosmon/state/` and yields
//! one [`GalaxyEntry`] per directory that looks like a cosmon-managed
//! galaxy.
//!
//! The caller passes the *galaxies root* (typically `~/galaxies/`); each
//! immediate subdirectory whose `.cosmon/state/` exists qualifies. A
//! single pass over the directory keeps the listing endpoint cheap
//! enough to call from the iOS polling loop without a cache.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// One discovered galaxy.
#[derive(Debug, Clone, Serialize)]
pub struct GalaxyEntry {
    /// Directory name under the galaxies root (e.g. `cosmon`,
    /// `mailroom`).
    pub name: String,
    /// Absolute path to the galaxy root (the directory that contains
    /// `.cosmon/`).
    pub path: PathBuf,
    /// Absolute path to the galaxy's `.cosmon/state/` directory.
    pub state_dir: PathBuf,
}

/// Walk `root` and return every immediate child that carries a
/// `.cosmon/state/` directory. The result is sorted by name so the API
/// is deterministic.
#[must_use]
pub fn discover_galaxies(root: &Path) -> Vec<GalaxyEntry> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<GalaxyEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let ty = entry.file_type().ok()?;
            if !ty.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Skip dotted children (~/galaxies/.cache, etc.).
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let state_dir = path.join(".cosmon").join("state");
            if !state_dir.is_dir() {
                return None;
            }
            Some(GalaxyEntry {
                name,
                path,
                state_dir,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Resolve one galaxy by name. `None` if no such galaxy exists or it
/// has no `.cosmon/state/` directory.
#[must_use]
pub fn find_galaxy(root: &Path, name: &str) -> Option<GalaxyEntry> {
    // Reject anything that could escape the galaxies root.
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let path = root.join(name);
    let state_dir = path.join(".cosmon").join("state");
    if !state_dir.is_dir() {
        return None;
    }
    Some(GalaxyEntry {
        name: name.to_owned(),
        path,
        state_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_galaxies_with_state_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("alpha/.cosmon/state")).unwrap();
        std::fs::create_dir_all(tmp.path().join("beta/.cosmon/state")).unwrap();
        std::fs::create_dir_all(tmp.path().join("gamma/.cosmon")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".dotted/.cosmon/state")).unwrap();

        let found = discover_galaxies(tmp.path());
        let names: Vec<_> = found.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn returns_empty_on_missing_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        assert!(discover_galaxies(&missing).is_empty());
    }

    #[test]
    fn find_galaxy_rejects_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("alpha/.cosmon/state")).unwrap();
        assert!(find_galaxy(tmp.path(), "../etc").is_none());
        assert!(find_galaxy(tmp.path(), "alpha/..").is_none());
        assert!(find_galaxy(tmp.path(), "").is_none());
        assert!(find_galaxy(tmp.path(), "alpha").is_some());
    }
}
