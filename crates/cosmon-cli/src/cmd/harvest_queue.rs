// SPDX-License-Identifier: AGPL-3.0-only

//! Which `Completed` molecules still owe a harvest (issue #174).
//!
//! `archived` is written by `cs done` only, so a branch merged by hand, or by
//! a `cs done` that predates the field, leaves a `Completed` molecule looking
//! un-harvested after its work is on the trunk. Dropping it from the queue is
//! only right when nothing is left to tear down: a worktree, a live tmux
//! session or a worker roster entry would be hidden along with it. A molecule
//! therefore *settles* — leaves the queue — only when its work has landed
//! **and** it holds no residue.
//!
//! `cs status` and `cs peek --phase harvestable` both read [`assess`] and
//! [`effectively_archived`], so the two surfaces answer alike. The git and tmux
//! calls are made once per invocation, and any failure keeps the molecule
//! listed, the safe direction for a queue whose job is to be noisy.

use std::collections::HashSet;
use std::path::Path;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::transport::TransportBackend;
use cosmon_state::{Fleet, MoleculeData};

use super::work_location::WorkLocation;

/// What [`assess`] found about the `Completed`, un-archived molecules.
#[derive(Debug, Default)]
pub(crate) struct HarvestAssessment {
    /// Molecules whose work is on the trunk, settled or not.
    landed: HashSet<String>,
    /// Landed molecules with nothing left to tear down: not in the queue.
    settled: HashSet<String>,
}

impl HarvestAssessment {
    /// This molecule has nothing left to harvest.
    #[must_use]
    pub(crate) fn is_settled(&self, id: &str) -> bool {
        self.settled.contains(id)
    }

    /// This molecule's work is already on the trunk.
    #[must_use]
    pub(crate) fn is_landed(&self, id: &str) -> bool {
        self.landed.contains(id)
    }

    /// Where a queued molecule's work sits and the gesture that finishes it:
    /// `cs done <id>` to merge, `cs done <id> --no-merge` once it has landed.
    #[must_use]
    pub(crate) fn location(
        &self,
        molecule: &MoleculeData,
        fleet: &Fleet,
        repo_root: &Path,
    ) -> WorkLocation {
        let location = WorkLocation::from_state(molecule, fleet, repo_root);
        if self.is_landed(molecule.id.as_str()) {
            location.already_merged()
        } else {
            location
        }
    }
}

/// The archive flag as the harvest queue reads it: a settled molecule counts
/// as finalized. Feed the result to `PhaseFilter::is_harvestable` or
/// `PhaseFilter::matches_molecule` in place of `MoleculeData::archived`.
#[must_use]
pub(crate) fn effectively_archived(archived: bool, settled: bool) -> bool {
    archived || settled
}

/// Classify every `Completed`, un-archived molecule. Calls git and tmux only
/// when there is at least one such molecule.
#[must_use]
pub(crate) fn assess(
    molecules: &[MoleculeData],
    fleet: &Fleet,
    repo_root: &Path,
    state_dir: &Path,
    tmux_socket: &str,
) -> HarvestAssessment {
    let candidates: Vec<&MoleculeData> = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Completed && !m.archived)
        .collect();
    if candidates.is_empty() {
        return HarvestAssessment::default();
    }
    let merged_and_gone = merged_and_deleted_molecules(repo_root);
    let live = live_sessions(state_dir, tmux_socket);
    let mut out = HarvestAssessment::default();
    for m in candidates {
        let id = m.id.as_str();
        if !(m.merged_at.is_some() || merged_and_gone.contains(id)) {
            continue;
        }
        out.landed.insert(id.to_owned());
        if !holds_residue(m, fleet, repo_root, live.as_ref()) {
            out.settled.insert(id.to_owned());
        }
    }
    out
}

/// Something is left to tear down: the worktree is on disk, the tmux session
/// is alive, or the worker is still on the roster. Unknown session state
/// (`live == None`) counts as residue.
fn holds_residue(
    molecule: &MoleculeData,
    fleet: &Fleet,
    repo_root: &Path,
    live: Option<&HashSet<String>>,
) -> bool {
    let worktree = WorkLocation::from_state(molecule, fleet, repo_root).worktree;
    Path::new(&worktree).exists()
        || live.is_none_or(|sessions| sessions.contains(&molecule.teardown_session()))
        || molecule
            .worker()
            .is_some_and(|w| fleet.workers.contains_key(w))
}

/// Names of the live tmux sessions on every fleet socket, or `None` when any
/// socket could not be listed.
fn live_sessions(state_dir: &Path, tmux_socket: &str) -> Option<HashSet<String>> {
    let mut names = HashSet::new();
    for backend in crate::energy_probe::discover_fleet_backends(state_dir, tmux_socket) {
        for info in backend.list_sessions().ok()? {
            names.insert(info.worker_id.as_str().to_owned());
            names.insert(info.session_name);
        }
    }
    Some(names)
}

/// Molecule ids whose `feat/<id>` branch was merged into `main` and no
/// longer exists.
///
/// A missing branch alone proves nothing (it may never have been created, or
/// been dropped unmerged), so the merge commit on `main` is required too. The
/// two git calls are made once per invocation, not once per molecule. Any git
/// failure yields an empty set: the molecule then stays listed, which is the
/// safe direction for a harvest queue.
fn merged_and_deleted_molecules(repo_root: &Path) -> HashSet<String> {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let Some(subjects) = git(&["log", "main", "--merges", "--format=%s"]) else {
        return HashSet::new();
    };
    let live =
        git(&["branch", "--list", "feat/*", "--format=%(refname:short)"]).unwrap_or_default();
    let live: HashSet<&str> = live.lines().map(str::trim).collect();
    merged_molecule_ids(&subjects)
        .into_iter()
        .filter(|id| !live.contains(format!("feat/{id}").as_str()))
        .collect()
}

/// Molecule ids named as `feat/<id>` in merge-commit subjects, one subject per
/// line (`Merge branch 'feat/<id>'`, `Merge pull request … from …/feat/<id>`).
fn merged_molecule_ids(subjects: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    for subject in subjects.lines() {
        for (at, _) in subject.match_indices("feat/") {
            let rest = &subject[at + "feat/".len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(rest.len());
            if MoleculeId::new(&rest[..end]).is_ok() {
                ids.insert(rest[..end].to_owned());
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #174: merge subjects name the molecule branch in several shapes;
    /// non-molecule refs and unrelated text yield nothing.
    #[test]
    fn merged_molecule_ids_reads_every_merge_subject_shape() {
        let subjects = "Merge branch 'feat/task-20260101-aaaa'\n\
                        Merge pull request #9 from org/feat/task-20260101-bbbb\n\
                        Merge branch 'feat/issue-171' into main\n\
                        Merge branch 'backup/task-20260101-cccc'\n";
        let ids = merged_molecule_ids(subjects);
        assert!(ids.contains("task-20260101-aaaa"));
        assert!(ids.contains("task-20260101-bbbb"));
        assert!(!ids.contains("task-20260101-cccc"));
    }

    /// A settled molecule is read as finalized; an unsettled one keeps its own flag.
    #[test]
    fn settled_counts_as_archived_and_nothing_else_does() {
        assert!(effectively_archived(true, false));
        assert!(effectively_archived(false, true));
        assert!(!effectively_archived(false, false));
    }
}
