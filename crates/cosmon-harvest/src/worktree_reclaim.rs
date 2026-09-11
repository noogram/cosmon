// SPDX-License-Identifier: AGPL-3.0-only

//! The Git/filesystem/`flock` adapter behind [`WorktreeObservationPort`], and
//! derived reclamation as a library operation.
//!
//! The predicates live in `cosmon_core::worktree_reclaim` and touch no I/O
//! (ADR-082). Everything that can fail — spawning Git, reading a status,
//! taking a lock — happens here, and every failure becomes a three-valued
//! `Unknown` carrying its [`ObservationError`] rather than a permissive
//! default. That inversion is the whole point of issue 61: both existing
//! consumers used to read "I could not check" as "there is nothing there".
//!
//! [`reclaim_derived`] is callable and tested but wired to **no CLI verb**.
//! No automatic path in this workspace removes a whole worktree.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use fs2::FileExt;

use cosmon_core::error::CosmonError;
use cosmon_core::id::MoleculeId;
use cosmon_core::worktree_reclaim::{
    selected_derived_paths, validate_derived_root, AheadObservation, DerivedObservation,
    DerivedRoots, DirtyObservation, IgnoredDurableObservation, LockObservation,
    MoleculeRecordObservation, ObservationError, RegistrationObservation, StatusObservation,
    WorktreeObservationPort,
};
use cosmon_state::StateStore;

/// The lock anchor, relative to a candidate worktree.
///
/// Cargo's own build lock: whoever holds it owns the build directory, which
/// is exactly the exclusion derived reclamation needs. It is an *anchor*, not
/// payload — selecting `target/` never deletes it, because holding a file
/// descriptor while unlinking its name does not stop another process from
/// creating a new inode at the same path.
pub const LOCK_ANCHOR: &str = "target/debug/.cargo-lock";

/// The default derived root set.
///
/// One entry today. The set is a field rather than a constant because the
/// contract requires a configurable set; the config *key spelling* is P3 and
/// is deliberately not chosen here.
pub const DEFAULT_DERIVED_ROOTS: [&str; 1] = ["target"];

/// Observes real worktrees with Git, the filesystem and `flock`.
///
/// Holds the acquired lock guards for its own lifetime: a
/// [`LockObservation::Acquired`] answer stays true for as long as the caller
/// can act on it. Dropping the observer releases them, which is why a caller
/// must keep it alive across the whole reclamation.
pub struct GitWorktreeObserver<'a> {
    /// Repository root, for `git worktree list` and ref resolution.
    repo_root: PathBuf,
    /// The base branch candidates are measured against.
    base_branch: String,
    /// Where molecule records are read from.
    store: &'a dyn StateStore,
    /// Validated, worktree-relative derived roots.
    derived_roots: Vec<PathBuf>,
    /// Acquired `flock` guards, keyed by candidate.
    guards: Mutex<HashMap<PathBuf, File>>,
    /// Roots that passed validation but whose **exclusion** could not be
    /// established, by candidate, with the reason. Recorded rather than
    /// dropped: a root that silently vanishes from the selection is the same
    /// invisible leak in the other direction.
    withheld_roots: Mutex<HashMap<PathBuf, Vec<(PathBuf, String)>>>,
}

impl<'a> GitWorktreeObserver<'a> {
    /// Build an observer over one repository.
    ///
    /// # Errors
    /// Returns the rejection of the first derived root that is not a
    /// rebuildable, worktree-local relative path — an invalid configuration
    /// must fail loudly rather than silently widening the selection.
    pub fn new(
        repo_root: impl Into<PathBuf>,
        base_branch: impl Into<String>,
        store: &'a dyn StateStore,
        derived_roots: &[&str],
    ) -> Result<Self, ObservationError> {
        let repo_root = repo_root.into();
        let mut validated = Vec::new();
        for root in derived_roots {
            let ok = validate_derived_root(Path::new(root)).map_err(|why| {
                ObservationError::new("validate derived root", *root, format!("{why:?}"))
            })?;
            validated.push(ok);
        }
        Ok(Self {
            repo_root,
            base_branch: base_branch.into(),
            store,
            derived_roots: validated,
            guards: Mutex::new(HashMap::new()),
            withheld_roots: Mutex::new(HashMap::new()),
        })
    }

    /// The molecule id a candidate directory claims by its name.
    fn molecule_id(candidate: &Path) -> Result<MoleculeId, ObservationError> {
        let name = candidate
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        MoleculeId::new(name).map_err(|e| {
            ObservationError::new(
                "parse molecule id from directory name",
                candidate,
                e.to_string(),
            )
        })
    }

    /// Run a Git command, mapping every failure mode to one error value.
    fn git(&self, cwd: &Path, args: &[&str]) -> Result<String, ObservationError> {
        let describe = format!("git {}", args.join(" "));
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .map_err(|e| ObservationError::new(describe.clone(), cwd, e.to_string()))?;
        if !out.status.success() {
            return Err(ObservationError::new(
                describe,
                cwd,
                format!(
                    "exit {}: {}",
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            ));
        }
        String::from_utf8(out.stdout)
            .map_err(|e| ObservationError::new(describe, cwd, e.to_string()))
    }

    /// Refuse a Git answer that came from the *parent* repository.
    ///
    /// Git walks up out of an unregistered scratch directory and answers as
    /// if it were the parent checkout. That answer is about the parent, not
    /// about the candidate, and reading it as evidence is how a scratch
    /// directory with the only copy of a note reads as clean and merged.
    fn assert_own_top_level(&self, candidate: &Path) -> Result<(), ObservationError> {
        let top = self.git(candidate, &["rev-parse", "--show-toplevel"])?;
        let top = PathBuf::from(top.trim());
        let same = std::fs::canonicalize(&top).ok() == std::fs::canonicalize(candidate).ok();
        if same {
            Ok(())
        } else {
            Err(ObservationError::new(
                "git rev-parse --show-toplevel",
                candidate,
                format!(
                    "answered for {} — parent-repository fallback",
                    top.display()
                ),
            ))
        }
    }

    /// Whether this observer can establish exclusion over one derived root.
    ///
    /// The only exclusion cosmon holds is Cargo's own build lock
    /// ([`LOCK_ANCHOR`]), and a lock excludes exactly the producers that take
    /// it. That is Cargo, writing into the build directory the anchor lives
    /// in. A root that does not contain the anchor — `build/ios` for a galaxy
    /// that also ships an iOS staticlib — has *no* established exclusion: the
    /// acquired flock says nothing about `xcodebuild`, which never asked for
    /// it. Such a root is withheld with that reason rather than reclaimed
    /// under a lock that does not cover it.
    fn exclusion_established(_root: &Path) -> bool {
        // PLACEHOLDER (red): the pre-P3 assumption — a configured root is a
        // reclaimable root. Implemented in the green commit.
        true
    }

    /// The reason a root without established exclusion is withheld.
    ///
    /// One function so the operator-facing register and the docs cannot
    /// drift: whatever an operator reads is this sentence.
    fn no_exclusion_reason(root: &Path) -> String {
        format!(
            "no exclusion protocol covers `{}`: the acquired lock is Cargo's \
             own build lock at `{LOCK_ANCHOR}`, which excludes Cargo and no \
             other producer",
            root.display()
        )
    }

    /// Roots enumerated for `candidate` that were withheld for want of an
    /// establishable exclusion, with the reason for each.
    ///
    /// Public because the withheld register is a user-facing obligation: a
    /// root cosmon declines to reclaim must be named with its reason, or the
    /// configuration silently does nothing and nobody finds out.
    #[must_use]
    pub fn withheld_roots(&self, candidate: &Path) -> Vec<(PathBuf, String)> {
        self.withheld_roots
            .lock()
            .ok()
            .and_then(|m| m.get(candidate).cloned())
            .unwrap_or_default()
    }

    /// `true` when `path` is the lock anchor or one of its ancestors.
    fn is_anchor_or_ancestor(candidate: &Path, path: &Path) -> bool {
        let anchor = candidate.join(LOCK_ANCHOR);
        anchor == path || anchor.starts_with(path)
    }

    /// Remove one derived root's payload, preserving the lock anchor.
    fn reclaim_root(
        candidate: &Path,
        root: &Path,
        report: &mut ReclaimReport,
    ) -> Result<(), ObservationError> {
        let entries = std::fs::read_dir(root)
            .map_err(|e| ObservationError::new("read derived root", root, e.to_string()))?;
        for entry in entries {
            let entry = entry
                .map_err(|e| ObservationError::new("read derived entry", root, e.to_string()))?;
            let path = entry.path();
            if Self::is_anchor_or_ancestor(candidate, &path) {
                if path == candidate.join(LOCK_ANCHOR) {
                    report.preserved.insert(path);
                } else {
                    // An ancestor of the anchor: descend, keep the directory.
                    Self::reclaim_root(candidate, &path, report)?;
                }
                continue;
            }
            let removed = if entry.path().is_dir() && !entry.path().is_symlink() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match removed {
                Ok(()) => {
                    report.removed.insert(path);
                }
                Err(e) => report.failures.push(ObservationError::new(
                    "remove derived payload",
                    &path,
                    e.to_string(),
                )),
            }
        }
        Ok(())
    }
}

/// What [`reclaim_derived`] did, by path.
///
/// Paths, never byte counts: an incident is actionable when it names
/// `…/target/debug/incremental`, not when it says "freed 4.2 GiB".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReclaimReport {
    /// Entries actually removed.
    pub removed: BTreeSet<PathBuf>,
    /// The synchronization anchor, kept deliberately.
    pub preserved: BTreeSet<PathBuf>,
    /// Per-path failures; partial progress is reported, never hidden.
    pub failures: Vec<ObservationError>,
}

/// Reclaim a candidate's derived payload, under a lock that is still held.
///
/// A library operation with **no CLI verb** (P3 owns the surface). The caller
/// must keep the [`GitWorktreeObserver`] that produced `obs` alive: it is
/// what holds the `flock`, and a dry-run observation is not mutation
/// authority — this function re-reads the predicate on the observation it is
/// given and does nothing unless it still selects.
///
/// # Errors
/// Returns an [`ObservationError`] when a selected root cannot be walked.
/// Per-entry failures are collected into [`ReclaimReport::failures`] instead,
/// so one undeletable file does not hide the rest of the work.
pub fn reclaim_derived(
    observer: &GitWorktreeObserver<'_>,
    obs: &DerivedObservation,
) -> Result<ReclaimReport, ObservationError> {
    let mut report = ReclaimReport::default();
    let selected = selected_derived_paths(obs);
    if selected.is_empty() {
        return Ok(report);
    }
    // Revalidate the guard rather than trusting the observation's age.
    let still_held = observer
        .guards
        .lock()
        .map(|g| g.contains_key(&obs.path))
        .unwrap_or(false);
    if !still_held {
        return Err(ObservationError::new(
            "revalidate lock guard",
            &obs.path,
            "the acquired flock guard is no longer held by this observer",
        ));
    }
    for root in &selected {
        if root.is_dir() {
            GitWorktreeObserver::reclaim_root(&obs.path, root, &mut report)?;
        }
    }
    Ok(report)
}

impl WorktreeObservationPort for GitWorktreeObserver<'_> {
    fn molecule_record(&self, candidate: &Path) -> MoleculeRecordObservation {
        let id = match Self::molecule_id(candidate) {
            Ok(id) => id,
            // A directory whose name is not a molecule id owns no molecule.
            Err(_) => return MoleculeRecordObservation::Absent,
        };
        match self.store.load_molecule(&id) {
            Ok(_) => MoleculeRecordObservation::Present,
            Err(CosmonError::MoleculeNotFound(_)) => MoleculeRecordObservation::Absent,
            Err(e) => MoleculeRecordObservation::Unknown(ObservationError::new(
                "load molecule",
                candidate,
                e.to_string(),
            )),
        }
    }

    /// Reads the status of the loaded record.
    ///
    /// With a typed store a decode failure fails the whole load, so this
    /// adapter reports `Unknown` exactly when [`Self::molecule_record`] does.
    /// The port keeps the axes separate because another store can fail to
    /// decode a status while the record itself is readable.
    fn molecule_status(&self, candidate: &Path) -> StatusObservation {
        let id = match Self::molecule_id(candidate) {
            Ok(id) => id,
            Err(e) => return StatusObservation::Unknown(e),
        };
        match self.store.load_molecule(&id) {
            Ok(mol) => StatusObservation::Known(mol.status),
            Err(e) => StatusObservation::Unknown(ObservationError::new(
                "load molecule status",
                candidate,
                e.to_string(),
            )),
        }
    }

    fn registration(&self, candidate: &Path) -> RegistrationObservation {
        let listing = match self.git(&self.repo_root, &["worktree", "list", "--porcelain"]) {
            Ok(out) => out,
            Err(e) => return RegistrationObservation::Unknown(e),
        };
        let target = std::fs::canonicalize(candidate).ok();
        let registered = listing
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .any(|p| std::fs::canonicalize(p).ok() == target && target.is_some());
        if registered {
            RegistrationObservation::Registered
        } else {
            RegistrationObservation::Unregistered
        }
    }

    fn lock(&self, candidate: &Path) -> LockObservation {
        let anchor = candidate.join(LOCK_ANCHOR);
        // Deliberately no `create(true)`: a missing anchor is ProbeFailed,
        // never Acquired. Creating it would manufacture the exclusion the
        // observation is supposed to *find*.
        let file = match File::options().read(true).write(true).open(&anchor) {
            Ok(f) => f,
            Err(e) => {
                return LockObservation::ProbeFailed(ObservationError::new(
                    "open lock anchor",
                    &anchor,
                    e.to_string(),
                ))
            }
        };
        match file.try_lock_exclusive() {
            Ok(()) => match self.guards.lock() {
                Ok(mut guards) => {
                    // Retain the guard: the answer must stay true while the
                    // caller acts on it.
                    guards.insert(candidate.to_path_buf(), file);
                    LockObservation::Acquired
                }
                Err(e) => LockObservation::ProbeFailed(ObservationError::new(
                    "retain lock guard",
                    &anchor,
                    e.to_string(),
                )),
            },
            Err(_) => LockObservation::Held,
        }
    }

    fn commits_ahead(&self, candidate: &Path) -> AheadObservation {
        if let Err(e) = self.assert_own_top_level(candidate) {
            return AheadObservation::Unknown(e);
        }
        // The *actual* candidate HEAD, never a synthesized `feat/<id>`: a ref
        // that does not exist must not read as merged.
        let head = match self.git(candidate, &["rev-parse", "--verify", "HEAD"]) {
            Ok(h) => h.trim().to_owned(),
            Err(e) => return AheadObservation::Unknown(e),
        };
        let base = format!("refs/heads/{}", self.base_branch);
        if let Err(e) = self.git(&self.repo_root, &["rev-parse", "--verify", &base]) {
            return AheadObservation::Unknown(e);
        }
        let range = format!("{base}..{head}");
        match self.git(&self.repo_root, &["rev-list", "--count", &range]) {
            Ok(out) => match out.trim().parse::<usize>() {
                Ok(0) => AheadObservation::Zero,
                Ok(n) => AheadObservation::Positive(n),
                Err(e) => AheadObservation::Unknown(ObservationError::new(
                    format!("git rev-list --count {range}"),
                    candidate,
                    e.to_string(),
                )),
            },
            Err(e) => AheadObservation::Unknown(e),
        }
    }

    fn dirty(&self, candidate: &Path) -> DirtyObservation {
        observe_dirty(candidate)
    }

    fn ignored_durable(&self, candidate: &Path) -> IgnoredDurableObservation {
        if let Err(e) = self.assert_own_top_level(candidate) {
            return IgnoredDurableObservation::Unknown(e);
        }
        let listing = match self.git(
            candidate,
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
            ],
        ) {
            Ok(out) => out,
            Err(e) => return IgnoredDurableObservation::Unknown(e),
        };
        let durable: Vec<String> = listing
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter(|l| {
                // Ignored content inside a validated derived root is
                // rebuildable; everything else ignored is durable.
                !self
                    .derived_roots
                    .iter()
                    .any(|root| Path::new(l).starts_with(root))
            })
            .map(str::to_owned)
            .collect();
        if durable.is_empty() {
            IgnoredDurableObservation::Absent
        } else {
            IgnoredDurableObservation::Present(durable)
        }
    }

    fn derived_roots(&self, candidate: &Path) -> DerivedRoots {
        let candidate_real = match std::fs::canonicalize(candidate) {
            Ok(p) => p,
            Err(e) => {
                return DerivedRoots::Unavailable(ObservationError::new(
                    "canonicalize candidate",
                    candidate,
                    e.to_string(),
                ))
            }
        };
        let mut roots = BTreeSet::new();
        let mut withheld = Vec::new();
        for root in &self.derived_roots {
            let path = candidate.join(root);
            if !Self::exclusion_established(root) {
                // Enumerated, reported, not selected. The contract's
                // "adapters must establish their applicable exclusion or
                // withhold those roots" is this branch.
                withheld.push((path, Self::no_exclusion_reason(root)));
                continue;
            }
            if !path.exists() {
                // A known-absent derived path contributes the empty set; it
                // is not a failure.
                continue;
            }
            // Symlink escape is decided here, where the filesystem is.
            match std::fs::canonicalize(&path) {
                Ok(real) if real.starts_with(&candidate_real) => {
                    roots.insert(path);
                }
                Ok(real) => {
                    return DerivedRoots::Unavailable(ObservationError::new(
                        "contain derived root",
                        &path,
                        format!("resolves outside the candidate, to {}", real.display()),
                    ))
                }
                Err(e) => {
                    return DerivedRoots::Unavailable(ObservationError::new(
                        "canonicalize derived root",
                        &path,
                        e.to_string(),
                    ))
                }
            }
        }
        if let Ok(mut register) = self.withheld_roots.lock() {
            register.insert(candidate.to_path_buf(), withheld);
        }
        DerivedRoots::Validated(roots)
    }
}

/// The one dirty probe both existing consumers share.
///
/// `cs purge`'s sweep and the harvest transaction's teardown used to have one
/// fail-open each — an empty list on a failed `git status`, and a warning
/// followed by removal. Two copies of a safety question drift; this is one
/// copy, and its failure arm is a value the caller cannot mistake for clean.
#[must_use]
pub fn observe_dirty(worktree: &Path) -> DirtyObservation {
    if !worktree.is_dir() {
        // Proven absence, distinct from a failed probe: there is no tree.
        return DirtyObservation::Clean;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["status", "--porcelain"])
        .output();
    let out = match out {
        Ok(out) => out,
        Err(e) => {
            return DirtyObservation::Unknown(ObservationError::new(
                "git status --porcelain",
                worktree,
                e.to_string(),
            ))
        }
    };
    if !out.status.success() {
        return DirtyObservation::Unknown(ObservationError::new(
            "git status --porcelain",
            worktree,
            format!(
                "exit {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    let stdout = match String::from_utf8(out.stdout) {
        Ok(s) => s,
        Err(e) => {
            return DirtyObservation::Unknown(ObservationError::new(
                "decode git status output",
                worktree,
                e.to_string(),
            ))
        }
    };
    let lines: Vec<String> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    if lines.is_empty() {
        DirtyObservation::Clean
    } else {
        DirtyObservation::Dirty(lines)
    }
}

// ---------------------------------------------------------------------------
// The enumeration root
// ---------------------------------------------------------------------------

/// The result of listing `<repo>/.worktrees/`.
///
/// Three-valued for the same reason every axis above is: "there is no
/// `.worktrees/` directory" and "I could not read `.worktrees/`" are opposite
/// answers, and a single `Vec` that is empty in both cases is exactly the
/// fail-open this issue exists to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeDirs {
    /// There is no `.worktrees/` directory. Nothing to enumerate.
    Absent,
    /// The directories found, in readdir order filtered to directories.
    Listed(Vec<PathBuf>),
    /// The listing itself failed.
    Unreadable(ObservationError),
}

/// List the immediate subdirectories of `<repo>/.worktrees/`.
///
/// **The** readdir walker over the worktree root. `cs doctor worktrees` and
/// `cs purge --worktrees` both call it, because two walkers over the same
/// directory are two opportunities to disagree about what a worktree *is* —
/// and the disagreement is invisible until an operator compares two commands'
/// output by hand.
#[must_use]
pub fn read_worktree_dirs(worktrees_root: &Path) -> WorktreeDirs {
    if !worktrees_root.exists() {
        return WorktreeDirs::Absent;
    }
    let entries = match std::fs::read_dir(worktrees_root) {
        Ok(it) => it,
        Err(e) => {
            return WorktreeDirs::Unreadable(ObservationError::new(
                "read .worktrees/",
                worktrees_root,
                e.to_string(),
            ))
        }
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    WorktreeDirs::Listed(dirs)
}

/// Where a candidate was found.
///
/// Carried because the two halves of the union answer different questions.
/// A directory Git does not know about is not thereby safe to remove — it is
/// precisely the class that has no molecule, no registration and no owner,
/// and the class the pre-issue-61 roster could not see at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateSource {
    /// Present under `.worktrees/` but absent from `git worktree list`.
    FilesystemOnly,
    /// Registered with Git but with no directory under `.worktrees/`.
    RegistrationOnly,
    /// Both — the ordinary case for a live worker.
    Both,
}

impl CandidateSource {
    /// The operator-facing name of this provenance.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FilesystemOnly => "on disk, unregistered",
            Self::RegistrationOnly => "registered, no directory",
            Self::Both => "on disk and registered",
        }
    }
}

/// One enumerated candidate directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumeratedCandidate {
    /// The candidate directory.
    pub path: PathBuf,
    /// Which half (or both) of the union produced it.
    pub source: CandidateSource,
}

/// Every candidate directory, and every failure met while finding them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Enumeration {
    /// The union, sorted by path.
    pub candidates: Vec<EnumeratedCandidate>,
    /// Failures that may have hidden candidates. A non-empty list means the
    /// enumeration is **incomplete**, and a caller must say so rather than
    /// present a partial list as the whole population.
    pub errors: Vec<ObservationError>,
}

/// Enumerate reclamation candidates: `readdir(.worktrees/)` ∪
/// `git worktree list --porcelain`.
///
/// Every prior mechanism in this workspace was keyed by *molecule* or by
/// *worker*, so a directory with neither was unreachable by construction —
/// on this repository the worker roster saw 2 of 14 directories and 5 had no
/// molecule at all. The enumeration root is therefore the filesystem and
/// Git's own registry; the molecule is consulted afterwards, as a **veto**
/// (`consideration_gate`), never as the way a candidate is found.
///
/// The repository root itself is excluded: it is the checkout, not a
/// reclamation candidate.
#[must_use]
pub fn enumerate_candidates(_repo_root: &Path) -> Enumeration {
    // PLACEHOLDER (red): nothing enumerates candidates yet — every prior
    // mechanism was keyed by molecule or by worker, so the population is
    // empty here. Implemented in the green commit.
    Enumeration::default()
}

/// Bytes occupied under `path`, following no symlink.
///
/// Presentation only. The selection sets are path sets and no decision in
/// this module reads a byte count — but an operator deciding whether to act
/// on a withheld register needs to know whether it is holding a megabyte or
/// forty gigabytes, and "N worktrees withheld" alone does not say.
/// Unreadable entries are skipped: a size that is approximate and cheap is
/// worth more here than one that can fail.
#[must_use]
pub fn approximate_size_bytes(path: &Path) -> u64 {
    let mut total = 0_u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Render a byte count the way an operator reads one.
///
/// Alongside [`approximate_size_bytes`] so the unit and the number are chosen
/// in one place; a register that prints raw bytes for a 40 GiB worktree is a
/// number nobody parses at a glance.
#[must_use]
pub fn describe_size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Tests — real Git fixtures, a real second process, real files
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::worktree_reclaim::{
        advisory_durable_paths, durable_eligibility, DurableEligibility,
    };
    use cosmon_filestore::FileStore;
    use cosmon_state::MoleculeData;
    use tempfile::TempDir;

    /// Run git in `cwd`, isolated from the developer's own configuration.
    fn git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn molecule(id: &str, status: &str) -> MoleculeData {
        let now = chrono::Utc::now().to_rfc3339();
        serde_json::from_value(serde_json::json!({
            "id": id,
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": status,
            "created_at": now,
            "updated_at": now,
            "total_steps": 2,
            "current_step": 2,
            "variables": {},
            "completed_steps": [],
            "links": [],
            "typed_links": [],
            "escalations": [],
            "briefing_seals": [],
            "bootstrap_seals": [],
            "tags": [],
        }))
        .unwrap()
    }

    /// A repository, one registered worktree, a molecule record, and a
    /// `target/` tree with the Cargo lock anchor and some payload.
    struct Fixture {
        _tmp: TempDir,
        repo: PathBuf,
        worktree: PathBuf,
        store: FileStore,
        id: String,
    }

    fn fixture(status: &str) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Noogram"]);
        git(&repo, &["config", "user.email", "maintainers@noogram.org"]);
        std::fs::write(repo.join(".gitignore"), ".worktrees/\ntarget/\nnote.txt\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "test: seed"],
        );
        let id = "task-20260911-0000".to_owned();
        let worktree = repo.join(".worktrees").join(&id);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-qb",
                &format!("feat/{id}"),
                &worktree.to_string_lossy(),
            ],
        );
        std::fs::create_dir_all(worktree.join("target/debug/incremental")).unwrap();
        std::fs::write(worktree.join(LOCK_ANCHOR), "").unwrap();
        std::fs::write(worktree.join("target/debug/incremental/a.o"), "object").unwrap();
        std::fs::write(worktree.join("target/CACHEDIR.TAG"), "tag").unwrap();
        let store = FileStore::new(tmp.path());
        let mol = molecule(&id, status);
        store.save_molecule(&mol.id, &mol).unwrap();
        Fixture {
            _tmp: tmp,
            repo,
            worktree,
            store,
            id,
        }
    }

    impl Fixture {
        fn observer<'a>(&'a self) -> GitWorktreeObserver<'a> {
            GitWorktreeObserver::new(&self.repo, "main", &self.store, &DEFAULT_DERIVED_ROOTS)
                .unwrap()
        }
    }

    /// The reclaim operation: an acquired lock selects `target/`, its payload
    /// goes, and the synchronization anchor stays — selecting `target` names
    /// its payload, not its identity.
    #[test]
    fn acquired_lock_reclaims_the_payload_and_preserves_the_anchor() {
        let f = fixture("collapsed");
        let observer = f.observer();
        let obs = observer.observe(&f.worktree);
        assert_eq!(obs.derived.lock, LockObservation::Acquired);
        assert_eq!(
            selected_derived_paths(&obs.derived),
            BTreeSet::from([f.worktree.join("target")])
        );
        let report = reclaim_derived(&observer, &obs.derived).unwrap();
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(report
            .removed
            .contains(&f.worktree.join("target/CACHEDIR.TAG")));
        assert!(report
            .removed
            .contains(&f.worktree.join("target/debug/incremental")));
        assert_eq!(
            report.preserved,
            BTreeSet::from([f.worktree.join(LOCK_ANCHOR)])
        );
        assert!(f.worktree.join(LOCK_ANCHOR).is_file());
        assert!(!f.worktree.join("target/debug/incremental").exists());
        assert!(!f.worktree.join("target/CACHEDIR.TAG").exists());
        // The worktree itself is never touched by any automatic path.
        assert!(f.worktree.join(".gitignore").is_file());
    }

    /// Falsifier 1 (P3) — the class every prior mechanism was blind to.
    ///
    /// A directory under `.worktrees/` with **no molecule record and no Git
    /// registration**. Everything before issue 61's P3 was keyed by molecule
    /// or by worker, so this directory was unreachable by construction; on
    /// the reporting repository five of fourteen were in this class. The
    /// enumeration root finds it, its durable axes resolve to `Unknown` /
    /// `Unregistered` so eligibility **withholds**, and its derived payload
    /// is still selectable once the lock is acquired.
    #[test]
    fn issue61_molecule_less_directory_is_enumerated_withheld_and_still_yields_derived() {
        let f = fixture("collapsed");
        // A leftover directory: a plausible molecule id, no record, and Git
        // was never told about it.
        let orphan = f.repo.join(".worktrees").join("task-20260101-dead");
        std::fs::create_dir_all(orphan.join("target/debug")).unwrap();
        std::fs::write(orphan.join(LOCK_ANCHOR), "").unwrap();
        std::fs::write(orphan.join("target/debug/big.o"), "payload").unwrap();

        let found = enumerate_candidates(&f.repo);
        assert!(
            found.errors.is_empty(),
            "enumeration must be complete here: {:?}",
            found.errors
        );
        let entry = found
            .candidates
            .iter()
            .find(|c| c.path.ends_with("task-20260101-dead"))
            .expect("the molecule-less directory must be enumerated");
        assert_eq!(entry.source, CandidateSource::FilesystemOnly);
        // The registered worktree is found too, by both halves.
        let registered = found
            .candidates
            .iter()
            .find(|c| c.path.ends_with(&f.id))
            .expect("the registered worktree must be enumerated");
        assert_eq!(registered.source, CandidateSource::Both);

        let observer = f.observer();
        let obs = observer.observe(&orphan);
        assert_eq!(
            obs.durable.registration,
            RegistrationObservation::Unregistered
        );
        assert!(
            matches!(obs.durable.ahead, AheadObservation::Unknown(_)),
            "ancestry must be Unknown for a directory git does not own: {:?}",
            obs.durable.ahead
        );
        assert_eq!(
            durable_eligibility(&obs.durable),
            DurableEligibility::Withhold,
            "unregistered scratch can never pass durable eligibility"
        );
        assert!(advisory_durable_paths(&obs.durable).is_empty());

        // …and yet the rebuildable half is available.
        assert_eq!(obs.derived.lock, LockObservation::Acquired);
        assert_eq!(
            selected_derived_paths(&obs.derived),
            BTreeSet::from([orphan.join("target")])
        );
        let report = reclaim_derived(&observer, &obs.derived).unwrap();
        assert!(report.removed.contains(&orphan.join("target/debug/big.o")));
        // The directory itself survives. It always does.
        assert!(orphan.is_dir());
    }

    /// Falsifier 4 (P3) — a configured root whose exclusion cannot be
    /// established is withheld, not selected.
    ///
    /// The acquired lock is Cargo's build lock. It excludes Cargo. A galaxy
    /// that also ships an iOS staticlib can configure `build/ios`, and
    /// `xcodebuild` never asked for that lock — so the root is enumerated,
    /// reported with that reason, and left alone.
    #[test]
    fn issue61_root_without_establishable_exclusion_is_withheld_with_a_reason() {
        let f = fixture("collapsed");
        std::fs::create_dir_all(f.worktree.join("build/ios")).unwrap();
        std::fs::write(f.worktree.join("build/ios/lib.a"), "archive").unwrap();

        let observer =
            GitWorktreeObserver::new(&f.repo, "main", &f.store, &["target", "build/ios"]).unwrap();
        let obs = observer.observe(&f.worktree);

        // Selected: the Cargo root only.
        assert_eq!(
            selected_derived_paths(&obs.derived),
            BTreeSet::from([f.worktree.join("target")]),
            "a non-Cargo root must not ride in on the Cargo lock"
        );

        // Withheld, by name, with the reason an operator will read.
        let withheld = observer.withheld_roots(&f.worktree);
        assert_eq!(withheld.len(), 1, "{withheld:?}");
        let (path, reason) = &withheld[0];
        assert_eq!(path, &f.worktree.join("build/ios"));
        assert!(reason.contains("build/ios"), "{reason}");
        assert!(reason.contains("no exclusion protocol"), "{reason}");
        assert!(reason.contains(LOCK_ANCHOR), "{reason}");

        // And the bytes are still there after a real reclamation.
        reclaim_derived(&observer, &obs.derived).unwrap();
        assert!(f.worktree.join("build/ios/lib.a").is_file());
    }

    /// The shared `.worktrees/` walker is three-valued, like every other
    /// observation in this module: absent and unreadable are not the same
    /// answer, and neither is an empty list.
    #[test]
    fn issue61_worktree_dir_listing_distinguishes_absent_from_unreadable() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            read_worktree_dirs(&tmp.path().join(".worktrees")),
            WorktreeDirs::Absent
        );
        let root = tmp.path().join(".worktrees");
        std::fs::create_dir(&root).unwrap();
        assert_eq!(read_worktree_dirs(&root), WorktreeDirs::Listed(Vec::new()));
        std::fs::create_dir(root.join("a")).unwrap();
        std::fs::write(root.join("not-a-dir"), "x").unwrap();
        assert_eq!(
            read_worktree_dirs(&root),
            WorktreeDirs::Listed(vec![root.join("a")])
        );
    }

    /// Real exclusion: a **second process** holds the flock, the predicate
    /// reports `Held`, and nothing is removed.
    #[test]
    fn a_second_process_holding_the_lock_yields_held_and_removes_nothing() {
        let f = fixture("collapsed");
        let anchor = f.worktree.join(LOCK_ANCHOR);
        let ready = f.worktree.join("target/holder.ready");
        let mut holder = Command::new("python3")
            .arg("-c")
            .arg(
                "import fcntl,sys,time\n\
                 f=open(sys.argv[1],'r+')\n\
                 fcntl.flock(f, fcntl.LOCK_EX|fcntl.LOCK_NB)\n\
                 open(sys.argv[2],'w').write('1')\n\
                 time.sleep(30)\n",
            )
            .arg(&anchor)
            .arg(&ready)
            .spawn()
            .expect("python3 is a build-time dependency of this repository's gates");
        let start = std::time::Instant::now();
        while !ready.exists() && start.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(ready.exists(), "the holder process never took the lock");

        let observer = f.observer();
        let obs = observer.observe(&f.worktree);
        assert_eq!(obs.derived.lock, LockObservation::Held);
        assert_eq!(selected_derived_paths(&obs.derived), BTreeSet::new());
        let report = reclaim_derived(&observer, &obs.derived).unwrap();
        assert_eq!(report, ReclaimReport::default());
        assert!(f.worktree.join("target/debug/incremental/a.o").is_file());
        assert!(f.worktree.join("target/CACHEDIR.TAG").is_file());

        holder.kill().ok();
        holder.wait().ok();
    }

    /// A missing anchor is `ProbeFailed`, never `Acquired`: the observer does
    /// not create the lock file it is supposed to find.
    #[test]
    fn missing_lock_anchor_is_probe_failed_never_acquired() {
        let f = fixture("collapsed");
        std::fs::remove_file(f.worktree.join(LOCK_ANCHOR)).unwrap();
        let observer = f.observer();
        let obs = observer.observe(&f.worktree);
        match &obs.derived.lock {
            LockObservation::ProbeFailed(e) => assert_eq!(e.operation, "open lock anchor"),
            other => panic!("expected ProbeFailed, got {other:?}"),
        }
        assert_eq!(selected_derived_paths(&obs.derived), BTreeSet::new());
        assert!(!f.worktree.join(LOCK_ANCHOR).exists(), "anchor recreated");
        assert!(f.worktree.join("target/debug/incremental/a.o").is_file());
    }

    /// An ignored note outside the derived set is durable and withholds;
    /// ignored content *inside* `target/` does not.
    #[test]
    fn ignored_note_is_durable_but_ignored_build_output_is_not() {
        let f = fixture("collapsed");
        let observer = f.observer();
        // Only `target/` is ignored so far.
        assert_eq!(
            observer.ignored_durable(&f.worktree),
            IgnoredDurableObservation::Absent
        );
        std::fs::write(f.worktree.join("note.txt"), "the only copy").unwrap();
        match observer.ignored_durable(&f.worktree) {
            IgnoredDurableObservation::Present(paths) => {
                assert_eq!(paths, vec!["note.txt".to_owned()])
            }
            other => panic!("expected Present, got {other:?}"),
        }
        let obs = observer.observe(&f.worktree);
        assert_eq!(
            durable_eligibility(&obs.durable),
            DurableEligibility::Withhold
        );
        assert!(advisory_durable_paths(&obs.durable).is_empty());
        // The note survives; derived selection is unaffected by it.
        assert!(f.worktree.join("note.txt").is_file());
    }

    /// A registered, clean, merged, note-free worktree is advisorily
    /// eligible — and still nothing removes it.
    #[test]
    fn fully_reachable_worktree_is_advisory_eligible_only() {
        let f = fixture("collapsed");
        let observer = f.observer();
        let obs = observer.observe(&f.worktree);
        assert_eq!(
            obs.durable.registration,
            RegistrationObservation::Registered
        );
        assert_eq!(obs.durable.ahead, AheadObservation::Zero);
        assert_eq!(obs.durable.dirty, DirtyObservation::Clean);
        assert_eq!(
            obs.durable.ignored_durable,
            IgnoredDurableObservation::Absent
        );
        assert_eq!(
            advisory_durable_paths(&obs.durable),
            BTreeSet::from([f.worktree.clone()])
        );
        assert!(f.worktree.is_dir(), "eligibility removed nothing");
    }

    /// A commit on the candidate's own HEAD is `Positive`, and withholds.
    #[test]
    fn a_commit_ahead_of_base_withholds_durable() {
        let f = fixture("collapsed");
        std::fs::write(f.worktree.join("deliverable.md"), "work").unwrap();
        git(&f.worktree, &["add", "deliverable.md"]);
        git(
            &f.worktree,
            &["-c", "commit.gpgsign=false", "commit", "-qm", "feat: work"],
        );
        let observer = f.observer();
        let obs = observer.observe(&f.worktree);
        assert_eq!(obs.durable.ahead, AheadObservation::Positive(1));
        assert!(advisory_durable_paths(&obs.durable).is_empty());
        // The lock differential still holds on the same row.
        assert_eq!(
            selected_derived_paths(&obs.derived),
            BTreeSet::from([f.worktree.join("target")])
        );
    }

    /// An unregistered scratch directory: Git walks up into the parent
    /// repository, and that answer is refused rather than believed.
    #[test]
    fn unregistered_scratch_is_unregistered_and_its_ancestry_unknown() {
        let f = fixture("collapsed");
        let scratch = f.repo.join(".worktrees/task-20260911-0001");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("note.txt"), "another unique note").unwrap();
        let observer = f.observer();
        assert_eq!(
            observer.registration(&scratch),
            RegistrationObservation::Unregistered
        );
        match observer.commits_ahead(&scratch) {
            AheadObservation::Unknown(e) => {
                assert!(e.cause.contains("parent-repository fallback"), "{e:?}")
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        assert_eq!(
            observer.molecule_record(&scratch),
            MoleculeRecordObservation::Absent
        );
        let obs = observer.observe(&scratch);
        assert!(advisory_durable_paths(&obs.durable).is_empty());
        assert!(scratch.join("note.txt").is_file());
    }

    /// A nonterminal molecule vetoes both predicates on a real fixture.
    #[test]
    fn running_molecule_vetoes_both_predicates() {
        let f = fixture("running");
        let observer = f.observer();
        let obs = observer.observe(&f.worktree);
        assert_eq!(
            obs.derived.status,
            StatusObservation::Known(cosmon_core::molecule::MoleculeStatus::Running)
        );
        assert_eq!(selected_derived_paths(&obs.derived), BTreeSet::new());
        assert!(advisory_durable_paths(&obs.durable).is_empty());
        assert!(f.worktree.join("target/debug/incremental/a.o").is_file());
        assert_eq!(f.id, "task-20260911-0000");
    }

    /// The shared dirty probe preserves its error instead of reading as clean.
    #[test]
    fn failed_dirty_probe_is_unknown_with_its_diagnostic() {
        let tmp = TempDir::new().unwrap();
        let not_a_repo = tmp.path().join("not-a-repo");
        std::fs::create_dir(&not_a_repo).unwrap();
        match observe_dirty(&not_a_repo) {
            DirtyObservation::Unknown(e) => {
                assert_eq!(e.operation, "git status --porcelain");
                assert_eq!(e.path, not_a_repo);
                assert!(!e.cause.is_empty());
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        // An absent tree is proven-clean, which is not the same value.
        assert_eq!(
            observe_dirty(&tmp.path().join("gone")),
            DirtyObservation::Clean
        );
    }

    /// A derived root that escapes the candidate through a symlink is refused
    /// by the adapter, where the filesystem can decide it.
    #[test]
    #[cfg(unix)]
    fn symlinked_derived_root_is_unavailable() {
        let f = fixture("collapsed");
        let outside = f.repo.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(f.worktree.join("target")).unwrap();
        std::os::unix::fs::symlink(&outside, f.worktree.join("target")).unwrap();
        let observer = f.observer();
        match observer.derived_roots(&f.worktree) {
            DerivedRoots::Unavailable(e) => assert_eq!(e.operation, "contain derived root"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert!(outside.is_dir());
    }
}
