// SPDX-License-Identifier: AGPL-3.0-only

//! Worktree reclaim — two named predicates over three-valued observations.
//!
//! This module is the I/O-free half of the contract in
//! `docs/design/worktree-reclaim/CONTRACT.md` (issue 61). It exists because
//! the single "is this worktree reclaimable?" question was contradictory:
//! derived build output and durable bytes have different safety conditions,
//! and one boolean cannot carry both. The contract splits the question in
//! two and this module is that split, expressed in types:
//!
//! * [`derived_selection`] answers *may the rebuildable payload go?* It reads
//!   molecule presence, molecule status and the exclusion lock — and nothing
//!   else. Ancestry, dirt, registration and ignored content are not fields of
//!   [`DerivedObservation`], so "derived selection is ancestry-blind" is a
//!   fact the compiler enforces rather than a sentence in a doc comment.
//! * [`durable_eligibility`] answers *is this directory's durable content
//!   reachable from somewhere else?* It is **advisory**: `Eligible`
//!   authorizes no removal at all. The lock is deliberately not one of its
//!   inputs, because eligibility is not execution authorization.
//!
//! Every observation is three-valued: positive evidence, negative evidence,
//! or [`ObservationError`]. `Unknown` is never `None`, zero, an empty list or
//! `false` — the 2026-08-02 incident is exactly what "I could not check"
//! reading the same as "there is nothing there" costs. An `Unknown` on an
//! axis a predicate does not read cannot infect that predicate; an `Unknown`
//! on an axis it does read always withholds, carrying its diagnostic.
//!
//! The adapter that fills these values (Git, the filesystem, `flock`) lives
//! outside the core, behind [`WorktreeObservationPort`] (ADR-082).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::molecule::MoleculeStatus;

// ---------------------------------------------------------------------------
// Failed observations
// ---------------------------------------------------------------------------

/// Why one observation could not be made.
///
/// Carried by every `Unknown` so a withhold can name itself. An operator can
/// act on "`git status` in `/w/x` exited 128: not a git repository"; they
/// cannot act on a missing directory that silently read as clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationError {
    /// The operation attempted, e.g. `git status --porcelain`.
    pub operation: String,
    /// The candidate path the operation was attempted against.
    pub path: PathBuf,
    /// Exit status, spawn failure, decoding or parse failure — verbatim.
    pub cause: String,
}

impl ObservationError {
    /// Build a failed observation from its three mandatory parts.
    ///
    /// A constructor rather than literal struct expressions at twenty call
    /// sites, so no adapter can omit the path and leave the operator with an
    /// unattributable diagnostic.
    pub fn new(
        operation: impl Into<String>,
        path: impl Into<PathBuf>,
        cause: impl Into<String>,
    ) -> Self {
        Self {
            operation: operation.into(),
            path: path.into(),
            cause: cause.into(),
        }
    }

    /// One line naming operation, path and cause, for an operator-facing alert.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "`{}` in {} failed: {}",
            self.operation,
            self.path.display(),
            self.cause
        )
    }
}

// ---------------------------------------------------------------------------
// The seven observation axes
// ---------------------------------------------------------------------------

/// Whether a molecule record was found for the candidate (axis `M`).
///
/// Separate from [`StatusObservation`] because proven absence and a failed
/// lookup are opposite answers: absence makes a stale status irrelevant,
/// while a failed lookup withholds everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoleculeRecordObservation {
    /// A record exists for this candidate.
    Present,
    /// The record is proven absent — no molecule owns this directory.
    Absent,
    /// The lookup itself failed.
    Unknown(ObservationError),
}

/// The candidate molecule's lifecycle status (axis `S`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusObservation {
    /// A status was read.
    Known(MoleculeStatus),
    /// The status could not be read (unreadable, undecodable, unparseable).
    Unknown(ObservationError),
}

/// Whether Git considers the candidate a registered worktree (axis `R`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationObservation {
    /// Positive membership in `git worktree list --porcelain`.
    Registered,
    /// Proven nonmembership — scratch, not a worktree.
    Unregistered,
    /// The enumeration failed.
    Unknown(ObservationError),
}

/// The exclusion lock on `<wt>/target/debug/.cargo-lock` (axis `L`).
///
/// Three values, not two. Two states were the previous contract's
/// contradiction: "not held" and "could not tell" had to be the same value,
/// and then no lock observation could both withhold on failure and permit
/// reclamation anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockObservation {
    /// Another holder prevented a nonblocking exclusive `flock`.
    Held,
    /// The observer **acquired** the `flock` and retains the guard for the
    /// whole reclamation. Mere absence of contention is not this value.
    Acquired,
    /// Missing lock file, inaccessible path, unsupported lock, or any other
    /// failure. Keeps derived content.
    ProbeFailed(ObservationError),
}

/// Commits on the candidate's branch not reachable from its base (axis `A`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AheadObservation {
    /// Zero commits in `base..HEAD`, with **both** refs verified.
    Zero,
    /// At least one unreachable commit.
    Positive(usize),
    /// Missing branch or base, parent-repository fallback, mismatched Git top
    /// level, or a failed count. Never collapses to [`AheadObservation::Zero`].
    Unknown(ObservationError),
}

/// `git status --porcelain` in the candidate (axis `D`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirtyObservation {
    /// A successful, empty status.
    Clean,
    /// Tracked modifications and/or nonignored untracked paths.
    Dirty(Vec<String>),
    /// Spawn, exit, decoding or parse failure. This is the value that
    /// replaces both consumers' historical fail-open empty list.
    Unknown(ObservationError),
}

/// Durable content that Git ignores but an operator would still lose (axis `I`).
///
/// An ignored note is durable unless it lies in the validated derived set:
/// `target/` is ignored and rebuildable, `operator-note.txt` is ignored and
/// the only copy of something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoredDurableObservation {
    /// A complete inventory found nothing outside the derived set.
    Absent,
    /// Ignored durable paths are present.
    Present(Vec<String>),
    /// The inventory failed or was incomplete.
    Unknown(ObservationError),
}

/// The validated derived roots under a candidate.
///
/// Not an axis of the truth table: the table assumes a validated, present
/// set. It is a field so that a classification failure supplies *no* root
/// with a reason instead of silently widening or narrowing the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivedRoots {
    /// Roots that passed [`validate_derived_root`] and were found present.
    Validated(BTreeSet<PathBuf>),
    /// Classification or inventory failed; nothing may be selected.
    Unavailable(ObservationError),
}

// ---------------------------------------------------------------------------
// The consideration gate (`G`)
// ---------------------------------------------------------------------------

/// Permission to consider either predicate at all (the `G` normalization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consideration {
    /// Neither predicate may select anything.
    No,
    /// Each predicate may apply its own rules.
    Yes,
}

/// Normalize molecule presence and status into [`Consideration`].
///
/// Molecule status is a **veto, never proof of reachability**. A proven-absent
/// record ignores status entirely — a stale status file cannot become
/// authority over bytes nobody claims. A present nonterminal molecule
/// (including `Frozen` and `Starved`, which are resumable) vetoes. A failed
/// lookup vetoes.
#[must_use]
pub fn consideration_gate(
    molecule: &MoleculeRecordObservation,
    status: &StatusObservation,
) -> Consideration {
    match molecule {
        MoleculeRecordObservation::Absent => Consideration::Yes,
        MoleculeRecordObservation::Unknown(_) => Consideration::No,
        MoleculeRecordObservation::Present => match status {
            StatusObservation::Known(MoleculeStatus::Completed | MoleculeStatus::Collapsed) => {
                Consideration::Yes
            }
            _ => Consideration::No,
        },
    }
}

// ---------------------------------------------------------------------------
// Predicate 1 — derived selection
// ---------------------------------------------------------------------------

/// The inputs of [`derived_selection`], and deliberately nothing more.
///
/// Registration, ancestry, dirt and ignored durable content are absent by
/// construction: a rebuildable payload's safety does not depend on whether
/// the source tree is dirty, and a type that cannot name those axes cannot
/// grow a hidden dependency on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedObservation {
    /// The candidate worktree directory.
    pub path: PathBuf,
    /// Axis `M`.
    pub molecule: MoleculeRecordObservation,
    /// Axis `S`.
    pub status: StatusObservation,
    /// Axis `L`.
    pub lock: LockObservation,
    /// The validated derived roots under [`path`](Self::path).
    pub derived_set: DerivedRoots,
}

/// What [`derived_selection`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedSelection {
    /// Select nothing.
    Keep,
    /// The validated derived roots may have their payload reclaimed, under a
    /// lock that is still held.
    ReclaimDerived,
}

/// Decide whether a candidate's derived payload may be reclaimed.
///
/// `ReclaimDerived` requires both a permitting [`Consideration`] and an
/// `Acquired` lock. `Held` is contention; `ProbeFailed` is ignorance; both
/// keep the content.
#[must_use]
pub fn derived_selection(obs: &DerivedObservation) -> DerivedSelection {
    match (consideration_gate(&obs.molecule, &obs.status), &obs.lock) {
        (Consideration::Yes, LockObservation::Acquired) => DerivedSelection::ReclaimDerived,
        _ => DerivedSelection::Keep,
    }
}

/// The exact path set [`derived_selection`] selects.
///
/// Either empty or the validated derived roots — never a byte count, never a
/// whole worktree. Unavailable roots select nothing.
#[must_use]
pub fn selected_derived_paths(obs: &DerivedObservation) -> BTreeSet<PathBuf> {
    match (derived_selection(obs), &obs.derived_set) {
        (DerivedSelection::ReclaimDerived, DerivedRoots::Validated(roots)) => roots.clone(),
        _ => BTreeSet::new(),
    }
}

// ---------------------------------------------------------------------------
// Predicate 2 — durable eligibility
// ---------------------------------------------------------------------------

/// The inputs of [`durable_eligibility`], and deliberately nothing more.
///
/// The lock is absent by construction: eligibility is advisory and grants no
/// execution authority, so an acquired lock would only look like permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableObservation {
    /// The candidate worktree directory.
    pub path: PathBuf,
    /// Axis `M`.
    pub molecule: MoleculeRecordObservation,
    /// Axis `S`.
    pub status: StatusObservation,
    /// Axis `R`.
    pub registration: RegistrationObservation,
    /// Axis `A`.
    pub ahead: AheadObservation,
    /// Axis `D`.
    pub dirty: DirtyObservation,
    /// Axis `I`.
    pub ignored_durable: IgnoredDurableObservation,
}

/// What [`durable_eligibility`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableEligibility {
    /// Keep the durable content.
    Withhold,
    /// Every byte is reachable from somewhere else. **Advisory only**: this
    /// authorizes no automatic removal, ever.
    Eligible,
}

/// Decide whether a candidate's durable content is reachable elsewhere.
///
/// Five conjuncts, each of which withholds on `Unknown`: permission,
/// positive registration, proven `Zero` ancestry, a clean status and an empty
/// ignored-durable inventory. Unregistered scratch cannot pass — Git walking
/// up into the parent repository is not evidence about a scratch directory.
#[must_use]
pub fn durable_eligibility(obs: &DurableObservation) -> DurableEligibility {
    if consideration_gate(&obs.molecule, &obs.status) == Consideration::No {
        return DurableEligibility::Withhold;
    }
    if obs.registration != RegistrationObservation::Registered {
        return DurableEligibility::Withhold;
    }
    if obs.ahead != AheadObservation::Zero {
        return DurableEligibility::Withhold;
    }
    if obs.dirty != DirtyObservation::Clean {
        return DurableEligibility::Withhold;
    }
    if obs.ignored_durable != IgnoredDurableObservation::Absent {
        return DurableEligibility::Withhold;
    }
    DurableEligibility::Eligible
}

/// The exact advisory path set [`durable_eligibility`] yields.
///
/// `{path}` or `{}`. It is an advisory reachability result and never a
/// removal plan: no caller in this workspace may turn it into one without an
/// operator gesture.
#[must_use]
pub fn advisory_durable_paths(obs: &DurableObservation) -> BTreeSet<PathBuf> {
    match durable_eligibility(obs) {
        DurableEligibility::Eligible => BTreeSet::from([obs.path.clone()]),
        DurableEligibility::Withhold => BTreeSet::new(),
    }
}

// ---------------------------------------------------------------------------
// Batch
// ---------------------------------------------------------------------------

/// One candidate's complete observation, as the port produces it.
///
/// The two predicate inputs are separate structs rather than one flat record
/// so that neither can read the other's axes; this type merely carries both
/// for a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateObservation {
    /// Input to [`derived_selection`].
    pub derived: DerivedObservation,
    /// Input to [`durable_eligibility`].
    pub durable: DurableObservation,
}

/// The result of planning over a batch of candidates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReclamationPlan {
    /// Union of the selected derived roots. The only set anything may act on.
    pub derived: BTreeSet<PathBuf>,
    /// Candidates whose durable content is advisorily reachable elsewhere.
    /// **Not** a removal set.
    pub durable_advisory: BTreeSet<PathBuf>,
}

/// Apply both predicates across a batch.
///
/// The batch falsifier of the contract: `derived` is exactly the union of
/// `E(w)` over selecting candidates and `durable_advisory` exactly the
/// selecting candidates' own paths.
#[must_use]
pub fn plan(candidates: &[CandidateObservation]) -> ReclamationPlan {
    let mut out = ReclamationPlan::default();
    for candidate in candidates {
        out.derived
            .extend(selected_derived_paths(&candidate.derived));
        out.durable_advisory
            .extend(advisory_durable_paths(&candidate.durable));
    }
    out
}

// ---------------------------------------------------------------------------
// Derived-root validation
// ---------------------------------------------------------------------------

/// Why a configured derived root was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedRootRejection {
    /// Absolute path, or empty.
    NotWorktreeLocal,
    /// Contains `.` or `..`, so it does not name a fixed subtree.
    Traversal,
    /// Names the worktree root itself — which is durable, not derived.
    WorktreeRoot,
    /// Reaches into Git metadata or another durable, non-rebuildable area.
    DurableArea,
}

/// Directory names that are durable by construction and can never be a
/// derived root, whatever a configuration says.
const DURABLE_AREAS: [&str; 3] = [".git", ".cosmon", ".worktrees"];

/// Validate one configured derived root as a rebuildable, worktree-local
/// relative path.
///
/// Lexical validation only. Symlink escape cannot be decided without the
/// filesystem, so it is the adapter's documented precondition: an adapter
/// must resolve each root and confirm containment before observing it as
/// [`DerivedRoots::Validated`].
pub fn validate_derived_root(root: &Path) -> Result<PathBuf, DerivedRootRejection> {
    use std::path::Component;
    if root.is_absolute() {
        return Err(DerivedRootRejection::NotWorktreeLocal);
    }
    let mut components = 0_usize;
    for component in root.components() {
        match component {
            Component::Normal(name) => {
                if DURABLE_AREAS
                    .iter()
                    .any(|d| name == std::ffi::OsStr::new(d))
                {
                    return Err(DerivedRootRejection::DurableArea);
                }
                components += 1;
            }
            Component::CurDir | Component::ParentDir => {
                return Err(DerivedRootRejection::Traversal)
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(DerivedRootRejection::NotWorktreeLocal)
            }
        }
    }
    if components == 0 {
        return Err(DerivedRootRejection::WorktreeRoot);
    }
    Ok(root.to_path_buf())
}

// ---------------------------------------------------------------------------
// The migration seam shared by both existing consumers
// ---------------------------------------------------------------------------

/// What a consumer holding a [`DirtyObservation`] may do with a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeRemovalDecision {
    /// Clean, or dirty with an explicit operator override.
    Remove,
    /// Dirty without an override; the listed paths are what is at stake.
    RefuseDirty(Vec<String>),
    /// The probe failed. Preserve the worktree for retry and report the
    /// failed operation. A generic force flag does **not** reach this arm:
    /// force overrides a *known* dirty tree, never an unknown one.
    Withhold(ObservationError),
}

/// The one place the "may these bytes go?" question is answered for both
/// `cs purge`'s sweep and the harvest transaction's teardown.
///
/// Before this, each consumer had its own fail-open: `purge` returned an
/// empty dirty list on a failed `git status`, and the transaction warned and
/// removed the worktree anyway. Two copies of a safety question drift; this
/// is one, and its `Unknown` arm keeps the bytes.
#[must_use]
pub fn worktree_removal_decision(dirty: &DirtyObservation, force: bool) -> WorktreeRemovalDecision {
    match dirty {
        DirtyObservation::Unknown(err) => WorktreeRemovalDecision::Withhold(err.clone()),
        DirtyObservation::Dirty(paths) if !force => {
            WorktreeRemovalDecision::RefuseDirty(paths.clone())
        }
        DirtyObservation::Dirty(_) | DirtyObservation::Clean => WorktreeRemovalDecision::Remove,
    }
}

// ---------------------------------------------------------------------------
// The observation port
// ---------------------------------------------------------------------------

/// Supplies domain values for one candidate directory.
///
/// Declared here and implemented outside the core (ADR-082): the predicates
/// above run no Git, filesystem, process, lock, network or clock operation,
/// and a test injects a fake rather than building a repository with a
/// diverged branch for each of the truth table's rows.
///
/// An implementation that answers [`LockObservation::Acquired`] **must hold
/// the guard** for as long as the caller may act on the answer. A sampled
/// lock that was released is not `Acquired`.
pub trait WorktreeObservationPort {
    /// Axis `M`.
    fn molecule_record(&self, candidate: &Path) -> MoleculeRecordObservation;
    /// Axis `S`.
    fn molecule_status(&self, candidate: &Path) -> StatusObservation;
    /// Axis `R`.
    fn registration(&self, candidate: &Path) -> RegistrationObservation;
    /// Axis `L`. Acquires and retains the exclusion guard.
    fn lock(&self, candidate: &Path) -> LockObservation;
    /// Axis `A`.
    fn commits_ahead(&self, candidate: &Path) -> AheadObservation;
    /// Axis `D`.
    fn dirty(&self, candidate: &Path) -> DirtyObservation;
    /// Axis `I`.
    fn ignored_durable(&self, candidate: &Path) -> IgnoredDurableObservation;
    /// The validated derived roots present under the candidate.
    fn derived_roots(&self, candidate: &Path) -> DerivedRoots;

    /// Compose one complete [`CandidateObservation`].
    ///
    /// Provided so every caller observes the same axes in the same order and
    /// none can quietly skip one and default it to a permissive value.
    fn observe(&self, candidate: &Path) -> CandidateObservation {
        let molecule = self.molecule_record(candidate);
        let status = self.molecule_status(candidate);
        CandidateObservation {
            derived: DerivedObservation {
                path: candidate.to_path_buf(),
                molecule: molecule.clone(),
                status: status.clone(),
                lock: self.lock(candidate),
                derived_set: self.derived_roots(candidate),
            },
            durable: DurableObservation {
                path: candidate.to_path_buf(),
                molecule,
                status,
                registration: self.registration(candidate),
                ahead: self.commits_ahead(candidate),
                dirty: self.dirty(candidate),
                ignored_durable: self.ignored_durable(candidate),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — the contract's truth table, row by row
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture path per row, so a batch equality catches an omitted or an
    /// extra root by identity rather than by counting.
    fn fixture(n: usize) -> PathBuf {
        PathBuf::from(format!("fixture/{n}"))
    }

    /// `E(w)` — two roots, so a `target`-only implementation cannot pass.
    fn derived_set(w: &Path) -> BTreeSet<PathBuf> {
        BTreeSet::from([w.join("target"), w.join("cache")])
    }

    fn err(op: &str) -> ObservationError {
        ObservationError::new(op, "/fixture", "exit 128")
    }

    /// The eight `S` values of the contract, `Unknown` included.
    fn statuses() -> Vec<StatusObservation> {
        let mut v: Vec<_> = MoleculeStatus::ALL
            .iter()
            .map(|s| StatusObservation::Known(*s))
            .collect();
        v.push(StatusObservation::Unknown(err("read status")));
        v
    }

    fn records() -> Vec<MoleculeRecordObservation> {
        vec![
            MoleculeRecordObservation::Present,
            MoleculeRecordObservation::Absent,
            MoleculeRecordObservation::Unknown(err("load molecule")),
        ]
    }

    fn registrations() -> Vec<RegistrationObservation> {
        vec![
            RegistrationObservation::Registered,
            RegistrationObservation::Unregistered,
            RegistrationObservation::Unknown(err("git worktree list")),
        ]
    }

    fn locks() -> Vec<LockObservation> {
        vec![
            LockObservation::Held,
            LockObservation::Acquired,
            LockObservation::ProbeFailed(err("flock")),
        ]
    }

    fn aheads() -> Vec<AheadObservation> {
        vec![
            AheadObservation::Zero,
            AheadObservation::Positive(2),
            AheadObservation::Unknown(err("git rev-list")),
        ]
    }

    fn dirts() -> Vec<DirtyObservation> {
        vec![
            DirtyObservation::Clean,
            DirtyObservation::Dirty(vec!["note.md".to_owned()]),
            DirtyObservation::Unknown(err("git status")),
        ]
    }

    fn ignoreds() -> Vec<IgnoredDurableObservation> {
        vec![
            IgnoredDurableObservation::Absent,
            IgnoredDurableObservation::Present(vec!["operator-note.txt".to_owned()]),
            IgnoredDurableObservation::Unknown(err("git ls-files --ignored")),
        ]
    }

    /// One expanded row of the seven-axis product.
    #[derive(Clone)]
    struct Row {
        s: StatusObservation,
        r: RegistrationObservation,
        l: LockObservation,
        a: AheadObservation,
        d: DirtyObservation,
        i: IgnoredDurableObservation,
        m: MoleculeRecordObservation,
    }

    impl Row {
        fn observation(&self, w: &Path) -> CandidateObservation {
            CandidateObservation {
                derived: DerivedObservation {
                    path: w.to_path_buf(),
                    molecule: self.m.clone(),
                    status: self.s.clone(),
                    lock: self.l.clone(),
                    derived_set: DerivedRoots::Validated(derived_set(w)),
                },
                durable: DurableObservation {
                    path: w.to_path_buf(),
                    molecule: self.m.clone(),
                    status: self.s.clone(),
                    registration: self.r.clone(),
                    ahead: self.a.clone(),
                    dirty: self.d.clone(),
                    ignored_durable: self.i.clone(),
                },
            }
        }

        /// The `G` column of the contract's normalization table, written out
        /// from the document rather than read back from the implementation.
        fn gate(&self) -> &'static str {
            match (&self.m, &self.s) {
                (MoleculeRecordObservation::Absent, _) => "Yes",
                (MoleculeRecordObservation::Unknown(_), _) => "No",
                (
                    MoleculeRecordObservation::Present,
                    StatusObservation::Known(MoleculeStatus::Completed | MoleculeStatus::Collapsed),
                ) => "Yes",
                (MoleculeRecordObservation::Present, _) => "No",
            }
        }

        /// The derived row class, `d0`–`d3`.
        fn derived_class(&self) -> &'static str {
            match (self.gate(), &self.l) {
                ("No", _) => "d0",
                (_, LockObservation::Held) => "d1",
                (_, LockObservation::ProbeFailed(_)) => "d2",
                (_, LockObservation::Acquired) => "d3",
            }
        }

        /// The durable row class, `h0`–`h5`.
        fn durable_class(&self) -> &'static str {
            if self.gate() == "No" {
                return "h0";
            }
            if self.r != RegistrationObservation::Registered {
                return "h1";
            }
            if self.a != AheadObservation::Zero {
                return "h2";
            }
            if self.d != DirtyObservation::Clean {
                return "h3";
            }
            if self.i != IgnoredDurableObservation::Absent {
                return "h4";
            }
            "h5"
        }
    }

    /// Every row of the seven-axis product, in the contract's order.
    fn product() -> Vec<Row> {
        let mut rows = Vec::new();
        for s in statuses() {
            for r in registrations() {
                for l in locks() {
                    for a in aheads() {
                        for d in dirts() {
                            for i in ignoreds() {
                                for m in records() {
                                    rows.push(Row {
                                        s: s.clone(),
                                        r: r.clone(),
                                        l: l.clone(),
                                        a: a.clone(),
                                        d: d.clone(),
                                        i: i.clone(),
                                        m: m.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        rows
    }

    /// The complete truth table: every one of the 5,832 rows, asserted as
    /// **exact path sets**, plus the batch falsifiers over the whole product.
    #[test]
    fn truth_table_every_row_exact_sets() {
        let rows = product();
        assert_eq!(rows.len(), 5832, "seven-axis product");
        let mut seen_derived = BTreeSet::new();
        let mut seen_durable = BTreeSet::new();
        let mut batch = Vec::new();
        let mut expected_derived = BTreeSet::new();
        let mut expected_durable = BTreeSet::new();
        for (n, row) in rows.iter().enumerate() {
            let w = fixture(n);
            let obs = row.observation(&w);
            let derived = selected_derived_paths(&obs.derived);
            let durable = advisory_durable_paths(&obs.durable);
            let expect_derived = if row.derived_class() == "d3" {
                derived_set(&w)
            } else {
                BTreeSet::new()
            };
            let expect_durable = if row.durable_class() == "h5" {
                BTreeSet::from([w.clone()])
            } else {
                BTreeSet::new()
            };
            assert_eq!(
                derived,
                expect_derived,
                "row {n} class {}",
                row.derived_class()
            );
            assert_eq!(
                durable,
                expect_durable,
                "row {n} class {}",
                row.durable_class()
            );
            seen_derived.insert(row.derived_class());
            seen_durable.insert(row.durable_class());
            expected_derived.extend(expect_derived);
            expected_durable.extend(expect_durable);
            batch.push(obs);
        }
        assert_eq!(seen_derived, BTreeSet::from(["d0", "d1", "d2", "d3"]));
        assert_eq!(
            seen_durable,
            BTreeSet::from(["h0", "h1", "h2", "h3", "h4", "h5"])
        );
        // Batch falsifier: exactly the union of E(w) over d3, and exactly the
        // h5 candidates' own paths. No byte or directory counts anywhere.
        let plan = plan(&batch);
        assert_eq!(plan.derived, expected_derived);
        assert_eq!(plan.durable_advisory, expected_durable);
        // An automatic whole-worktree removal selection is always empty: no
        // candidate's own path is ever in the derived set.
        for (n, _) in rows.iter().enumerate() {
            assert!(!plan.derived.contains(&fixture(n)));
        }
    }

    /// A row of the product built from a permitting, fully reachable baseline.
    fn baseline() -> Row {
        Row {
            s: StatusObservation::Known(MoleculeStatus::Collapsed),
            r: RegistrationObservation::Registered,
            l: LockObservation::Acquired,
            a: AheadObservation::Zero,
            d: DirtyObservation::Clean,
            i: IgnoredDurableObservation::Absent,
            m: MoleculeRecordObservation::Present,
        }
    }

    fn sets(row: &Row) -> (BTreeSet<PathBuf>, BTreeSet<PathBuf>) {
        let w = PathBuf::from("w");
        let obs = row.observation(&w);
        (
            selected_derived_paths(&obs.derived),
            advisory_durable_paths(&obs.durable),
        )
    }

    /// Class `d0` / `h0`: a nonterminal molecule vetoes both predicates.
    #[test]
    fn class_d0_h0_nonterminal_status_vetoes_both() {
        for status in [
            MoleculeStatus::Pending,
            MoleculeStatus::Queued,
            MoleculeStatus::Running,
            MoleculeStatus::Frozen,
            MoleculeStatus::Starved,
        ] {
            let row = Row {
                s: StatusObservation::Known(status),
                ..baseline()
            };
            assert_eq!(sets(&row), (BTreeSet::new(), BTreeSet::new()), "{status:?}");
        }
        let unknown_status = Row {
            s: StatusObservation::Unknown(err("read status")),
            ..baseline()
        };
        assert_eq!(sets(&unknown_status), (BTreeSet::new(), BTreeSet::new()));
        let unknown_record = Row {
            m: MoleculeRecordObservation::Unknown(err("load molecule")),
            ..baseline()
        };
        assert_eq!(sets(&unknown_record), (BTreeSet::new(), BTreeSet::new()));
    }

    /// Class `d1`: contention keeps derived content.
    #[test]
    fn class_d1_held_lock_keeps_derived() {
        let row = Row {
            l: LockObservation::Held,
            ..baseline()
        };
        assert_eq!(sets(&row).0, BTreeSet::new());
    }

    /// Class `d2`: a failed lock probe — including a missing lock file — is
    /// never `Acquired` and keeps derived content.
    #[test]
    fn class_d2_probe_failure_keeps_derived() {
        let row = Row {
            l: LockObservation::ProbeFailed(ObservationError::new(
                "flock target/debug/.cargo-lock",
                "w",
                "No such file or directory",
            )),
            ..baseline()
        };
        assert_eq!(sets(&row).0, BTreeSet::new());
    }

    /// Class `d3`: a permitted, acquired candidate selects exactly `E(w)`.
    #[test]
    fn class_d3_acquired_selects_exactly_the_validated_roots() {
        let (derived, _) = sets(&baseline());
        assert_eq!(derived, derived_set(Path::new("w")));
    }

    /// Class `d3` with an unavailable root inventory selects nothing: a
    /// classification failure must not widen the set.
    #[test]
    fn unavailable_derived_roots_select_nothing() {
        let obs = DerivedObservation {
            derived_set: DerivedRoots::Unavailable(err("classify roots")),
            ..baseline().observation(Path::new("w")).derived
        };
        assert_eq!(derived_selection(&obs), DerivedSelection::ReclaimDerived);
        assert_eq!(selected_derived_paths(&obs), BTreeSet::new());
    }

    /// Class `h1`: unregistered scratch — and a failed enumeration — cannot
    /// pass durable eligibility.
    #[test]
    fn class_h1_unregistered_or_unknown_registration_withholds() {
        for r in [
            RegistrationObservation::Unregistered,
            RegistrationObservation::Unknown(err("git worktree list")),
        ] {
            let row = Row { r, ..baseline() };
            assert_eq!(sets(&row).1, BTreeSet::new());
        }
    }

    /// Class `h2`: a non-ancestor, or an unprovable ancestry, withholds.
    #[test]
    fn class_h2_positive_or_unknown_ahead_withholds() {
        for a in [
            AheadObservation::Positive(1),
            AheadObservation::Unknown(err("git rev-list")),
        ] {
            let row = Row { a, ..baseline() };
            assert_eq!(sets(&row).1, BTreeSet::new());
        }
    }

    /// Class `h3`: a dirty tree, or a failed status probe, withholds. The
    /// second half is the hole this contract closes.
    #[test]
    fn class_h3_dirty_or_unknown_status_withholds() {
        for d in [
            DirtyObservation::Dirty(vec!["src/a.rs".to_owned()]),
            DirtyObservation::Unknown(err("git status")),
        ] {
            let row = Row { d, ..baseline() };
            assert_eq!(sets(&row).1, BTreeSet::new());
        }
    }

    /// Class `h4`: an ignored durable note, or an incomplete inventory,
    /// withholds — the second confirmed data-loss hole.
    #[test]
    fn class_h4_ignored_durable_or_unknown_inventory_withholds() {
        for i in [
            IgnoredDurableObservation::Present(vec!["operator-note.txt".to_owned()]),
            IgnoredDurableObservation::Unknown(err("git ls-files --ignored")),
        ] {
            let row = Row { i, ..baseline() };
            assert_eq!(sets(&row).1, BTreeSet::new());
        }
    }

    /// Class `h5`: all five conjuncts satisfied yields exactly `H(w)`.
    #[test]
    fn class_h5_all_conjuncts_yield_exactly_the_candidate() {
        assert_eq!(sets(&baseline()).1, BTreeSet::from([PathBuf::from("w")]));
    }

    /// A molecule-less directory can still yield derived content, and its
    /// stale status cannot become authority either way.
    #[test]
    fn absent_molecule_ignores_status_and_still_permits_derived() {
        for status in MoleculeStatus::ALL {
            let row = Row {
                m: MoleculeRecordObservation::Absent,
                s: StatusObservation::Known(status),
                ..baseline()
            };
            assert_eq!(sets(&row).0, derived_set(Path::new("w")), "{status:?}");
        }
    }

    /// Refutation mode 1 — false-red controls. For every dirty or
    /// non-ancestor row the durable set is empty, while a permitted,
    /// acquired row still yields the full derived set.
    #[test]
    fn refutation_false_red_controls() {
        for row in product() {
            let (derived, durable) = sets(&row);
            if row.d != DirtyObservation::Clean || row.a != AheadObservation::Zero {
                assert!(durable.is_empty(), "durable selected on unsafe row");
            }
            if row.gate() == "Yes" && row.l == LockObservation::Acquired {
                assert_eq!(derived, derived_set(Path::new("w")));
            }
        }
    }

    /// Refutation mode 2 — differential controls: each flip changes exactly
    /// the predicate the contract says it changes, with every other conjunct
    /// satisfied.
    #[test]
    fn refutation_differential_controls() {
        let base = baseline();
        let (base_derived, base_durable) = sets(&base);
        assert_eq!(base_derived, derived_set(Path::new("w")));
        assert_eq!(base_durable, BTreeSet::from([PathBuf::from("w")]));

        // Held → Acquired flips derived only.
        let held = Row {
            l: LockObservation::Held,
            ..base.clone()
        };
        let (held_derived, held_durable) = sets(&held);
        assert_eq!(held_derived, BTreeSet::new());
        assert_eq!(held_durable, base_durable);

        // Each durable conjunct flips durable only.
        for flipped in [
            Row {
                a: AheadObservation::Positive(3),
                ..base.clone()
            },
            Row {
                d: DirtyObservation::Dirty(vec!["note.md".to_owned()]),
                ..base.clone()
            },
            Row {
                i: IgnoredDurableObservation::Present(vec!["operator-note.txt".to_owned()]),
                ..base.clone()
            },
            Row {
                r: RegistrationObservation::Unregistered,
                ..base.clone()
            },
        ] {
            let (derived, durable) = sets(&flipped);
            assert_eq!(derived, base_derived);
            assert_eq!(durable, BTreeSet::new());
        }
    }

    /// An `Unknown` on an axis a predicate does not read cannot infect it.
    #[test]
    fn unknown_on_an_irrelevant_axis_does_not_infect_a_predicate() {
        let base = baseline();
        let (base_derived, base_durable) = sets(&base);
        for r in registrations() {
            for a in aheads() {
                for d in dirts() {
                    for i in ignoreds() {
                        let row = Row {
                            r: r.clone(),
                            a: a.clone(),
                            d: d.clone(),
                            i: i.clone(),
                            ..base.clone()
                        };
                        assert_eq!(sets(&row).0, base_derived, "derived is not blind");
                    }
                }
            }
        }
        for l in locks() {
            let row = Row { l, ..base.clone() };
            assert_eq!(sets(&row).1, base_durable, "durable reads the lock");
        }
    }

    /// The contract's one required concrete row: collapsed, registered,
    /// acquired, non-ancestor, dirty, no ignored durable content.
    #[test]
    fn required_row_collapsed_dirty_non_ancestor_yields_target() {
        let row = Row {
            a: AheadObservation::Positive(4),
            d: DirtyObservation::Dirty(vec!["src/a.rs".to_owned()]),
            ..baseline()
        };
        let (derived, durable) = sets(&row);
        assert_eq!(derived, derived_set(Path::new("w")));
        assert!(derived.contains(&PathBuf::from("w/target")));
        assert_eq!(durable, BTreeSet::new());
        // Changing I to Present preserves both sets — and the note.
        let with_note = Row {
            i: IgnoredDurableObservation::Present(vec!["operator-note.txt".to_owned()]),
            ..row
        };
        assert_eq!(sets(&with_note), (derived, durable));
    }

    /// Derived roots are validated as rebuildable, worktree-local relative
    /// paths — the root itself, traversal and durable areas are refused.
    #[test]
    fn derived_root_validation_refuses_unsafe_roots() {
        assert_eq!(
            validate_derived_root(Path::new("target")),
            Ok(PathBuf::from("target"))
        );
        assert_eq!(
            validate_derived_root(Path::new("node_modules/.cache")),
            Ok(PathBuf::from("node_modules/.cache"))
        );
        assert_eq!(
            validate_derived_root(Path::new("/tmp/target")),
            Err(DerivedRootRejection::NotWorktreeLocal)
        );
        assert_eq!(
            validate_derived_root(Path::new("../sibling/target")),
            Err(DerivedRootRejection::Traversal)
        );
        assert_eq!(
            validate_derived_root(Path::new("./target")),
            Err(DerivedRootRejection::Traversal)
        );
        assert_eq!(
            validate_derived_root(Path::new("")),
            Err(DerivedRootRejection::WorktreeRoot)
        );
        assert_eq!(
            validate_derived_root(Path::new(".git/objects")),
            Err(DerivedRootRejection::DurableArea)
        );
        assert_eq!(
            validate_derived_root(Path::new(".cosmon")),
            Err(DerivedRootRejection::DurableArea)
        );
    }

    /// The shared consumer decision: `Unknown` withholds, and a generic force
    /// flag cannot reinterpret it as clean.
    #[test]
    fn shared_removal_decision_withholds_on_unknown_even_with_force() {
        let failed = err("git status --porcelain");
        for force in [false, true] {
            assert_eq!(
                worktree_removal_decision(&DirtyObservation::Unknown(failed.clone()), force),
                WorktreeRemovalDecision::Withhold(failed.clone()),
            );
        }
        assert_eq!(
            worktree_removal_decision(&DirtyObservation::Clean, false),
            WorktreeRemovalDecision::Remove
        );
        let dirty = DirtyObservation::Dirty(vec!["a.rs".to_owned()]);
        assert_eq!(
            worktree_removal_decision(&dirty, false),
            WorktreeRemovalDecision::RefuseDirty(vec!["a.rs".to_owned()])
        );
        assert_eq!(
            worktree_removal_decision(&dirty, true),
            WorktreeRemovalDecision::Remove
        );
    }

    /// The port composes all seven axes; a fake supplies them without Git.
    #[test]
    fn injected_port_observes_every_axis() {
        struct Fake;
        impl WorktreeObservationPort for Fake {
            fn molecule_record(&self, _: &Path) -> MoleculeRecordObservation {
                MoleculeRecordObservation::Present
            }
            fn molecule_status(&self, _: &Path) -> StatusObservation {
                StatusObservation::Known(MoleculeStatus::Completed)
            }
            fn registration(&self, _: &Path) -> RegistrationObservation {
                RegistrationObservation::Registered
            }
            fn lock(&self, _: &Path) -> LockObservation {
                LockObservation::Acquired
            }
            fn commits_ahead(&self, _: &Path) -> AheadObservation {
                AheadObservation::Zero
            }
            fn dirty(&self, _: &Path) -> DirtyObservation {
                DirtyObservation::Clean
            }
            fn ignored_durable(&self, _: &Path) -> IgnoredDurableObservation {
                IgnoredDurableObservation::Absent
            }
            fn derived_roots(&self, candidate: &Path) -> DerivedRoots {
                DerivedRoots::Validated(derived_set(candidate))
            }
        }
        let obs = Fake.observe(Path::new("w"));
        assert_eq!(
            selected_derived_paths(&obs.derived),
            derived_set(Path::new("w"))
        );
        assert_eq!(
            advisory_durable_paths(&obs.durable),
            BTreeSet::from([PathBuf::from("w")])
        );
    }
}
