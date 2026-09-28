// SPDX-License-Identifier: AGPL-3.0-only

//! Shared projection of the work a molecule still holds before `cs done`.
//!
//! `cs tackle --workdir` makes the conventional `.worktrees/<id>` path only
//! a fallback. Completion, stopping, and status surfaces must all read the
//! worker checkout recorded in fleet state so they cannot point an operator
//! at a plausible but unrelated directory.

use std::path::{Path, PathBuf};

use cosmon_state::{Fleet, MoleculeData};

use super::Context;

/// The concrete branch, checkout, and command needed to harvest one molecule.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WorkLocation {
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
    /// Project one molecule through the existing molecule and fleet records.
    #[must_use]
    pub(crate) fn from_state(molecule: &MoleculeData, fleet: &Fleet, repo_root: &Path) -> Self {
        let branch = molecule
            .originating_branch
            .clone()
            .filter(|branch| !branch.trim().is_empty())
            .unwrap_or_else(|| format!("feat/{}", molecule.id));
        let recorded = molecule.worker().and_then(|worker_id| {
            fleet
                .workers
                .get(worker_id)?
                .repo
                .as_deref()
                .map(|repo| cosmon_filestore::resolve_repo_path(repo, repo_root))
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

    /// Render the location line shared by completion and status text.
    #[must_use]
    pub(crate) fn render(&self) -> String {
        format!(
            "branch `{}` · worktree `{}` · run `{}` to merge it",
            self.branch, self.worktree, self.harvest_command
        )
    }
}

/// Resolve the repository root against which fleet-relative checkout paths
/// were recorded. A flat test store falls back to the invocation directory.
#[must_use]
pub(crate) fn repo_root(ctx: &Context) -> PathBuf {
    let config = super::resolve_config_from_context(ctx);
    let root = cosmon_cli::target_repo::resolve_from_config(&config)
        .map(|resolved| resolved.root)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_else(|_| PathBuf::from("."));
    std::fs::canonicalize(&root).unwrap_or(root)
}
