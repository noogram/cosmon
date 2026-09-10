// SPDX-License-Identifier: AGPL-3.0-only

//! The worktree a molecule was tackled in, and the containment guard that
//! keeps a harvest's artifact commit out of a tree the molecule never
//! claimed.
//!
//! The invariant these encode is *"only commit into the tree this molecule
//! was tackled in"*. `cs evolve` needs the same answer for its per-step
//! auto-commit and keeps its own equality-shaped guard on top of
//! [`canonical_or`]; the containment-shaped one is the harvest's, so it lives
//! with the harvest.

use std::path::{Path, PathBuf};

use cosmon_filestore::FileStore;
use cosmon_state::StateStore as _;

/// Canonicalize `p`, degrading to a lexical copy when the path does not exist
/// on disk (`std::fs::canonicalize` requires the path to exist).
///
/// The worktree guard must be **total** — it can never error — because the
/// auto-commit it protects is a defensive convenience that must not block the
/// molecule lifecycle. Canonicalizing also resolves symlinks so a worktree
/// reached through a symlinked path still matches its recorded target.
#[must_use]
pub fn canonical_or(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Resolve the worktree `cs tackle` recorded for `mol`, as an absolute path.
///
/// `cs tackle` stamps the worker's worktree into the bound worker's `repo`
/// field (stored relative to the galaxy root for portability — see
/// [`cosmon_filestore::make_relative`]). We read it back from the fleet and
/// resolve it against `galaxy_root`.
///
/// Returns `None` for molecules with no bound worker or no recorded repo —
/// the legacy / test shapes that must keep behaving as before (no
/// worktree-mismatch regression). This is the *recorded-path* source the
/// guard compares against, which is why the guard is Cell-B-safe: it never
/// hard-codes a `~/galaxies` prefix.
#[must_use]
pub fn recorded_worktree_for(
    store: &FileStore,
    mol: &cosmon_state::MoleculeData,
    galaxy_root: &Path,
) -> Option<PathBuf> {
    let wid = mol.assigned_worker.as_ref()?;
    let fleet = store.load_fleet().ok()?;
    let repo = fleet.workers.get(wid)?.repo.as_deref()?;
    Some(cosmon_filestore::resolve_repo_path(repo, galaxy_root))
}

/// Decide whether `cs done`'s artifact commit may run in `commit_root`.
///
/// Unlike `cs evolve`'s equality-shaped guard, `cs done` commits the
/// molecule's durable artifacts from the **galaxy root** (the worktree is
/// torn down first), so the safety question is *containment*, not equality:
/// the recorded worktree must live inside the galaxy we are about to commit
/// into. When it does not, `cs done` is running in a foreign repo (the
/// genericize ghost-commit — a release clone outside the galaxy) and must
/// SKIP + warn.
///
/// Returns `Some((recorded, root))` on mismatch, `None` when safe (contained,
/// or no recorded worktree).
#[must_use]
pub fn done_worktree_mismatch(
    recorded_worktree: Option<&Path>,
    commit_root: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let recorded = recorded_worktree?;
    let rec = canonical_or(recorded);
    let root = canonical_or(commit_root);
    (!rec.starts_with(&root)).then_some((rec, root))
}
