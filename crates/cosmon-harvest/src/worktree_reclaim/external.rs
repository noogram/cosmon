// SPDX-License-Identifier: AGPL-3.0-only

//! The filesystem half of ADR-191: external derived roots and orphans.
//!
//! The decisions are the pure functions of
//! [`cosmon_core::worktree_reclaim::external`]; this module observes what
//! they read. It never assumes a layout: every `w/<root>` with a declared
//! entry is read with `lstat`, a link is resolved and canonicalized, and the
//! directory it reaches is identified by path **and** by device and inode.
//! The only "in use" test is Cargo's build lock — every
//! `<profile>/.cargo-lock` and `<triple>/<profile>/.cargo-lock` present —
//! never a process listing and never a modification time.
//!
//! Execution re-resolves each link (or re-reads each orphan) under the held
//! locks and refuses when the directory is no longer the one observed.

use std::collections::{BTreeSet, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use cosmon_core::config::ExternalDerivedRootConfig;
use cosmon_core::error::CosmonError;
use cosmon_core::id::MoleculeId;
use cosmon_core::worktree_reclaim::external::{
    classify_base_child, classify_link, colliding, combine_locks, external_selection, suffixes_of,
    validate_base_location, validate_declaration, validate_entry_set, ChildReading, DirIdentity,
    ExternalObservation, ExternalWithhold, LinkVerdict, ProtectedLocations, ReachableEntry,
};
use cosmon_core::worktree_reclaim::{
    LockObservation, MoleculeRecordObservation, ObservationError, StatusObservation,
};

use super::{approximate_size_bytes, GitWorktreeObserver, ReclaimReport};

/// A declared entry whose base did not exist or canonicalize at pass start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableBase {
    /// The base as declared.
    pub base: PathBuf,
    /// Why it could not be reached.
    pub error: ObservationError,
}

/// The validated `[[worktree_reclaim.external]]` entries of one pass.
///
/// Built once per pass by [`ExternalDeclarations::resolve`], which refuses
/// an invalid declaration before anything is observed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalDeclarations {
    /// Entries whose base canonicalized.
    reachable: Vec<ReachableEntry>,
    /// Entries whose base did not, reported and skipped.
    unreachable: Vec<UnreachableBase>,
    /// Every root with at least one entry, reachable or not.
    roots: BTreeSet<PathBuf>,
}

impl ExternalDeclarations {
    /// Validate and resolve every entry (ADR-191 D1).
    ///
    /// `worktrees` and `home` feed the forbidden-location rule; both are
    /// canonicalized here when they exist.
    ///
    /// # Errors
    /// A sentence naming the entry, its base and the condition it fails.
    pub fn resolve(
        decls: &[ExternalDerivedRootConfig],
        evict: &[String],
        repo_root: &Path,
        worktrees: &[PathBuf],
        home: Option<&Path>,
    ) -> Result<Self, String> {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let protected = ProtectedLocations {
            home: home.map(canon),
            repo_root: canon(repo_root),
            worktrees: worktrees.iter().map(|w| canon(w)).collect(),
        };
        let refuse = |i: usize, base: &Path, why: String| {
            format!(
                "[[worktree_reclaim.external]] entry {i} (base {}): {why}",
                base.display()
            )
        };
        let mut out = Self::default();
        for (i, d) in decls.iter().enumerate() {
            validate_declaration(&d.root, &d.base, &d.suffix, evict)
                .map_err(|why| refuse(i, &d.base, why.describe()))?;
            out.roots.insert(PathBuf::from(&d.root));
            match std::fs::canonicalize(&d.base) {
                Ok(base) if base.is_dir() => {
                    validate_base_location(&base, &protected)
                        .map_err(|why| refuse(i, &d.base, why.describe()))?;
                    out.reachable.push(ReachableEntry {
                        index: i,
                        root: PathBuf::from(&d.root),
                        base,
                        suffix: d.suffix.clone(),
                    });
                }
                other => {
                    // Checked lexically: nothing under it is touched.
                    validate_base_location(&d.base, &protected)
                        .map_err(|why| refuse(i, &d.base, why.describe()))?;
                    let cause = match other {
                        Ok(_) => "not a directory".to_owned(),
                        Err(e) => e.to_string(),
                    };
                    out.unreachable.push(UnreachableBase {
                        base: d.base.clone(),
                        error: ObservationError::new("canonicalize external base", &d.base, cause),
                    });
                }
            }
        }
        let set: Vec<(usize, &Path, &str)> = out
            .reachable
            .iter()
            .map(|e| (e.index, e.base.as_path(), e.suffix.as_str()))
            .collect();
        if let Err((i, why)) = validate_entry_set(&set) {
            return Err(refuse(i, &decls[i].base, why.describe()));
        }
        Ok(out)
    }

    /// Whether `root` has at least one entry.
    #[must_use]
    pub fn declares(&self, root: &Path) -> bool {
        self.roots.contains(root)
    }

    /// The distinct canonical reachable bases, in declaration order.
    fn bases(&self) -> Vec<PathBuf> {
        let mut seen = Vec::new();
        for e in &self.reachable {
            if !seen.contains(&e.base) {
                seen.push(e.base.clone());
            }
        }
        seen
    }
}

/// How an external directory was reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalKind {
    /// Through a worktree's link.
    Linked {
        /// The worktree.
        worktree: PathBuf,
        /// The link `worktree/<root>`.
        link: PathBuf,
    },
    /// As a direct child of a base that no live link reaches.
    Orphan,
}

impl ExternalKind {
    /// `linked` or `orphan`, for the register and `--json`.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Linked { .. } => "linked",
            Self::Orphan => "orphan",
        }
    }
}

/// One external directory the pass looked at, selected or withheld.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalItem {
    /// How it was reached.
    pub kind: ExternalKind,
    /// The resolved directory, or the link itself when it is dangling.
    pub path: PathBuf,
    /// The declared base it lies under, when it lies under one.
    pub base: Option<PathBuf>,
    /// Why it is withheld; empty exactly when it is selected.
    pub reasons: Vec<String>,
    /// Bytes under the resolved directory, following no symbolic link.
    pub bytes: u64,
    /// The identity observed, re-checked at execution.
    identity: Option<DirIdentity>,
    /// The lock anchors acquired, preserved at execution.
    anchors: BTreeSet<PathBuf>,
}

impl ExternalItem {
    /// Whether the payload may be reclaimed.
    #[must_use]
    pub fn selected(&self) -> bool {
        self.reasons.is_empty()
    }

    fn withheld(kind: ExternalKind, path: PathBuf, base: Option<PathBuf>, why: String) -> Self {
        Self {
            kind,
            path,
            base,
            reasons: vec![why],
            bytes: 0,
            identity: None,
            anchors: BTreeSet::new(),
        }
    }
}

/// What the pass found under one declared base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseState {
    /// The listing was read.
    Scanned,
    /// The base did not exist or canonicalize; not scanned.
    Unreachable(ObservationError),
    /// The listing failed; every child of it is withheld.
    ListingFailed(ObservationError),
}

/// One line of the per-base register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseReport {
    /// The base: canonical when reachable, as declared otherwise.
    pub base: PathBuf,
    /// What happened to it.
    pub state: BaseState,
}

/// Everything the external half of the pass observed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalPlan {
    /// One line per declared base.
    pub bases: Vec<BaseReport>,
    /// Every linked root and base child looked at.
    pub items: Vec<ExternalItem>,
}

/// Device and inode, where the platform has them.
#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((meta.dev(), meta.ino()))
}

/// Device and inode, where the platform has them.
#[cfg(not(unix))]
fn file_id(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// A directory's identity, or `None` when it is not a directory.
fn dir_identity(path: &Path) -> Option<DirIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    meta.is_dir().then(|| DirIdentity {
        path: path.to_path_buf(),
        file_id: file_id(&meta),
    })
}

/// A base child or link target waiting for the gate.
struct Pending {
    item: ExternalItem,
    id: MoleculeId,
    identity: DirIdentity,
}

/// The sentence for a withhold decided by the pure gate.
fn withhold_reason(why: &ExternalWithhold, id: &MoleculeId, dir: &Path, base: &Path) -> String {
    match why {
        ExternalWithhold::EnumerationIncomplete => "the worktree enumeration is incomplete — \
             uniqueness cannot be established, so no external root or orphan is selected"
            .to_owned(),
        ExternalWithhold::BaseListingFailed => format!(
            "the listing of base {} failed — every child of it is withheld",
            base.display()
        ),
        ExternalWithhold::NotUnique => "another link or base child resolves to the same \
             directory — a shared directory is never reclaimed"
            .to_owned(),
        ExternalWithhold::RecordAbsent => format!(
            "no record of molecule {id} in this fleet's store — an external base may be \
             shared with another fleet"
        ),
        ExternalWithhold::RecordUnknown(e) => {
            format!("the molecule record could not be read: {}", e.describe())
        }
        ExternalWithhold::StatusUnknown(e) => {
            format!("the molecule status could not be read: {}", e.describe())
        }
        ExternalWithhold::NotTerminal(s) => format!(
            "molecule {id} is {s} — a molecule that is not Completed or Collapsed may \
             still be resumed"
        ),
        ExternalWithhold::LockHeld => format!(
            "another process holds the build lock of a profile under {} — reclaiming \
             under contention would delete a running build's output",
            dir.display()
        ),
        ExternalWithhold::LockProbeFailed(e) => format!(
            "the build lock could not be probed: {} — no exclusion, no removal",
            e.describe()
        ),
    }
}

/// The sentence for a link that is not the candidate's own directory.
fn refusal_reason(verdict: &LinkVerdict, link: &Path, root: &Path, target: &Path) -> String {
    match verdict {
        LinkVerdict::Accepted { .. } => String::new(),
        LinkVerdict::OutsideBases => format!(
            "the link {} resolves outside every declared base of `{}`, to {}",
            link.display(),
            root.display(),
            target.display()
        ),
        LinkVerdict::IsBase(base) => format!(
            "the link {} resolves to the declared base itself, {} — a shared, persistent target",
            link.display(),
            base.display()
        ),
        LinkVerdict::Deeper(base) => format!(
            "the link {} resolves to {}, below a child of {} — not the declared layout",
            link.display(),
            target.display(),
            base.display()
        ),
        LinkVerdict::NotOwnChild { base, reading } => format!(
            "the link {} resolves to {}, which is not this molecule's directory under {}: {}",
            link.display(),
            target.display(),
            base.display(),
            reading_phrase(reading)
        ),
    }
}

/// How a child name read, in words.
fn reading_phrase(reading: &ChildReading) -> String {
    match reading {
        ChildReading::Molecule { id, .. } => format!("its name reads as molecule {id}"),
        ChildReading::NotAMolecule => {
            "not a molecule directory: no declared suffix of this base leaves a molecule id"
                .to_owned()
        }
        ChildReading::Ambiguous(entries) => format!(
            "the name is ambiguous between entries {}",
            entries
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    }
}

impl GitWorktreeObserver<'_> {
    /// Attach this pass's validated external declarations.
    #[must_use]
    pub fn with_external(mut self, external: ExternalDeclarations) -> Self {
        self.external = external;
        self
    }

    /// Whether `root` has a declared external entry and `path` is a
    /// symbolic link — the case the in-worktree rule leaves to this module.
    pub(super) fn left_to_external(&self, root: &Path, path: &Path) -> bool {
        self.external.declares(root)
            && std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
    }

    /// The record and status of one molecule in this fleet's store.
    fn record_of(
        &self,
        id: &MoleculeId,
        at: &Path,
    ) -> (MoleculeRecordObservation, StatusObservation) {
        match self.store.load_molecule(id) {
            Ok(mol) => (
                MoleculeRecordObservation::Present,
                StatusObservation::Known(mol.status),
            ),
            Err(CosmonError::MoleculeNotFound(_)) => (
                MoleculeRecordObservation::Absent,
                StatusObservation::Unknown(ObservationError::new(
                    "load molecule status",
                    at,
                    "no record",
                )),
            ),
            Err(e) => {
                let err = ObservationError::new("load molecule", at, e.to_string());
                (
                    MoleculeRecordObservation::Unknown(err.clone()),
                    StatusObservation::Unknown(err),
                )
            }
        }
    }

    /// Take, or recognise as already ours, the flock on one anchor.
    fn lock_anchor(&self, anchor: &Path) -> LockObservation {
        let fail = |cause: String| {
            LockObservation::ProbeFailed(ObservationError::new("open lock anchor", anchor, cause))
        };
        match std::fs::symlink_metadata(anchor) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return fail("not a regular file".to_owned()),
            Err(e) => return fail(e.to_string()),
        }
        // No `create(true)`: a missing anchor is never manufactured.
        let file = match File::options().read(true).write(true).open(anchor) {
            Ok(f) => f,
            Err(e) => return fail(e.to_string()),
        };
        let id = file.metadata().ok().as_ref().and_then(file_id);
        if self.holds_lock_on(id) {
            return LockObservation::Acquired;
        }
        match file.try_lock_exclusive() {
            Ok(()) => {
                self.retain_lock(id, file);
                LockObservation::Acquired
            }
            Err(_) => LockObservation::Held,
        }
    }

    /// Every profile lock present under `dir`, `debug/.cargo-lock` always
    /// among them (missing is a failed probe, D5).
    fn anchors_under(dir: &Path) -> Result<BTreeSet<PathBuf>, ObservationError> {
        let mut anchors = BTreeSet::from([dir.join("debug/.cargo-lock")]);
        let is_real_dir =
            |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir());
        let read = |p: &Path| {
            std::fs::read_dir(p)
                .map_err(|e| ObservationError::new("list build profiles", p, e.to_string()))
        };
        for child in read(dir)? {
            let child = child
                .map_err(|e| ObservationError::new("list build profiles", dir, e.to_string()))?
                .path();
            if !is_real_dir(&child) {
                continue;
            }
            if child.join(".cargo-lock").exists() {
                anchors.insert(child.join(".cargo-lock"));
            }
            for grand in read(&child)? {
                let grand = grand
                    .map_err(|e| {
                        ObservationError::new("list build profiles", &child, e.to_string())
                    })?
                    .path();
                if is_real_dir(&grand) && grand.join(".cargo-lock").exists() {
                    anchors.insert(grand.join(".cargo-lock"));
                }
            }
        }
        Ok(anchors)
    }

    /// Acquire every profile lock under `dir`.
    fn lock_dir(&self, dir: &Path) -> (LockObservation, BTreeSet<PathBuf>) {
        let anchors = match Self::anchors_under(dir) {
            Ok(a) => a,
            Err(e) => return (LockObservation::ProbeFailed(e), BTreeSet::new()),
        };
        let locks: Vec<LockObservation> = anchors.iter().map(|a| self.lock_anchor(a)).collect();
        (combine_locks(&locks, dir), anchors)
    }

    /// Observe every external root and orphan (ADR-191 D2–D7).
    ///
    /// `candidates` must be the whole enumerated population and
    /// `enumeration_complete` must say whether it is: uniqueness and the
    /// orphan rule range over it, and an incomplete population selects
    /// nothing external.
    #[must_use]
    pub fn observe_external(
        &self,
        candidates: &[PathBuf],
        enumeration_complete: bool,
    ) -> ExternalPlan {
        let mut plan = ExternalPlan::default();
        if self.external.roots.is_empty() {
            return plan;
        }
        let entries = &self.external.reachable;
        let mut pending: Vec<Pending> = Vec::new();
        // Every directory a live link resolves to, whatever the verdict.
        let mut reached: Vec<DirIdentity> = Vec::new();

        for candidate in candidates {
            let owner = Self::molecule_id(candidate).ok();
            for root in &self.external.roots {
                let link = candidate.join(root);
                let kind = ExternalKind::Linked {
                    worktree: candidate.clone(),
                    link: link.clone(),
                };
                match std::fs::symlink_metadata(&link) {
                    Ok(m) if m.file_type().is_symlink() => {}
                    // A real directory takes the in-worktree rule; absence
                    // contributes no root.
                    Ok(_) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        plan.items.push(ExternalItem::withheld(
                            kind,
                            link.clone(),
                            None,
                            format!("the link {} could not be read: {e}", link.display()),
                        ));
                        continue;
                    }
                }
                let target = match std::fs::canonicalize(&link) {
                    Ok(t) => t,
                    Err(e) => {
                        let raw = std::fs::read_link(&link)
                            .map_or_else(|_| "?".to_owned(), |p| p.display().to_string());
                        plan.items.push(ExternalItem::withheld(
                            kind,
                            link.clone(),
                            None,
                            format!(
                                "the link {} is dangling (it points to {raw}): {e} — no \
                                 root from this worktree",
                                link.display()
                            ),
                        ));
                        continue;
                    }
                };
                let identity = dir_identity(&target);
                if let Some(id) = &identity {
                    reached.push(id.clone());
                }
                let verdict = classify_link(owner.as_ref(), root, &target, entries);
                match (verdict, identity, owner.clone()) {
                    (LinkVerdict::Accepted { base, .. }, Some(identity), Some(id)) => {
                        pending.push(Pending {
                            item: ExternalItem {
                                kind,
                                path: target.clone(),
                                base: Some(base),
                                reasons: Vec::new(),
                                bytes: approximate_size_bytes(&target),
                                identity: None,
                                anchors: BTreeSet::new(),
                            },
                            id,
                            identity,
                        });
                    }
                    (LinkVerdict::Accepted { base, .. }, _, _) => {
                        plan.items.push(ExternalItem::withheld(
                            kind,
                            target.clone(),
                            Some(base),
                            format!(
                                "the link {} resolves to {}, which is not a directory",
                                link.display(),
                                target.display()
                            ),
                        ));
                    }
                    (verdict, _, _) => {
                        let base = match &verdict {
                            LinkVerdict::IsBase(b)
                            | LinkVerdict::Deeper(b)
                            | LinkVerdict::NotOwnChild { base: b, .. } => Some(b.clone()),
                            _ => None,
                        };
                        let why = refusal_reason(&verdict, &link, root, &target);
                        let mut item = ExternalItem::withheld(kind, target.clone(), base, why);
                        item.bytes = approximate_size_bytes(&target);
                        plan.items.push(item);
                    }
                }
            }
        }

        for unreachable in &self.external.unreachable {
            plan.bases.push(BaseReport {
                base: unreachable.base.clone(),
                state: BaseState::Unreachable(unreachable.error.clone()),
            });
        }
        let mut listed_ok: Vec<(PathBuf, bool)> = Vec::new();
        for base in self.external.bases() {
            let ok = self.scan_base(&base, &reached, &mut plan, &mut pending);
            listed_ok.push((base, ok));
        }

        let identities: Vec<DirIdentity> = pending.iter().map(|p| p.identity.clone()).collect();
        let collisions = colliding(&identities);
        for (n, mut p) in pending.into_iter().enumerate() {
            let base = p.item.base.clone().unwrap_or_default();
            let (molecule, status) = self.record_of(&p.id, &p.item.path);
            let mut obs = ExternalObservation {
                molecule,
                status,
                // Probed only once every other condition holds, so a
                // withheld directory's build is never blocked by this pass.
                lock: LockObservation::Acquired,
                unique: !collisions.contains(&n),
                enumeration_complete,
                base_listed: listed_ok.iter().any(|(b, ok)| *b == base && *ok),
            };
            if external_selection(&obs).is_ok() {
                let (lock, anchors) = self.lock_dir(&p.item.path);
                obs.lock = lock;
                p.item.anchors = anchors;
            }
            if let Err(why) = external_selection(&obs) {
                p.item
                    .reasons
                    .push(withhold_reason(&why, &p.id, &p.item.path, &base));
                p.item.anchors.clear();
            } else {
                p.item.identity = Some(p.identity);
            }
            plan.items.push(p.item);
        }
        plan
    }

    /// List one base's direct children into orphan candidates or withheld
    /// items. Returns whether the listing was read without error.
    fn scan_base(
        &self,
        base: &Path,
        reached: &[DirIdentity],
        plan: &mut ExternalPlan,
        pending: &mut Vec<Pending>,
    ) -> bool {
        let entries = &self.external.reachable;
        let suffixes = suffixes_of(base, entries);
        let listing = match std::fs::read_dir(base) {
            Ok(l) => l,
            Err(e) => {
                plan.bases.push(BaseReport {
                    base: base.to_path_buf(),
                    state: BaseState::ListingFailed(ObservationError::new(
                        "list external base",
                        base,
                        e.to_string(),
                    )),
                });
                return false;
            }
        };
        let mut ok = true;
        for child in listing {
            let child = match child {
                Ok(c) => c,
                Err(e) => {
                    ok = false;
                    plan.bases.push(BaseReport {
                        base: base.to_path_buf(),
                        state: BaseState::ListingFailed(ObservationError::new(
                            "list external base",
                            base,
                            e.to_string(),
                        )),
                    });
                    break;
                }
            };
            let path = child.path();
            let withheld = |why: String| {
                ExternalItem::withheld(
                    ExternalKind::Orphan,
                    path.clone(),
                    Some(base.to_path_buf()),
                    why,
                )
            };
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    plan.items.push(withheld(format!("could not be read: {e}")));
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                plan.items.push(withheld(
                    "is a symbolic link — a base child is reclaimed only as a real directory"
                        .to_owned(),
                ));
                continue;
            }
            if !meta.is_dir() {
                plan.items.push(withheld("is not a directory".to_owned()));
                continue;
            }
            let identity = DirIdentity {
                path: path.clone(),
                file_id: file_id(&meta),
            };
            if reached.iter().any(|r| r.same_as(&identity)) {
                // Reported through the link that reaches it.
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            match classify_base_child(&name, &suffixes) {
                ChildReading::Molecule { id, .. } => {
                    let mut item = withheld(String::new());
                    item.reasons.clear();
                    item.bytes = approximate_size_bytes(&path);
                    pending.push(Pending { item, id, identity });
                }
                reading => {
                    let mut item = withheld(reading_phrase(&reading));
                    item.bytes = approximate_size_bytes(&path);
                    plan.items.push(item);
                }
            }
        }
        if ok {
            plan.bases.push(BaseReport {
                base: base.to_path_buf(),
                state: BaseState::Scanned,
            });
        }
        ok
    }

    /// Re-check that `item` still names the directory observed, under the
    /// locks this observer holds.
    fn revalidate(&self, item: &ExternalItem) -> Result<(), ObservationError> {
        let observed = item.identity.as_ref().ok_or_else(|| {
            ObservationError::new("revalidate external root", &item.path, "never selected")
        })?;
        let now = match &item.kind {
            ExternalKind::Linked { link, .. } => {
                let meta = std::fs::symlink_metadata(link)
                    .map_err(|e| ObservationError::new("re-read link", link, e.to_string()))?;
                if !meta.file_type().is_symlink() {
                    return Err(ObservationError::new(
                        "re-read link",
                        link,
                        "is no longer a symbolic link",
                    ));
                }
                std::fs::canonicalize(link)
                    .map_err(|e| ObservationError::new("re-resolve link", link, e.to_string()))?
            }
            ExternalKind::Orphan => {
                let meta = std::fs::symlink_metadata(&item.path).map_err(|e| {
                    ObservationError::new("re-read base child", &item.path, e.to_string())
                })?;
                if !meta.file_type().is_dir() {
                    return Err(ObservationError::new(
                        "re-read base child",
                        &item.path,
                        "is no longer a real directory",
                    ));
                }
                item.path.clone()
            }
        };
        let current = dir_identity(&now);
        if now != observed.path || current.as_ref() != Some(observed) {
            return Err(ObservationError::new(
                "re-resolve external root",
                &item.path,
                format!(
                    "now resolves to {}, not the directory observed — refusing",
                    now.display()
                ),
            ));
        }
        for anchor in &item.anchors {
            let id = std::fs::metadata(anchor).ok().as_ref().and_then(file_id);
            if !self.holds_lock_on(id) {
                return Err(ObservationError::new(
                    "revalidate lock guard",
                    anchor,
                    "the acquired flock guard is no longer held by this observer",
                ));
            }
        }
        Ok(())
    }

    /// Reclaim one selected external directory's payload (D3).
    ///
    /// The link, every lock anchor and its ancestors, the base and anything
    /// outside the directory stay. Nothing happens unless the link (or the
    /// orphan) still resolves to the directory observed, by path, device
    /// and inode, and every lock is still held.
    ///
    /// # Errors
    /// The failed re-check, or a directory that cannot be walked.
    pub fn reclaim_external(&self, item: &ExternalItem) -> Result<ReclaimReport, ObservationError> {
        let mut report = ReclaimReport::default();
        if !item.selected() {
            return Ok(report);
        }
        self.revalidate(item)?;
        Self::reclaim_tree(&item.path, &item.anchors, &mut report)?;
        Ok(report)
    }

    /// Whether this observer holds a flock on the file with this identity.
    pub(super) fn holds_lock_on(&self, id: Option<(u64, u64)>) -> bool {
        id.is_some_and(|id| {
            self.held_ids.lock().is_ok_and(
                |held: std::sync::MutexGuard<'_, HashSet<(u64, u64)>>| held.contains(&id),
            )
        })
    }

    /// Keep an acquired lock's file open for the observer's lifetime.
    pub(super) fn retain_lock(&self, id: Option<(u64, u64)>, file: File) {
        if let (Some(id), Ok(mut held)) = (id, self.held_ids.lock()) {
            held.insert(id);
        }
        if let Ok(mut files) = self.external_guards.lock() {
            files.push(file);
        }
    }
}

/// The identity of an open lock file, for [`GitWorktreeObserver::lock`].
pub(super) fn open_file_id(file: &File) -> Option<(u64, u64)> {
    file.metadata().ok().as_ref().and_then(file_id)
}
