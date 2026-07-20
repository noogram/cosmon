// SPDX-License-Identifier: AGPL-3.0-only

//! Tenant-cwd workspace fixtures — the §3.5 clause (e) substrate.
//!
//! A `cs` subprocess admitted by the RPP runs with `cwd =
//! ~/galaxies/<noyau>/`. Every test that exercises the subprocess
//! envelope therefore needs a `TempDir`-backed `galaxies/` tree with at
//! least one populated noyau. This module provides two shapes:
//!
//! - [`tenant_workspace`] — single-noyau convenience. The returned
//!   [`TenantWorkspace`] wraps the [`TempDir`] (cleaned on drop) and
//!   exposes the absolute paths the test will need.
//! - [`TenantWorkspaces`] — multi-noyau case, where the canonical
//!   `noyau-A-cannot-read-noyau-B` test plants identical molecule ids
//!   in two parallel tenants and proves the JWT for noyau A cannot
//!   reach noyau B's state.
//!
//! Neither shape writes to `.cosmon/state/` outside the `TempDir`; both
//! are safe to run in parallel.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tempfile::TempDir;

/// Single-tenant workspace. See module documentation for the layout.
pub struct TenantWorkspace {
    /// `TempDir` is RAII — drop cleans up the tree. Held privately to
    /// keep the public surface focused on paths.
    _tempdir: TempDir,
    galaxies_root: PathBuf,
    tenant: TenantPath,
}

impl TenantWorkspace {
    /// Provision a single-noyau workspace. Convenience wrapper around
    /// [`TenantWorkspaces::new`] + [`TenantWorkspaces::add`].
    ///
    /// # Panics
    ///
    /// Panics if a fresh `TempDir` cannot be created or if filesystem
    /// permissions prevent populating the tree (both extremely
    /// unusual conditions in CI sandboxes).
    #[must_use]
    pub fn new(noyau: &str) -> Self {
        let mut set = TenantWorkspaces::new();
        let path = set.add(noyau);
        let TenantWorkspaces {
            tempdir,
            galaxies_root,
            ..
        } = set;
        Self {
            _tempdir: tempdir,
            galaxies_root,
            tenant: path,
        }
    }

    /// Path the subprocess invoker passes via `galaxies_root`. Format:
    /// `<tempdir>/galaxies/`.
    #[must_use]
    pub fn galaxies_root(&self) -> &Path {
        &self.galaxies_root
    }

    /// Tenant scope (the `noyau` directory below `galaxies_root`).
    #[must_use]
    pub fn noyau(&self) -> &str {
        &self.tenant.noyau
    }

    /// Absolute path of `~/galaxies/<noyau>/` inside the `TempDir`.
    #[must_use]
    pub fn tenant_root(&self) -> &Path {
        &self.tenant.root
    }

    /// Absolute path of `~/galaxies/<noyau>/.cosmon/state/`.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.tenant.state_dir
    }

    /// Full handle to the tenant layout (`state_dir`, `root`, `noyau`).
    #[must_use]
    pub fn tenant(&self) -> &TenantPath {
        &self.tenant
    }
}

impl std::fmt::Debug for TenantWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The held TempDir is intentionally excluded — its Debug
        // value is opaque and meaningless to test diagnostics.
        f.debug_struct("TenantWorkspace")
            .field("galaxies_root", &self.galaxies_root)
            .field("tenant", &self.tenant)
            .finish_non_exhaustive()
    }
}

/// Multi-noyau workspace fixture. All noyaus share a single `TempDir`
/// and a single `galaxies_root`, so the subprocess invoker can resolve
/// every tenant from one path. Each call to [`Self::add`] returns the
/// freshly-created [`TenantPath`]; calls are idempotent in name (a
/// repeated `add` returns the existing entry).
pub struct TenantWorkspaces {
    tempdir: TempDir,
    galaxies_root: PathBuf,
    tenants: Vec<TenantPath>,
}

impl TenantWorkspaces {
    /// Allocate the `TempDir` + `galaxies/` parent. No tenants yet.
    #[must_use]
    pub fn new() -> Self {
        let tempdir = TempDir::new().expect("create TempDir for TenantWorkspaces");
        let galaxies_root = tempdir.path().join("galaxies");
        std::fs::create_dir_all(&galaxies_root).expect("create galaxies/ root");
        Self {
            tempdir,
            galaxies_root,
            tenants: Vec::new(),
        }
    }

    /// Path to the shared `~/galaxies/` parent directory (the value to
    /// pass as `galaxies_root` to `cosmon-rpp-adapter::AppState`).
    #[must_use]
    pub fn galaxies_root(&self) -> &Path {
        &self.galaxies_root
    }

    /// Provision a noyau under the shared `galaxies/` parent. Returns
    /// the populated layout. If the noyau already exists, returns the
    /// existing entry rather than panicking.
    ///
    /// # Panics
    ///
    /// Panics if the directory tree cannot be created (e.g. permission
    /// denied on the `TempDir`'s parent).
    pub fn add(&mut self, noyau: &str) -> TenantPath {
        if let Some(existing) = self.tenants.iter().find(|t| t.noyau == noyau) {
            return existing.clone();
        }
        let root = self.galaxies_root.join(noyau);
        let state_dir = root.join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).expect("create tenant .cosmon/state");
        // A faux molecules/ subdir signals to the fake-cs binary that
        // this is a real cosmon root — without it `cs observe` would
        // refuse to resolve any molecule.
        std::fs::create_dir_all(state_dir.join("molecules")).expect("create molecules/ stub");
        let tenant = TenantPath {
            noyau: noyau.to_owned(),
            root,
            state_dir,
        };
        self.tenants.push(tenant.clone());
        self.tenants
            .last()
            .cloned()
            .expect("just-pushed tenant is the last entry")
    }

    /// Look up a previously-added tenant by `noyau` name.
    #[must_use]
    pub fn tenant(&self, noyau: &str) -> Option<&TenantPath> {
        self.tenants.iter().find(|t| t.noyau == noyau)
    }

    /// Borrow the list of provisioned tenants.
    #[must_use]
    pub fn tenants(&self) -> &[TenantPath] {
        &self.tenants
    }
}

impl Default for TenantWorkspaces {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TenantWorkspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantWorkspaces")
            .field("galaxies_root", &self.galaxies_root)
            .field("tenants", &self.tenants)
            .field("tempdir", &self.tempdir.path())
            .finish()
    }
}

/// Layout of a single noyau under [`TenantWorkspaces::galaxies_root`].
#[derive(Clone, Debug)]
pub struct TenantPath {
    /// Tenant scope name.
    pub noyau: String,
    /// `<galaxies_root>/<noyau>/`.
    pub root: PathBuf,
    /// `<galaxies_root>/<noyau>/.cosmon/state/`.
    pub state_dir: PathBuf,
}

impl TenantPath {
    /// Plant a molecule that the library-direct
    /// `cosmon_state::ops::observe` verb will resolve when called
    /// against this tenant's `<state_dir>`.
    ///
    /// The state JSON is written to
    /// `<state_dir>/fleets/default/molecules/<id>/state.json` so the
    /// production `cosmon_filestore::FileStore::load_molecule` walks
    /// it without any fleet enumeration shortcut. The body is the
    /// canonical `MoleculeData` shape — a minimal-but-decodable
    /// envelope with sensible defaults — merged with any fields supplied
    /// in `state_value`. Most tests pass `{}` and rely on the defaults;
    /// the few that need to override (`status`, `formula`, `kind`) can
    /// pass a partial JSON object.
    pub fn insert_molecule(
        &self,
        molecule_id: &str,
        state_value: &Value,
    ) -> std::io::Result<PathBuf> {
        let mol_dir = self
            .state_dir
            .join("fleets")
            .join("default")
            .join("molecules")
            .join(molecule_id);
        std::fs::create_dir_all(&mol_dir)?;
        let path = mol_dir.join("state.json");
        let body = make_molecule_envelope(molecule_id, state_value);
        std::fs::write(&path, serde_json::to_vec(&body)?)?;
        Ok(path)
    }
}

impl TenantPath {
    /// Plant a formula file at
    /// `<state_dir>/../formulas/<name>.formula.toml` so the
    /// library-direct nucleate path can resolve it.
    ///
    /// `body` is the raw TOML text. Most tests pass the minimal
    /// single-step formula returned by `minimal_task_work_formula`.
    pub fn insert_formula(&self, name: &str, body: &str) -> std::io::Result<PathBuf> {
        let formulas_dir = self.root.join(".cosmon").join("formulas");
        std::fs::create_dir_all(&formulas_dir)?;
        let path = formulas_dir.join(format!("{name}.formula.toml"));
        std::fs::write(&path, body)?;
        Ok(path)
    }

    /// Convenience wrapper — installs the canonical "task-work"
    /// minimal formula used by V1 POST tests.
    pub fn install_task_work_formula(&self) -> std::io::Result<PathBuf> {
        self.insert_formula("task-work", minimal_task_work_formula())
    }
}

/// Minimal `task-work.formula.toml` body used by V1 POST integration
/// tests. Single step, no required variables, `task` id prefix.
#[must_use]
pub fn minimal_task_work_formula() -> &'static str {
    r#"
formula = "task-work"
version = 1
description = "minimal task-work formula for tests"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "Implement"
description = "Do the work."
"#
}

/// Build a minimal but decodable `MoleculeData` JSON envelope.
///
/// The canonical `FileStore` layout requires every required field of
/// `MoleculeData` to be present. We supply defaults that match
/// `MoleculeStatus::Pending`, `formula = "task-work"`, no worker, and
/// `total_steps = 1`. Any field present in `overrides` (a JSON object)
/// replaces the default at the top level.
fn make_molecule_envelope(molecule_id: &str, overrides: &Value) -> Value {
    let now = chrono::Utc::now().to_rfc3339();
    let mut body = serde_json::json!({
        "id": molecule_id,
        "fleet_id": "default",
        "formula_id": "task-work",
        "status": "pending",
        "variables": {},
        "assigned_worker": null,
        "created_at": now,
        "updated_at": now,
        "total_steps": 1,
        "current_step": 0,
        "completed_steps": [],
        "collapse_reason": null,
        "collapsed_step": null,
        "links": [],
        "kind": "task",
    });
    if let Value::Object(over) = overrides {
        if let Value::Object(map) = &mut body {
            for (k, v) in over {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    body
}

/// Convenience constructor — see [`TenantWorkspace::new`].
#[must_use]
pub fn tenant_workspace(noyau: &str) -> TenantWorkspace {
    TenantWorkspace::new(noyau)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn single_tenant_layout_under_galaxies_root() {
        let ws = tenant_workspace("a");
        assert_eq!(ws.noyau(), "a");
        assert!(ws.tenant_root().ends_with("galaxies/a"));
        assert!(ws.state_dir().ends_with("galaxies/a/.cosmon/state"));
        assert!(ws.tenant_root().is_dir());
        assert!(ws.state_dir().is_dir());
    }

    #[test]
    fn multi_tenant_share_galaxies_root() {
        let mut set = TenantWorkspaces::new();
        let a = set.add("a");
        let b = set.add("b");
        assert!(a.root.starts_with(set.galaxies_root()));
        assert!(b.root.starts_with(set.galaxies_root()));
        assert_ne!(a.root, b.root);
    }

    #[test]
    fn add_is_idempotent_in_name() {
        let mut set = TenantWorkspaces::new();
        let _ = set.add("dup");
        let _ = set.add("dup");
        assert_eq!(set.tenants().len(), 1);
    }

    #[test]
    fn insert_molecule_writes_state_json() {
        let mut set = TenantWorkspaces::new();
        let a = set.add("a");
        let body = json!({"id": "task-1", "state": "pending"});
        let path = a.insert_molecule("task-1", &body).unwrap();
        assert!(path.ends_with("molecules/task-1/state.json"));
        let read: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read["id"], "task-1");
    }

    #[test]
    fn tempdir_cleanup_removes_tree_on_drop() {
        let path = {
            let ws = tenant_workspace("cleanup");
            ws.tenant_root().to_owned()
        };
        // After the workspace is dropped, the directory should not
        // exist — `TempDir` RAII.
        assert!(!path.exists());
    }
}
