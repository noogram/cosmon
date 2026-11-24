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
    /// The molecule's merge is reserved (`no-auto-harvest`, `harvest_to:*`):
    /// it lands by an explicit gesture, typically `cs land-train`, and no
    /// automatic path will merge it.
    pub held_for_landing: bool,
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
            held_for_landing: cosmon_core::harvest_hold::harvest_reserved(
                molecule.tags.iter().map(cosmon_core::tag::Tag::as_str),
            ),
        }
    }

    /// Mark the work as already merged: the way to finish the molecule is
    /// plain `cs done <id>`. It finds the branch already on the base, merges
    /// nothing a second time, and archives and tears down the worktree, session
    /// and worker. `--no-merge` would do the same teardown but records a
    /// `MergeSkipped` that is false for work that did land.
    #[must_use]
    pub fn already_merged(mut self) -> Self {
        self.harvest_command = format!("cs done {}", self.molecule);
        self.landed = true;
        self
    }

    /// Render the location line shared by human-readable lifecycle surfaces.
    #[must_use]
    pub fn render(&self) -> String {
        let outcome = if self.landed {
            "already merged; run `{}` to archive and tear it down"
        } else if self.held_for_landing {
            "held for external landing (no-auto-harvest): it lands by `cs land-train`; \
             run `{}` only to land it yourself"
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #213: the hint for landed work is plain `cs done`, never
    /// `--no-merge`, which would record a `MergeSkipped` that is false.
    #[test]
    fn landed_work_points_at_plain_cs_done() {
        let location = WorkLocation {
            molecule: "task-20260101-aaaa".to_owned(),
            branch: "feat/task-20260101-aaaa".to_owned(),
            worktree: "/repo/.worktrees/task-20260101-aaaa".to_owned(),
            harvest_command: "cs done task-20260101-aaaa".to_owned(),
            landed: false,
            held_for_landing: false,
        }
        .already_merged();
        assert!(location.landed);
        assert_eq!(location.harvest_command, "cs done task-20260101-aaaa");
        assert!(!location.render().contains("--no-merge"));
    }

    fn location(landed: bool, held_for_landing: bool) -> WorkLocation {
        WorkLocation {
            molecule: "task-20260101-aaaa".to_owned(),
            branch: "feat/task-20260101-aaaa".to_owned(),
            worktree: "/repo/.worktrees/task-20260101-aaaa".to_owned(),
            harvest_command: "cs done task-20260101-aaaa".to_owned(),
            landed,
            held_for_landing,
        }
    }

    /// A reserved molecule is told it lands by train; `cs done` is for
    /// landing it oneself, not "to merge it".
    #[test]
    fn held_work_says_it_lands_by_train() {
        let line = location(false, true).render();
        assert!(line.contains("`cs land-train`"), "{line}");
        assert!(!line.contains("to merge it"), "{line}");
    }

    #[test]
    fn plain_and_landed_work_keep_their_hints() {
        let plain = location(false, false).render();
        assert!(
            plain.contains("run `cs done task-20260101-aaaa` to merge it"),
            "{plain}"
        );
        let landed = location(true, true).render();
        assert!(landed.contains("already merged"), "{landed}");
    }
}
