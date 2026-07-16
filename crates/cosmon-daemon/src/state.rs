// SPDX-License-Identifier: AGPL-3.0-only

//! Shared application state — galaxies root + small caches.
//!
//! The router holds an `Arc<AppState>` so each request can resolve a
//! galaxy by name in O(1) (a `HashMap` keyed on name) without a fresh
//! filesystem scan. The list endpoint still re-scans on demand, so
//! galaxies created at runtime show up without restarting the daemon.

use std::path::{Path, PathBuf};

use crate::galaxies::{discover_galaxies, GalaxyEntry};

/// Newtype wrapper around the `/srv/cosmon/` root directory.
#[derive(Debug, Clone)]
pub struct GalaxiesRoot(pub PathBuf);

impl GalaxiesRoot {
    /// Resolve the galaxies root from `$COSMON_GALAXIES_ROOT` if set,
    /// otherwise fall back to `$HOME/galaxies/`.
    #[must_use]
    pub fn from_env() -> Self {
        if let Ok(v) = std::env::var("COSMON_GALAXIES_ROOT") {
            if !v.trim().is_empty() {
                return Self(PathBuf::from(v));
            }
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self(home.join("galaxies"))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Shared application state.
#[derive(Debug)]
pub struct AppState {
    /// Filesystem root containing one directory per galaxy.
    pub root: GalaxiesRoot,
}

impl AppState {
    #[must_use]
    pub fn new(root: GalaxiesRoot) -> Self {
        Self { root }
    }

    /// Re-scan and return the current galaxy listing.
    #[must_use]
    pub fn list_galaxies(&self) -> Vec<GalaxyEntry> {
        discover_galaxies(self.root.as_path())
    }

    /// Resolve a galaxy by name, with traversal protection.
    #[must_use]
    pub fn galaxy(&self, name: &str) -> Option<GalaxyEntry> {
        crate::galaxies::find_galaxy(self.root.as_path(), name)
    }
}
