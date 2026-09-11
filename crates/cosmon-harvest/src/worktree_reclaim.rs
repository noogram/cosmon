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
        for root in &self.derived_roots {
            let path = candidate.join(root);
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
