// SPDX-License-Identifier: Apache-2.0

//! REPO-EXACT: which repository a session is working in, resolved rather than
//! guessed.
//!
//! The mission invariant reads: *"`stagecraft` denotes a resolved galaxy/root
//! identity, not a substring search in paths. Worktrees are distinguished from
//! the canonical root."* Two concrete failures motivate it:
//!
//! - `sanitize_path` maps every non-alphanumeric byte to `-`, so
//!   `…/cosmon/.worktrees/task-X` and `…/cosmon--worktrees/task-X` produce the
//!   same Claude project directory name (probe P6 of ADR-168). A repo identity
//!   read back from that directory name is therefore a guess, and this module
//!   never reads one: the `cwd` comes from *inside* the log.
//! - A worktree checked out at `…/cosmon-scratch` contains the string `cosmon`.
//!   Any `path.contains(galaxy)` test selects it. So this module offers no
//!   substring predicate at all — [`RepoIdentity`] compares by equality of a
//!   canonicalised root, and that is the only comparison there is.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Whether a resolved root is a repository's canonical checkout or one of its
/// linked worktrees.
///
/// The distinction is load-bearing: a fleet routinely runs one worker per
/// worktree of the same galaxy, and collapsing them would make two different
/// sessions look like one repo identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RepoKind {
    /// `.git` is a directory — the canonical checkout.
    Canonical,
    /// `.git` is a file pointing into `…/.git/worktrees/<name>` — a linked
    /// worktree.
    Worktree,
}

/// The repository a session is working in.
///
/// Equality is equality of [`root`](Self::root) and [`kind`](Self::kind) —
/// nothing is compared by name, prefix or substring.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepoIdentity {
    /// The canonicalised directory that holds the `.git` entry.
    root: PathBuf,
    /// Canonical checkout or linked worktree.
    kind: RepoKind,
    /// For a worktree, the canonical checkout it is linked to, when the
    /// `gitdir:` pointer names one. `None` for a canonical checkout, and for a
    /// worktree whose pointer cannot be read.
    linked_root: Option<PathBuf>,
}

impl RepoIdentity {
    /// Resolve the repository identity of a working directory.
    ///
    /// Canonicalises `cwd` (following symlinks, so `/tmp` and `/private/tmp`
    /// on macOS resolve to one identity) and walks up to the first ancestor
    /// carrying a `.git` entry.
    ///
    /// Returns `None` when the path does not exist or no ancestor is a
    /// repository — a session outside a repo has no repo identity, and
    /// FAIL-CLOSED-AUTHORITY says an unknown must stay unknown rather than
    /// become a plausible default.
    #[must_use]
    pub fn resolve(cwd: impl AsRef<Path>) -> Option<Self> {
        let start = std::fs::canonicalize(cwd.as_ref()).ok()?;
        for ancestor in start.ancestors() {
            let dot_git = ancestor.join(".git");
            let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
                continue;
            };
            if meta.is_dir() {
                return Some(Self {
                    root: ancestor.to_path_buf(),
                    kind: RepoKind::Canonical,
                    linked_root: None,
                });
            }
            if meta.is_file() {
                return Some(Self {
                    root: ancestor.to_path_buf(),
                    kind: RepoKind::Worktree,
                    linked_root: linked_root_from_pointer(&dot_git),
                });
            }
        }
        None
    }

    /// The canonicalised root directory of the repository.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical checkout or linked worktree.
    #[must_use]
    pub fn kind(&self) -> RepoKind {
        self.kind
    }

    /// The canonical checkout a worktree belongs to, when it could be read.
    ///
    /// This is how a cockpit says *"these two workers are in two worktrees of
    /// the same galaxy"* without ever comparing path text: two worktrees agree
    /// here and still differ in [`root`](Self::root).
    #[must_use]
    pub fn linked_root(&self) -> Option<&Path> {
        self.linked_root.as_deref()
    }

    /// Whether two identities denote the same checkout.
    ///
    /// Exists to be the *only* comparison callers reach for. It is exact:
    /// a worktree never equals the canonical checkout it links to.
    #[must_use]
    pub fn is_same(&self, other: &Self) -> bool {
        self == other
    }
}

/// Read `…/.worktrees/x/.git` (a one-line `gitdir: <path>/.git/worktrees/<name>`
/// pointer) and return the canonical checkout it points into.
fn linked_root_from_pointer(dot_git_file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git_file).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    // `<canonical>/.git/worktrees/<name>` → strip from the `.git` component on.
    let mut cursor = target;
    while let Some(parent) = cursor.parent() {
        if cursor.file_name().is_some_and(|n| n == ".git") {
            return Some(parent.to_path_buf());
        }
        cursor = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canonical_checkout_and_its_homonym_neighbour_are_different_identities() {
        let tmp = tempfile::tempdir().unwrap();
        // Two sibling repos whose names share a prefix: a substring test
        // ("does the path contain `cosmon`?") accepts both.
        let cosmon = tmp.path().join("cosmon");
        let homonym = tmp.path().join("cosmon-scratch");
        for dir in [&cosmon, &homonym] {
            std::fs::create_dir_all(dir.join(".git")).unwrap();
        }

        let a = RepoIdentity::resolve(&cosmon).unwrap();
        let b = RepoIdentity::resolve(&homonym).unwrap();
        assert!(!a.is_same(&b));
        assert_eq!(a.kind(), RepoKind::Canonical);
    }

    #[test]
    fn a_worktree_is_not_its_canonical_checkout_but_knows_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("galaxy");
        std::fs::create_dir_all(root.join(".git").join("worktrees").join("task-a")).unwrap();
        let wt = root.join(".worktrees").join("task-a");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!(
                "gitdir: {}\n",
                root.join(".git").join("worktrees").join("task-a").display()
            ),
        )
        .unwrap();

        let canonical = RepoIdentity::resolve(&root).unwrap();
        let worktree = RepoIdentity::resolve(&wt).unwrap();

        assert!(!worktree.is_same(&canonical), "worktree ≠ canonical root");
        assert_eq!(worktree.kind(), RepoKind::Worktree);
        assert_eq!(
            worktree
                .linked_root()
                .map(|p| std::fs::canonicalize(p).unwrap()),
            Some(std::fs::canonicalize(&root).unwrap()),
            "the worktree still names the checkout it belongs to"
        );
    }

    #[test]
    fn a_path_outside_any_repository_has_no_identity() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(RepoIdentity::resolve(tmp.path().join("nowhere")).is_none());
    }
}
