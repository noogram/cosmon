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

/// Resolve the branch that holds a molecule's work from persisted state.
///
/// `originating_branch` records non-conventional and integration branches.
/// Molecules written before that field existed retain the historical
/// `feat/<id>` convention.
#[must_use]
pub fn molecule_branch(molecule: &MoleculeData) -> String {
    molecule
        .originating_branch
        .clone()
        .filter(|branch| !branch.trim().is_empty())
        .unwrap_or_else(|| format!("feat/{}", molecule.id))
}

/// Concrete branch, checkout, and command needed to harvest one molecule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkLocation {
    /// Molecule whose work is being located.
    pub molecule: String,
    /// Branch holding the work.
    pub branch: String,
    /// Worker checkout recorded at dispatch, or the conventional legacy path.
    pub worktree: String,
    /// Concrete command that finishes this molecule: merges it and tears its
    /// worker down, or — once [`Self::landed`] — only archives and tears down.
    pub harvest_command: String,
    /// The work is already on the trunk, so [`Self::harvest_command`] must
    /// not merge it again.
    pub landed: bool,
}

impl WorkLocation {
    /// Project one molecule through existing molecule and fleet records.
    ///
    /// `repo_root` is supplied by the filesystem adapter. Relative worker
    /// paths are resolved beneath it; legacy absolute paths remain absolute.
    #[must_use]
    pub fn from_state(molecule: &MoleculeData, fleet: &Fleet, repo_root: &Path) -> Self {
        let branch = molecule_branch(molecule);
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
            landed: false,
        }
    }

    /// Mark the work as already merged: the way to finish the molecule is
    /// `cs done <id> --no-merge`, which archives it and tears down its
    /// worktree, session and worker without merging a second time.
    #[must_use]
    pub fn already_merged(mut self) -> Self {
        self.harvest_command = format!("cs done {} --no-merge", self.molecule);
        self.landed = true;
        self
    }

    /// Render the location line shared by human-readable lifecycle surfaces.
    #[must_use]
    pub fn render(&self) -> String {
        let outcome = if self.landed {
            "already merged; run `{}` to archive and tear it down"
        } else {
            "run `{}` to merge it"
        };
        format!(
            "branch `{}` · worktree `{}` · {}",
            self.branch,
            self.worktree,
            outcome.replace("{}", &self.harvest_command)
        )
    }
}
