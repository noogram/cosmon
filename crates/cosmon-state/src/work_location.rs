// SPDX-License-Identifier: AGPL-3.0-only

//! I/O-free projection of the work a molecule still holds before harvest.
//!
//! `cs tackle --workdir` makes the conventional `.worktrees/<id>` path only
//! a fallback. Every surface that tells an operator how to finish or stop work
//! must therefore project the recorded worker checkout, not reconstruct a
//! plausible but potentially unrelated path.

use std::path::Path;

use serde::Serialize;

use crate::{Fleet, MoleculeData};

/// Concrete branch, checkout, and command needed to harvest one molecule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkLocation {
    /// Molecule whose work is being located.
    pub molecule: String,
    /// Branch holding the work.
    pub branch: String,
    /// Worker checkout recorded at dispatch, or the conventional legacy path.
    pub worktree: String,
    /// Concrete command that merges this molecule and tears its worker down.
    pub harvest_command: String,
}

impl WorkLocation {
    /// Project one molecule through existing molecule and fleet records.
    ///
    /// `repo_root` is supplied by the filesystem adapter. Relative worker
    /// paths are resolved beneath it; legacy absolute paths remain absolute.
    #[must_use]
    pub fn from_state(molecule: &MoleculeData, fleet: &Fleet, repo_root: &Path) -> Self {
        let branch = molecule
            .originating_branch
            .clone()
            .filter(|branch| !branch.trim().is_empty())
            .unwrap_or_else(|| format!("feat/{}", molecule.id));
        let recorded = molecule.worker().and_then(|worker_id| {
            let repo = fleet.workers.get(worker_id)?.repo.as_deref()?;
            let path = Path::new(repo);
            Some(if path.is_absolute() {
                path.to_path_buf()
            } else {
                repo_root.join(path)
            })
        });
        let worktree =
            recorded.unwrap_or_else(|| repo_root.join(".worktrees").join(molecule.id.as_str()));

        Self {
            molecule: molecule.id.to_string(),
            branch,
            worktree: worktree.display().to_string(),
            harvest_command: format!("cs done {}", molecule.id),
        }
    }

    /// Render the location line shared by human-readable lifecycle surfaces.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "branch `{}` · worktree `{}` · run `{}` to merge it",
            self.branch, self.worktree, self.harvest_command
        )
    }
}
