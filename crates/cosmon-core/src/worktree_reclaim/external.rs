// SPDX-License-Identifier: AGPL-3.0-only

//! External derived roots — the I/O-free half of ADR-191.
//!
//! A project may declare that a derived root such as `target` lives outside
//! the worktree, as a per-molecule child of a declared **base**:
//! `<base>/<molecule-id><suffix>`. Every decision that makes such a
//! directory reclaimable is a pure function here, with what the filesystem
//! said passed in:
//!
//! * [`validate_declaration`], [`validate_base_location`] and
//!   [`validate_entry_set`] refuse a declaration that could name a shared or
//!   durable directory (D1);
//! * [`classify_base_child`] reads one child name of a base against every
//!   entry of that base and yields one molecule, none, or an ambiguity (D2);
//! * [`classify_link`] decides whether a link's canonical target is the
//!   candidate's own directory under one entry (D2, D7);
//! * [`colliding`] finds resolved directories reached more than once, by
//!   path or by device and inode (D2);
//! * [`external_selection`] is the gate for an external directory, linked
//!   or orphan: a record **present in this fleet's store**, a terminal
//!   status, every lock acquired, uniqueness and a complete enumeration
//!   (D4, D5, D6).
//!
//! The seven-axis predicates of the parent module are not changed: an
//! external directory is a new candidate class with its own gate, not a new
//! row of the contract's table.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{LockObservation, MoleculeRecordObservation, StatusObservation};
use crate::id::MoleculeId;
use crate::molecule::MoleculeStatus;

/// The longest suffix a declaration may carry, in bytes.
pub const MAX_SUFFIX_BYTES: usize = 64;

// ---------------------------------------------------------------------------
// D1 — declaration validation
// ---------------------------------------------------------------------------

/// Why one `[[worktree_reclaim.external]]` entry was refused.
///
/// Each variant is one condition the ADR names, so the configuration error
/// an operator reads says which one failed rather than "invalid".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationRejection {
    /// The entry's `root` is not one of `[worktree_reclaim].evict`.
    RootNotEvicted(String),
    /// The entry's `root` is not a rebuildable, worktree-local path.
    RootInvalid(String, super::DerivedRootRejection),
    /// The base is not an absolute path.
    BaseNotAbsolute,
    /// The suffix contains `/`, so `<m><suffix>` would not be one component.
    SuffixHasSlash,
    /// The suffix contains `..`.
    SuffixHasParent,
    /// The suffix contains NUL.
    SuffixHasNul,
    /// The suffix is longer than [`MAX_SUFFIX_BYTES`].
    SuffixTooLong,
    /// The base is `/`.
    BaseIsFilesystemRoot,
    /// The base is the user's home directory.
    BaseIsHome,
    /// The base is the repository root.
    BaseIsRepositoryRoot,
    /// The base is an ancestor of the repository root.
    BaseIsAncestorOfRepository,
    /// The base lies inside the repository.
    BaseInsideRepository,
    /// The base lies inside (or is) a worktree.
    BaseInsideWorktree(PathBuf),
    /// Same canonical base and same suffix as an earlier entry.
    Duplicate {
        /// Index of the earlier entry.
        of: usize,
    },
    /// The base lies strictly inside another declared base.
    Nested {
        /// Index of the entry whose base contains this one.
        inside: usize,
    },
}

impl DeclarationRejection {
    /// The condition, in the words the configuration error prints.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::RootNotEvicted(r) => {
                format!("root `{r}` is not in [worktree_reclaim].evict")
            }
            Self::RootInvalid(r, why) => format!("root `{r}` is not a derived root: {why:?}"),
            Self::BaseNotAbsolute => "the base must be an absolute path".to_owned(),
            Self::SuffixHasSlash => "the suffix contains `/`".to_owned(),
            Self::SuffixHasParent => "the suffix contains `..`".to_owned(),
            Self::SuffixHasNul => "the suffix contains NUL".to_owned(),
            Self::SuffixTooLong => {
                format!("the suffix is longer than {MAX_SUFFIX_BYTES} bytes")
            }
            Self::BaseIsFilesystemRoot => "the base is `/`".to_owned(),
            Self::BaseIsHome => "the base is the home directory".to_owned(),
            Self::BaseIsRepositoryRoot => "the base is the repository root".to_owned(),
            Self::BaseIsAncestorOfRepository => {
                "the base is an ancestor of the repository root".to_owned()
            }
            Self::BaseInsideRepository => "the base lies inside the repository".to_owned(),
            Self::BaseInsideWorktree(w) => {
                format!("the base lies inside the worktree {}", w.display())
            }
            Self::Duplicate { of } => {
                format!("duplicate of entry {of}: the same canonical base with the same suffix")
            }
            Self::Nested { inside } => {
                format!("the base is nested inside the base of entry {inside}")
            }
        }
    }
}

/// Lexical checks on one entry: its root, its base's form, its suffix.
///
/// Decidable without the filesystem, so they run on every entry, reachable
/// or not.
///
/// # Errors
/// The first condition the entry fails.
pub fn validate_declaration(
    root: &str,
    base: &Path,
    suffix: &str,
    evict: &[String],
) -> Result<(), DeclarationRejection> {
    if !evict.iter().any(|e| e == root) {
        return Err(DeclarationRejection::RootNotEvicted(root.to_owned()));
    }
    super::validate_derived_root(Path::new(root))
        .map_err(|why| DeclarationRejection::RootInvalid(root.to_owned(), why))?;
    if !base.is_absolute() {
        return Err(DeclarationRejection::BaseNotAbsolute);
    }
    validate_suffix(suffix)
}

/// A suffix is a plain fragment of one path component: `<m><suffix>` is
/// always a single direct child of the base.
///
/// # Errors
/// The first condition the suffix fails.
pub fn validate_suffix(suffix: &str) -> Result<(), DeclarationRejection> {
    if suffix.contains('/') {
        return Err(DeclarationRejection::SuffixHasSlash);
    }
    if suffix.contains("..") {
        return Err(DeclarationRejection::SuffixHasParent);
    }
    if suffix.contains('\0') {
        return Err(DeclarationRejection::SuffixHasNul);
    }
    if suffix.len() > MAX_SUFFIX_BYTES {
        return Err(DeclarationRejection::SuffixTooLong);
    }
    Ok(())
}

/// The places a base may never be, all in canonical form.
///
/// Passed in rather than discovered so the rule is testable without a home
/// directory or a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtectedLocations {
    /// The user's home directory, when one is known.
    pub home: Option<PathBuf>,
    /// The repository root.
    pub repo_root: PathBuf,
    /// Every enumerated worktree.
    pub worktrees: Vec<PathBuf>,
}

/// Refuse a base at a location whose children are not per-molecule build
/// directories: `/`, home, the repository or an ancestor of it, anything
/// inside the repository or a worktree.
///
/// # Errors
/// The first location the base matches.
pub fn validate_base_location(
    base: &Path,
    protected: &ProtectedLocations,
) -> Result<(), DeclarationRejection> {
    if base.parent().is_none() {
        return Err(DeclarationRejection::BaseIsFilesystemRoot);
    }
    if protected.home.as_deref() == Some(base) {
        return Err(DeclarationRejection::BaseIsHome);
    }
    if let Some(w) = protected.worktrees.iter().find(|w| base.starts_with(w)) {
        return Err(DeclarationRejection::BaseInsideWorktree(w.clone()));
    }
    if base == protected.repo_root {
        return Err(DeclarationRejection::BaseIsRepositoryRoot);
    }
    if protected.repo_root.starts_with(base) {
        return Err(DeclarationRejection::BaseIsAncestorOfRepository);
    }
    if base.starts_with(&protected.repo_root) {
        return Err(DeclarationRejection::BaseInsideRepository);
    }
    Ok(())
}

/// Refuse duplicates and nesting across the reachable entries.
///
/// `entries` is `(index, canonical base, suffix)`. Two entries on the same
/// canonical base with different suffixes are allowed; their children are
/// classified together by [`classify_base_child`].
///
/// # Errors
/// `(index, rejection)` for the first offending entry.
pub fn validate_entry_set(
    entries: &[(usize, &Path, &str)],
) -> Result<(), (usize, DeclarationRejection)> {
    for (pos, (i, base, suffix)) in entries.iter().enumerate() {
        for (j, other, other_suffix) in &entries[..pos] {
            if base == other && suffix == other_suffix {
                return Err((*i, DeclarationRejection::Duplicate { of: *j }));
            }
        }
        for (j, other, _) in entries {
            if base != other && base.starts_with(other) {
                return Err((*i, DeclarationRejection::Nested { inside: *j }));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// D2 — one child name, one reading
// ---------------------------------------------------------------------------

/// How a direct child of a base reads against every entry of that base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildReading {
    /// Exactly one entry yields a molecule id.
    Molecule {
        /// The entry index.
        entry: usize,
        /// The molecule the name belongs to under that entry.
        id: MoleculeId,
    },
    /// No entry yields a molecule id.
    NotAMolecule,
    /// More than one entry yields a molecule id.
    Ambiguous(Vec<usize>),
}

/// Read `name` against `(entry index, suffix)` for every entry of one
/// canonical base.
///
/// For each entry whose suffix ends `name` (an empty suffix always does),
/// the reading is `name` with that suffix removed; it counts when it parses
/// as a [`MoleculeId`]. Validity is syntactic: whether the molecule is in
/// this store is the gate's question, asked later.
#[must_use]
pub fn classify_base_child(name: &str, entries: &[(usize, &str)]) -> ChildReading {
    let mut readings = Vec::new();
    for (entry, suffix) in entries {
        if let Some(stem) = name.strip_suffix(suffix) {
            if let Ok(id) = MoleculeId::new(stem) {
                readings.push((*entry, id));
            }
        }
    }
    match readings.len() {
        0 => ChildReading::NotAMolecule,
        1 => {
            let (entry, id) = readings.remove(0);
            ChildReading::Molecule { entry, id }
        }
        _ => ChildReading::Ambiguous(readings.into_iter().map(|(e, _)| e).collect()),
    }
}

// ---------------------------------------------------------------------------
// D2, D7 — the link verdict
// ---------------------------------------------------------------------------

/// One declared entry whose base exists and canonicalized at pass start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachableEntry {
    /// Position of the entry in the configuration.
    pub index: usize,
    /// The derived root it is declared for.
    pub root: PathBuf,
    /// The canonical base.
    pub base: PathBuf,
    /// The fixed suffix after the molecule id.
    pub suffix: String,
}

/// The suffixes declared on one canonical base, as [`classify_base_child`]
/// takes them.
#[must_use]
pub fn suffixes_of<'a>(base: &Path, entries: &'a [ReachableEntry]) -> Vec<(usize, &'a str)> {
    entries
        .iter()
        .filter(|e| e.base == base)
        .map(|e| (e.index, e.suffix.as_str()))
        .collect()
}

/// What a link `w/<root>` resolving to a canonical target is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkVerdict {
    /// The candidate's own directory under one reachable entry.
    Accepted {
        /// The entry it was accepted under.
        entry: usize,
        /// The canonical base.
        base: PathBuf,
    },
    /// Under no declared base of this root.
    OutsideBases,
    /// The base itself — a shared, persistent target.
    IsBase(PathBuf),
    /// A direct child of a base that is not this molecule's under any entry
    /// declared for this root.
    NotOwnChild {
        /// The canonical base.
        base: PathBuf,
        /// How the child name reads.
        reading: ChildReading,
    },
    /// Below a child of a base.
    Deeper(PathBuf),
}

/// Decide what a link's canonical target is for the candidate owned by
/// `owner` (`None` when the candidate's name is not a molecule id).
///
/// Reachable bases are pairwise equal or disjoint ([`validate_entry_set`]),
/// so a target lies under at most one canonical base. The child is read
/// against **every** entry of that base, whatever its root, and accepted
/// only when the single reading is `owner` under an entry of `root`.
#[must_use]
pub fn classify_link(
    owner: Option<&MoleculeId>,
    root: &Path,
    target: &Path,
    entries: &[ReachableEntry],
) -> LinkVerdict {
    let Some(base) = entries
        .iter()
        .filter(|e| e.root == root)
        .map(|e| &e.base)
        .find(|b| target.starts_with(b))
    else {
        return LinkVerdict::OutsideBases;
    };
    if target == base {
        return LinkVerdict::IsBase(base.clone());
    }
    if target.parent() != Some(base.as_path()) {
        return LinkVerdict::Deeper(base.clone());
    }
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let reading = classify_base_child(name, &suffixes_of(base, entries));
    if let ChildReading::Molecule { entry, id } = &reading {
        let own_root = entries.iter().any(|e| e.index == *entry && e.root == root);
        if own_root && Some(id) == owner {
            return LinkVerdict::Accepted {
                entry: *entry,
                base: base.clone(),
            };
        }
    }
    LinkVerdict::NotOwnChild {
        base: base.clone(),
        reading,
    }
}

// ---------------------------------------------------------------------------
// D2 — uniqueness
// ---------------------------------------------------------------------------

/// A resolved directory's identity: its canonical path and, where the
/// platform has them, its device and inode numbers.
///
/// Both, because a bind mount makes one directory reachable under two
/// canonical paths, which no path comparison sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirIdentity {
    /// The canonical path.
    pub path: PathBuf,
    /// `(device, inode)`, when known.
    pub file_id: Option<(u64, u64)>,
}

impl DirIdentity {
    /// Whether two identities name the same directory.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        self.path == other.path || (self.file_id.is_some() && self.file_id == other.file_id)
    }
}

/// Indexes of identities that name the same directory as another one.
///
/// Every member of a collision is returned: when two candidates reach one
/// directory, neither is selected.
#[must_use]
pub fn colliding(identities: &[DirIdentity]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    for (i, a) in identities.iter().enumerate() {
        for (j, b) in identities.iter().enumerate().skip(i + 1) {
            if a.same_as(b) {
                out.insert(i);
                out.insert(j);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// D4, D5, D6 — selection
// ---------------------------------------------------------------------------

/// Combine the observations of every profile lock found under a directory.
///
/// `Held` wins, because contention is the more specific answer; then any
/// failed probe; `Acquired` only when every lock was acquired. An empty
/// list is a failed probe: no lock, no exclusion.
#[must_use]
pub fn combine_locks(locks: &[LockObservation], dir: &Path) -> LockObservation {
    if locks.is_empty() {
        return LockObservation::ProbeFailed(super::ObservationError::new(
            "find build locks",
            dir,
            "no `.cargo-lock` was found",
        ));
    }
    if locks.contains(&LockObservation::Held) {
        return LockObservation::Held;
    }
    for lock in locks {
        if let LockObservation::ProbeFailed(e) = lock {
            return LockObservation::ProbeFailed(e.clone());
        }
    }
    LockObservation::Acquired
}

/// Everything the gate of one external directory reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalObservation {
    /// The molecule record the directory's name or link attributes it to.
    pub molecule: MoleculeRecordObservation,
    /// Its status.
    pub status: StatusObservation,
    /// Every profile lock, combined by [`combine_locks`].
    pub lock: LockObservation,
    /// No other link or child reaches the same directory.
    pub unique: bool,
    /// The worktree enumeration had no error.
    pub enumeration_complete: bool,
    /// The base's listing was read without error.
    pub base_listed: bool,
}

/// Why an external directory was not selected, in decision order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalWithhold {
    /// The enumeration is incomplete, so uniqueness is unknown.
    EnumerationIncomplete,
    /// The base listing failed part-way.
    BaseListingFailed,
    /// Another link or child reaches the same directory.
    NotUnique,
    /// No record of the molecule in this fleet's store.
    RecordAbsent,
    /// The record could not be read.
    RecordUnknown(super::ObservationError),
    /// The status could not be read.
    StatusUnknown(super::ObservationError),
    /// The molecule is not `Completed` or `Collapsed`.
    NotTerminal(MoleculeStatus),
    /// A build lock is held by another process.
    LockHeld,
    /// A build lock could not be probed.
    LockProbeFailed(super::ObservationError),
}

/// Decide whether one external directory may have its payload reclaimed.
///
/// Unlike [`super::consideration_gate`], an absent record **withholds**: an
/// external base is outside this repository and may be shared with another
/// fleet, so only a molecule this store knows can claim a child of it. The
/// completeness and uniqueness conditions come first, so a self-inflicted
/// lock contention between two links to one directory reads as what it is.
///
/// # Errors
/// The first condition that withholds.
pub fn external_selection(obs: &ExternalObservation) -> Result<(), ExternalWithhold> {
    if !obs.enumeration_complete {
        return Err(ExternalWithhold::EnumerationIncomplete);
    }
    if !obs.base_listed {
        return Err(ExternalWithhold::BaseListingFailed);
    }
    if !obs.unique {
        return Err(ExternalWithhold::NotUnique);
    }
    match &obs.molecule {
        MoleculeRecordObservation::Absent => return Err(ExternalWithhold::RecordAbsent),
        MoleculeRecordObservation::Unknown(e) => {
            return Err(ExternalWithhold::RecordUnknown(e.clone()))
        }
        MoleculeRecordObservation::Present => {}
    }
    match &obs.status {
        StatusObservation::Unknown(e) => return Err(ExternalWithhold::StatusUnknown(e.clone())),
        StatusObservation::Known(MoleculeStatus::Completed | MoleculeStatus::Collapsed) => {}
        StatusObservation::Known(s) => return Err(ExternalWithhold::NotTerminal(*s)),
    }
    match &obs.lock {
        LockObservation::Acquired => Ok(()),
        LockObservation::Held => Err(ExternalWithhold::LockHeld),
        LockObservation::ProbeFailed(e) => Err(ExternalWithhold::LockProbeFailed(e.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree_reclaim::ObservationError;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn id(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn entry(index: usize, base: &str, suffix: &str) -> ReachableEntry {
        ReachableEntry {
            index,
            root: p("target"),
            base: p(base),
            suffix: suffix.to_owned(),
        }
    }

    fn evict() -> Vec<String> {
        vec!["target".to_owned()]
    }

    /// 28 — each suffix condition is refused by name; an empty suffix and
    /// `-wt` are accepted; exactly 64 bytes is accepted and 65 is not.
    #[test]
    fn suffix_conditions_are_refused_by_name() {
        assert_eq!(validate_suffix(""), Ok(()));
        assert_eq!(validate_suffix("-wt"), Ok(()));
        assert_eq!(validate_suffix(&"x".repeat(64)), Ok(()));
        assert_eq!(
            validate_suffix(&"x".repeat(65)),
            Err(DeclarationRejection::SuffixTooLong)
        );
        assert_eq!(
            validate_suffix("a/b"),
            Err(DeclarationRejection::SuffixHasSlash)
        );
        assert_eq!(
            validate_suffix("a..b"),
            Err(DeclarationRejection::SuffixHasParent)
        );
        assert_eq!(
            validate_suffix("a.b"),
            Ok(()),
            "a single dot is a plain character"
        );
        assert_eq!(
            validate_suffix("a\0b"),
            Err(DeclarationRejection::SuffixHasNul)
        );
    }

    /// Each rejection names its own condition: the configuration error an
    /// operator reads is this sentence.
    #[test]
    fn every_rejection_names_its_condition() {
        let cases = [
            (
                DeclarationRejection::RootNotEvicted("cache".into()),
                "not in [worktree_reclaim].evict",
            ),
            (
                DeclarationRejection::RootInvalid(
                    "..".into(),
                    super::super::DerivedRootRejection::Traversal,
                ),
                "not a derived root",
            ),
            (DeclarationRejection::BaseNotAbsolute, "absolute"),
            (DeclarationRejection::SuffixHasSlash, "contains `/`"),
            (DeclarationRejection::SuffixHasParent, "contains `..`"),
            (DeclarationRejection::SuffixHasNul, "NUL"),
            (DeclarationRejection::SuffixTooLong, "longer than 64 bytes"),
            (DeclarationRejection::BaseIsFilesystemRoot, "is `/`"),
            (DeclarationRejection::BaseIsHome, "home directory"),
            (
                DeclarationRejection::BaseIsRepositoryRoot,
                "repository root",
            ),
            (
                DeclarationRejection::BaseIsAncestorOfRepository,
                "ancestor of the repository root",
            ),
            (
                DeclarationRejection::BaseInsideRepository,
                "inside the repository",
            ),
            (
                DeclarationRejection::BaseInsideWorktree(p("/w/x")),
                "inside the worktree /w/x",
            ),
            (
                DeclarationRejection::Duplicate { of: 2 },
                "duplicate of entry 2",
            ),
            (
                DeclarationRejection::Nested { inside: 3 },
                "nested inside the base of entry 3",
            ),
        ];
        for (why, needle) in cases {
            assert!(
                why.describe().contains(needle),
                "{why:?}: {}",
                why.describe()
            );
        }
    }

    /// The lexical checks: root in `evict`, root a derived root, base
    /// absolute, then the suffix.
    #[test]
    fn declaration_lexical_conditions() {
        let base = p("/v/builds");
        assert_eq!(validate_declaration("target", &base, "", &evict()), Ok(()));
        assert_eq!(
            validate_declaration("cache", &base, "", &evict()),
            Err(DeclarationRejection::RootNotEvicted("cache".to_owned()))
        );
        assert!(matches!(
            validate_declaration("..", &base, "", &["..".to_owned()]),
            Err(DeclarationRejection::RootInvalid(_, _))
        ));
        assert_eq!(
            validate_declaration("target", &p("v/builds"), "", &evict()),
            Err(DeclarationRejection::BaseNotAbsolute)
        );
        assert_eq!(
            validate_declaration("target", &base, "a/b", &evict()),
            Err(DeclarationRejection::SuffixHasSlash)
        );
    }

    /// 18 — every forbidden location, and a sibling of the repository that
    /// is accepted.
    #[test]
    fn base_location_conditions() {
        let protected = ProtectedLocations {
            home: Some(p("/home/op")),
            repo_root: p("/src/repo"),
            worktrees: vec![p("/src/wt/task-20261118-aa01")],
        };
        let check = |b: &str| validate_base_location(&p(b), &protected);
        assert_eq!(check("/"), Err(DeclarationRejection::BaseIsFilesystemRoot));
        assert_eq!(check("/home/op"), Err(DeclarationRejection::BaseIsHome));
        assert_eq!(
            check("/src/repo"),
            Err(DeclarationRejection::BaseIsRepositoryRoot)
        );
        assert_eq!(
            check("/src"),
            Err(DeclarationRejection::BaseIsAncestorOfRepository)
        );
        assert_eq!(
            check("/src/repo/builds"),
            Err(DeclarationRejection::BaseInsideRepository)
        );
        assert_eq!(
            check("/src/wt/task-20261118-aa01/b"),
            Err(DeclarationRejection::BaseInsideWorktree(p(
                "/src/wt/task-20261118-aa01"
            )))
        );
        assert_eq!(check("/src/builds"), Ok(()));
        assert_eq!(check("/home/op/builds"), Ok(()), "below home is allowed");
        assert_eq!(
            check("/src/repo-builds"),
            Ok(()),
            "a name prefix is not containment"
        );
    }

    /// 28 — duplicates and nesting, by index; the same base with two
    /// suffixes is allowed.
    #[test]
    fn entry_set_refuses_duplicates_and_nesting() {
        let a = p("/v/a");
        let b = p("/v/b");
        let inner = p("/v/a/inner");
        assert_eq!(validate_entry_set(&[(0, &a, ""), (1, &b, "")]), Ok(()));
        assert_eq!(validate_entry_set(&[(0, &a, ""), (1, &a, "t")]), Ok(()));
        assert_eq!(
            validate_entry_set(&[(0, &a, "-wt"), (1, &a, "-wt")]),
            Err((1, DeclarationRejection::Duplicate { of: 0 }))
        );
        assert_eq!(
            validate_entry_set(&[(0, &a, ""), (1, &inner, "-wt")]),
            Err((1, DeclarationRejection::Nested { inside: 0 }))
        );
        assert_eq!(
            validate_entry_set(&[(0, &inner, "-wt"), (1, &a, "")]),
            Err((0, DeclarationRejection::Nested { inside: 1 })),
            "nesting is found whatever the declaration order"
        );
    }

    /// 28 — `task-20261118-b7d7t` reads as two molecules under `""` and `t`.
    #[test]
    fn a_name_with_two_readings_is_ambiguous() {
        let name = "task-20261118-b7d7t";
        assert_eq!(
            classify_base_child(name, &[(0, ""), (1, "t")]),
            ChildReading::Ambiguous(vec![0, 1])
        );
        assert_eq!(
            classify_base_child(name, &[(1, "t")]),
            ChildReading::Molecule {
                entry: 1,
                id: id("task-20261118-b7d7")
            }
        );
    }

    /// 26 — `<m>-wt` reads under `-wt`; `<m>` and `notanid-wt` do not.
    #[test]
    fn child_names_read_only_through_a_declared_suffix() {
        let m = "task-20261118-aa01";
        assert_eq!(
            classify_base_child(&format!("{m}-wt"), &[(3, "-wt")]),
            ChildReading::Molecule {
                entry: 3,
                id: id(m)
            }
        );
        assert_eq!(
            classify_base_child(m, &[(3, "-wt")]),
            ChildReading::NotAMolecule
        );
        assert_eq!(
            classify_base_child("notanid-wt", &[(3, "-wt")]),
            ChildReading::NotAMolecule
        );
        assert_eq!(
            classify_base_child(&format!("{m}-wt"), &[(0, "")]),
            ChildReading::NotAMolecule,
            "`-wt` does not parse as an id's last segment"
        );
    }

    /// D2 — every link refusal, and the acceptance it is measured against.
    #[test]
    fn link_verdicts() {
        let m = id("task-20261118-aa01");
        let entries = vec![entry(0, "/a", "-wt"), entry(1, "/b", "-wt")];
        let verdict = |t: &str| classify_link(Some(&m), Path::new("target"), &p(t), &entries);
        assert_eq!(
            verdict("/b/task-20261118-aa01-wt"),
            LinkVerdict::Accepted {
                entry: 1,
                base: p("/b")
            }
        );
        assert_eq!(
            verdict("/c/task-20261118-aa01-wt"),
            LinkVerdict::OutsideBases
        );
        assert_eq!(verdict("/a"), LinkVerdict::IsBase(p("/a")));
        assert_eq!(
            verdict("/a/task-20261118-aa01-wt/sub"),
            LinkVerdict::Deeper(p("/a"))
        );
        assert_eq!(
            verdict("/a/task-20261118-aa01"),
            LinkVerdict::NotOwnChild {
                base: p("/a"),
                reading: ChildReading::NotAMolecule
            }
        );
        assert_eq!(
            verdict("/a/task-20261118-aa02-wt"),
            LinkVerdict::NotOwnChild {
                base: p("/a"),
                reading: ChildReading::Molecule {
                    entry: 0,
                    id: id("task-20261118-aa02")
                }
            }
        );
        // A molecule-less candidate never reaches an external root.
        assert!(matches!(
            classify_link(
                None,
                Path::new("target"),
                &p("/a/task-20261118-aa01-wt"),
                &entries
            ),
            LinkVerdict::NotOwnChild { .. }
        ));
        // Entries of another root do not count for this one.
        assert_eq!(
            classify_link(
                Some(&m),
                Path::new("cache"),
                &p("/a/task-20261118-aa01-wt"),
                &entries
            ),
            LinkVerdict::OutsideBases
        );
    }

    /// D2 — a suffix declared on a different base does not apply here.
    #[test]
    fn a_suffix_of_another_base_does_not_apply() {
        let m = id("task-20261118-aa01");
        let entries = vec![entry(0, "/a", ""), entry(1, "/b", "-wt")];
        assert!(matches!(
            classify_link(
                Some(&m),
                Path::new("target"),
                &p("/a/task-20261118-aa01-wt"),
                &entries
            ),
            LinkVerdict::NotOwnChild { .. }
        ));
    }

    /// D2 — a child readable under an entry of another root of the same
    /// base is not this root's.
    #[test]
    fn a_reading_under_another_roots_entry_is_refused() {
        let m = id("task-20261118-aa01");
        let mut cache = entry(1, "/a", "-c");
        cache.root = p("cache");
        let entries = vec![entry(0, "/a", ""), cache];
        assert!(matches!(
            classify_link(
                Some(&m),
                Path::new("target"),
                &p("/a/task-20261118-aa01-c"),
                &entries
            ),
            LinkVerdict::NotOwnChild { .. }
        ));
    }

    /// 5, 25 — collisions by path and by device and inode (a bind mount).
    #[test]
    fn collisions_by_path_and_by_inode() {
        let d = |path: &str, id: Option<(u64, u64)>| DirIdentity {
            path: p(path),
            file_id: id,
        };
        assert_eq!(
            colliding(&[d("/a/x", Some((1, 2))), d("/a/y", Some((1, 3)))]),
            BTreeSet::new()
        );
        assert_eq!(
            colliding(&[
                d("/a/x", Some((1, 2))),
                d("/b/x", Some((1, 2))),
                d("/c", None)
            ]),
            BTreeSet::from([0, 1])
        );
        assert_eq!(
            colliding(&[d("/a/x", None), d("/a/x", None)]),
            BTreeSet::from([0, 1])
        );
        assert_eq!(
            colliding(&[d("/a/x", None), d("/a/y", None)]),
            BTreeSet::new(),
            "two unknown inodes are not equal"
        );
    }

    fn err() -> ObservationError {
        ObservationError::new("flock", "/a/x/debug/.cargo-lock", "EIO")
    }

    /// D5 — any held lock holds; any failed probe fails; no lock is no
    /// exclusion.
    #[test]
    fn combined_locks() {
        let d = Path::new("/a/x");
        use LockObservation::{Acquired, Held, ProbeFailed};
        assert_eq!(combine_locks(&[Acquired, Acquired], d), Acquired);
        assert_eq!(combine_locks(&[Acquired, Held], d), Held);
        assert_eq!(
            combine_locks(&[ProbeFailed(err()), Held], d),
            Held,
            "contention is reported over ignorance"
        );
        assert_eq!(
            combine_locks(&[Acquired, ProbeFailed(err())], d),
            ProbeFailed(err())
        );
        assert!(matches!(combine_locks(&[], d), ProbeFailed(_)));
    }

    fn permitting() -> ExternalObservation {
        ExternalObservation {
            molecule: MoleculeRecordObservation::Present,
            status: StatusObservation::Known(MoleculeStatus::Completed),
            lock: LockObservation::Acquired,
            unique: true,
            enumeration_complete: true,
            base_listed: true,
        }
    }

    /// D4 — Completed and Collapsed select; every other status, an absent
    /// record and an unreadable one withhold.
    #[test]
    fn the_external_gate_requires_a_present_terminal_record() {
        assert_eq!(external_selection(&permitting()), Ok(()));
        for status in MoleculeStatus::ALL {
            let mut obs = permitting();
            obs.status = StatusObservation::Known(status);
            let terminal = matches!(
                status,
                MoleculeStatus::Completed | MoleculeStatus::Collapsed
            );
            assert_eq!(external_selection(&obs).is_ok(), terminal, "{status:?}");
        }
        let mut obs = permitting();
        obs.molecule = MoleculeRecordObservation::Absent;
        assert_eq!(
            external_selection(&obs),
            Err(ExternalWithhold::RecordAbsent)
        );
        let mut obs = permitting();
        obs.molecule = MoleculeRecordObservation::Unknown(err());
        assert!(matches!(
            external_selection(&obs),
            Err(ExternalWithhold::RecordUnknown(_))
        ));
        let mut obs = permitting();
        obs.status = StatusObservation::Unknown(err());
        assert!(matches!(
            external_selection(&obs),
            Err(ExternalWithhold::StatusUnknown(_))
        ));
    }

    /// D2, D5, D6 — each remaining condition withholds on its own, and the
    /// completeness and uniqueness reasons win over a lock reason.
    #[test]
    fn every_other_condition_withholds_and_completeness_comes_first() {
        let mut obs = permitting();
        obs.lock = LockObservation::Held;
        assert_eq!(external_selection(&obs), Err(ExternalWithhold::LockHeld));
        obs.unique = false;
        assert_eq!(external_selection(&obs), Err(ExternalWithhold::NotUnique));
        obs.base_listed = false;
        assert_eq!(
            external_selection(&obs),
            Err(ExternalWithhold::BaseListingFailed)
        );
        obs.enumeration_complete = false;
        assert_eq!(
            external_selection(&obs),
            Err(ExternalWithhold::EnumerationIncomplete)
        );
        let mut obs = permitting();
        obs.lock = LockObservation::ProbeFailed(err());
        assert!(matches!(
            external_selection(&obs),
            Err(ExternalWithhold::LockProbeFailed(_))
        ));
        let mut obs = permitting();
        obs.molecule = MoleculeRecordObservation::Absent;
        obs.unique = false;
        assert_eq!(
            external_selection(&obs),
            Err(ExternalWithhold::NotUnique),
            "uniqueness is decided before the record"
        );
    }
}
