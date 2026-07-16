// SPDX-License-Identifier: AGPL-3.0-only

//! Neurion-backed galaxy index — fallback when the TOML is missing.
//!
//! Reads the `repos` table of the neurion `SQLite` inventory and
//! projects each row into a [`Galaxy`] entry. Fields not present in
//! the neurion schema (`fleet`, `default_formulas`, `claude_md_digest`)
//! are synthesized with safe defaults — the operator can promote the
//! entry to the richer TOML catalog when those fields matter.
//!
//! This backend exists for two reasons:
//!
//! * Bootstrap — until the operator writes
//!   `~/.config/cosmon/galaxies.toml`, neurion already knows every
//!   repo that cosmon has discovered.
//! * Audit — the neurion DB is the machine-observable view of the
//!   workspace; the TOML is the human-declared view. Having both
//!   lets drift-checkers catch divergence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::{Galaxy, GalaxyIndex, RegistryError};

/// Galaxy index materialized from the neurion `SQLite` `repos` table.
#[derive(Debug, Clone)]
pub struct NeurionBackedGalaxyIndex {
    entries: Vec<Galaxy>,
    source: Option<PathBuf>,
}

impl NeurionBackedGalaxyIndex {
    /// Build an empty index (no neurion DB found / error tolerated
    /// upstream). Callers that insist on an error can call
    /// [`Self::load_from`] directly.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            source: None,
        }
    }

    /// Load from the default neurion DB location
    /// (`<data_dir>/neurion/neurion.db`). Returns an empty index when
    /// the DB does not exist yet (fresh environment).
    ///
    /// # Errors
    ///
    /// [`RegistryError::Backend`] if the DB file exists but the
    /// `SQLite` handle cannot be opened.
    pub fn load_default() -> Result<Self, RegistryError> {
        let Some(path) = default_path() else {
            return Ok(Self::empty());
        };
        if !path.exists() {
            return Ok(Self {
                entries: Vec::new(),
                source: Some(path),
            });
        }
        Self::load_from(&path)
    }

    /// Load from an explicit `SQLite` path. Missing or malformed
    /// tables collapse to an empty index rather than an error —
    /// legacy or pre-migration DBs must not poison the fallback.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Backend`] only when the `SQLite` handle itself
    /// cannot be opened; query failures are swallowed to an empty
    /// index.
    pub fn load_from(path: &Path) -> Result<Self, RegistryError> {
        let conn = Connection::open(path).map_err(|e| RegistryError::Backend(e.to_string()))?;
        let entries = read_repos(&conn).unwrap_or_default();
        Ok(Self {
            entries,
            source: Some(path.to_path_buf()),
        })
    }

    /// Path the index was loaded from, if any.
    #[must_use]
    pub fn source_path(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

impl GalaxyIndex for NeurionBackedGalaxyIndex {
    fn resolve(&self, name: &str) -> Option<Galaxy> {
        self.entries.iter().find(|g| g.name == name).cloned()
    }

    fn list(&self) -> Vec<Galaxy> {
        self.entries.clone()
    }
}

fn default_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("neurion").join("neurion.db"))
}

fn read_repos(conn: &Connection) -> Result<Vec<Galaxy>, rusqlite::Error> {
    // Probe for the expected shape; pre-migration DBs without a
    // `path` column fall through to a name-only projection.
    let has_path = conn.prepare("SELECT name, path FROM repos LIMIT 1").is_ok();
    let sql = if has_path {
        "SELECT name, path FROM repos ORDER BY name"
    } else {
        "SELECT name, NULL as path FROM repos ORDER BY name"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let path: Option<String> = row.get(1)?;
        Ok(Galaxy {
            name,
            path: path.map_or_else(PathBuf::new, PathBuf::from),
            fleet: "default".to_owned(),
            claude_md_digest: None,
            default_formulas: HashMap::new(),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_backend_resolves_nothing() {
        let idx = NeurionBackedGalaxyIndex::empty();
        assert!(idx.resolve("mailroom").is_none());
        assert!(idx.list().is_empty());
    }

    #[test]
    fn missing_db_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("no.db");
        // load_from opens the connection; SQLite creates a file at
        // `path` on open, but the `repos` table won't exist, so
        // read_repos fails, which we swallow into an empty index.
        let idx = NeurionBackedGalaxyIndex::load_from(&nonexistent).unwrap();
        assert!(idx.list().is_empty());
    }

    #[test]
    fn reads_a_synthetic_repos_table() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("n.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE repos (name TEXT PRIMARY KEY, path TEXT);
             INSERT INTO repos (name, path) VALUES ('cosmon', '/tmp/cosmon');
             INSERT INTO repos (name, path) VALUES ('mailroom', '/tmp/sec');",
        )
        .unwrap();
        drop(conn);

        let idx = NeurionBackedGalaxyIndex::load_from(&db).unwrap();
        let g = idx.resolve("cosmon").unwrap();
        assert_eq!(g.path, PathBuf::from("/tmp/cosmon"));
        assert_eq!(g.fleet, "default");
        assert_eq!(idx.list().len(), 2);
    }

    #[test]
    fn list_and_resolve_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("n.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE repos (name TEXT PRIMARY KEY, path TEXT);
             INSERT INTO repos (name, path) VALUES ('a', '/a');
             INSERT INTO repos (name, path) VALUES ('b', '/b');",
        )
        .unwrap();
        drop(conn);

        let idx = NeurionBackedGalaxyIndex::load_from(&db).unwrap();
        for g in idx.list() {
            assert_eq!(idx.resolve(&g.name), Some(g));
        }
    }
}
