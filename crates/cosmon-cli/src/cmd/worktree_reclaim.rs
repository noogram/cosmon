// SPDX-License-Identifier: AGPL-3.0-only

//! The operator surface over the issue-61 reclaim contract.
//!
//! `cs purge --worktrees` and the `cs tackle` pre-spawn pressure check both
//! run the pass in this module. It is one code path with one safety question,
//! deliberately: the repository has already drifted once by answering the same
//! question in two places (`purge`'s dirty probe and the harvest transaction's,
//! which fail-opened differently for a year), and a second verb would have been
//! a third copy.
//!
//! What the pass does, in order:
//!
//! 1. **Enumerate** `readdir(.worktrees/) ∪ git worktree list --porcelain`
//!    ([`cosmon_harvest::worktree_reclaim::enumerate_candidates`]). The
//!    enumeration root is the filesystem and Git's registry — never the
//!    molecule roster, which on this repository saw 2 of 14 directories.
//! 2. **Observe** each candidate across the contract's seven axes.
//! 3. **Decide** with the two pure predicates. `derived_selection` may select
//!    a rebuildable payload; `durable_eligibility` is advisory and **nothing
//!    here acts on it** (ADR-178).
//! 4. **Report** — including every withheld candidate with a concrete reason.
//!    A withheld set nobody can see is the same invisible leak in the other
//!    direction, which is how 5 molecule-less directories went unnoticed.
//! 5. **Execute**, only when the operator asked, and only the derived set.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cosmon_core::worktree_reclaim::{
    consideration_gate, durable_eligibility, selected_derived_paths, AheadObservation,
    CandidateObservation, Consideration, DerivedRoots, DirtyObservation, DurableEligibility,
    IgnoredDurableObservation, LockObservation, MoleculeRecordObservation, ObservationError,
    RegistrationObservation, StatusObservation, WorktreeObservationPort,
};
use cosmon_harvest::worktree_reclaim::{
    approximate_size_bytes, describe_size, enumerate_candidates, reclaim_derived, CandidateSource,
    GitWorktreeObserver, LOCK_ANCHOR,
};
use cosmon_state::StateStore;

/// A candidate whose derived payload the pass selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selected {
    /// The candidate worktree.
    pub path: PathBuf,
    /// How it was found.
    pub source: CandidateSource,
    /// The exact derived roots selected — never the worktree itself.
    pub roots: BTreeSet<PathBuf>,
    /// Bytes under those roots, for the operator's arithmetic only.
    pub bytes: u64,
}

/// A candidate the pass declined to touch, and why.
///
/// `reasons` is the load-bearing field. "3 worktrees withheld" is not
/// actionable; "`task-…-0c2d`: molecule is Running — a resumable molecule
/// vetoes reclamation" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Withheld {
    /// The candidate worktree.
    pub path: PathBuf,
    /// How it was found.
    pub source: CandidateSource,
    /// One line per reason, each naming the thing that decided it.
    pub reasons: Vec<String>,
    /// Bytes under the candidate — the size of what stays.
    pub bytes: u64,
}

/// Everything one pass found and, if asked, did.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReclaimPass {
    /// Candidates whose derived payload may go.
    pub selected: Vec<Selected>,
    /// Candidates withheld, with reasons.
    pub withheld: Vec<Withheld>,
    /// Failures during enumeration. Non-empty means the population below is
    /// **incomplete** and must be presented as such.
    pub enumeration_errors: Vec<ObservationError>,
    /// Whether the pass actually removed anything.
    pub executed: bool,
    /// Entries removed, when it did.
    pub removed: BTreeSet<PathBuf>,
    /// The lock anchors deliberately preserved.
    pub preserved: BTreeSet<PathBuf>,
    /// Per-path removal failures; partial progress is reported, never hidden.
    pub failures: Vec<ObservationError>,
}

impl ReclaimPass {
    /// Bytes the selected roots occupy — what an execution would free.
    pub fn selected_bytes(&self) -> u64 {
        self.selected.iter().map(|s| s.bytes).sum()
    }

    /// Bytes the withheld candidates occupy — what stays whatever happens.
    pub fn withheld_bytes(&self) -> u64 {
        self.withheld.iter().map(|w| w.bytes).sum()
    }
}

// ---------------------------------------------------------------------------
// Reasons — pure over observations, so they are testable without a repository
// ---------------------------------------------------------------------------

/// Why the derived payload of this candidate was not selected.
///
/// `None` when it *was* selected. Pure over the observation so the string an
/// operator will read is asserted directly in a test, rather than inferred
/// from a count that stays right while the sentence rots.
pub(crate) fn derived_reason(obs: &CandidateObservation) -> Option<String> {
    let d = &obs.derived;
    if consideration_gate(&d.molecule, &d.status) == Consideration::No {
        return Some(gate_reason(&d.molecule, &d.status));
    }
    match &d.lock {
        LockObservation::Held => Some(format!(
            "another process holds the build lock at {} — reclaiming under \
             contention would delete a running build's output",
            d.path.join(LOCK_ANCHOR).display()
        )),
        LockObservation::ProbeFailed(e) => Some(format!(
            "the build lock could not be probed: {} — no exclusion, no removal",
            e.describe()
        )),
        LockObservation::Acquired => match &d.derived_set {
            DerivedRoots::Unavailable(e) => Some(format!(
                "the derived roots could not be inventoried: {}",
                e.describe()
            )),
            DerivedRoots::Validated(roots) if roots.is_empty() => {
                Some("no configured derived root is present here".to_owned())
            }
            DerivedRoots::Validated(_) => None,
        },
    }
}

/// Why this candidate's **durable** content is not reachable elsewhere.
///
/// Always reported, never acted on. Eligibility is advisory (ADR-178): this
/// string tells an operator what a manual `git worktree remove` would be
/// risking, and authorises nothing.
pub(crate) fn durable_reason(obs: &CandidateObservation) -> Option<String> {
    let h = &obs.durable;
    if durable_eligibility(h) == DurableEligibility::Eligible {
        return None;
    }
    if consideration_gate(&h.molecule, &h.status) == Consideration::No {
        return Some(gate_reason(&h.molecule, &h.status));
    }
    match &h.registration {
        RegistrationObservation::Unregistered => {
            return Some(
                "git does not know this directory as a worktree — unregistered \
                 scratch, whose contents nothing else holds a copy of"
                    .to_owned(),
            )
        }
        RegistrationObservation::Unknown(e) => {
            return Some(format!("registration is unknown: {}", e.describe()))
        }
        RegistrationObservation::Registered => {}
    }
    match &h.ahead {
        AheadObservation::Positive(n) => {
            return Some(format!(
                "{n} commit(s) are not reachable from the base branch"
            ))
        }
        AheadObservation::Unknown(e) => {
            return Some(format!(
                "ancestry is unknown: {} — a ref that cannot be proven merged is not merged",
                e.describe()
            ))
        }
        AheadObservation::Zero => {}
    }
    match &h.dirty {
        DirtyObservation::Dirty(paths) => {
            return Some(format!(
                "{} uncommitted file(s) in the worktree: {}",
                paths.len(),
                paths.join(", ")
            ))
        }
        DirtyObservation::Unknown(e) => {
            return Some(format!("the worktree status is unknown: {}", e.describe()))
        }
        DirtyObservation::Clean => {}
    }
    match &h.ignored_durable {
        IgnoredDurableObservation::Present(paths) => Some(format!(
            "{} ignored but durable path(s) live here: {}",
            paths.len(),
            paths.join(", ")
        )),
        IgnoredDurableObservation::Unknown(e) => Some(format!(
            "the ignored-content inventory is unknown: {}",
            e.describe()
        )),
        IgnoredDurableObservation::Absent => None,
    }
}

/// The `G` veto, said in the operator's words.
///
/// Shared by both predicates' reasons because it is literally the same
/// normalization; two copies would let one of them start saying something the
/// gate no longer does.
fn gate_reason(molecule: &MoleculeRecordObservation, status: &StatusObservation) -> String {
    match (molecule, status) {
        (MoleculeRecordObservation::Unknown(e), _) => {
            format!("the molecule record could not be read: {}", e.describe())
        }
        (MoleculeRecordObservation::Present, StatusObservation::Unknown(e)) => {
            format!("the molecule status could not be read: {}", e.describe())
        }
        (MoleculeRecordObservation::Present, StatusObservation::Known(s)) => format!(
            "molecule is {s} — a molecule that is not Completed or Collapsed \
             may still be resumed, and its worktree is where it resumes"
        ),
        // `Absent` never vetoes; reached only if the gate and this function
        // disagree, which the tests forbid.
        (MoleculeRecordObservation::Absent, _) => {
            "the consideration gate refused this candidate".to_owned()
        }
    }
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// Run the reclamation pass over one repository.
///
/// `execute` is the operator's gesture (`--allow-unharvested`), and it governs
/// **only** the derived tier. Whatever it is set to, no path in this function
/// removes a worktree: the durable set is computed, reported and dropped.
///
/// # Errors
/// Returns an error only when the observer itself cannot be built — which
/// means a configured derived root is not a rebuildable, worktree-local
/// relative path, and the configuration must be fixed before anything runs.
pub(crate) fn run_pass(
    repo_root: &Path,
    base_branch: &str,
    store: &dyn StateStore,
    evict: &[String],
    execute: bool,
) -> anyhow::Result<ReclaimPass> {
    let roots: Vec<&str> = evict.iter().map(String::as_str).collect();
    let observer = GitWorktreeObserver::new(repo_root, base_branch, store, &roots)
        .map_err(|e| anyhow::anyhow!("[worktree_reclaim].evict is invalid: {}", e.describe()))?;

    let enumeration = enumerate_candidates(repo_root);
    let mut pass = ReclaimPass {
        enumeration_errors: enumeration.errors,
        executed: execute,
        ..ReclaimPass::default()
    };

    for candidate in &enumeration.candidates {
        let obs = observer.observe(&candidate.path);
        let selected_roots = selected_derived_paths(&obs.derived);

        let mut reasons = Vec::new();
        if let Some(why) = derived_reason(&obs) {
            reasons.push(format!("derived: {why}"));
        }
        if let Some(why) = durable_reason(&obs) {
            reasons.push(format!("durable: {why}"));
        }
        // A root that validated but whose exclusion the adapter could not
        // establish is named here, not silently dropped from the set.
        for (root, why) in observer.withheld_roots(&candidate.path) {
            reasons.push(format!("root {}: {why}", root.display()));
        }

        if selected_roots.is_empty() {
            pass.withheld.push(Withheld {
                path: candidate.path.clone(),
                source: candidate.source,
                reasons,
                bytes: approximate_size_bytes(&candidate.path),
            });
            continue;
        }

        let bytes = selected_roots
            .iter()
            .map(|r| approximate_size_bytes(r))
            .sum();
        pass.selected.push(Selected {
            path: candidate.path.clone(),
            source: candidate.source,
            roots: selected_roots,
            bytes,
        });
        // Even a selected candidate's durable content is withheld — and said
        // so, because "we reclaimed this worktree's target/" must never read
        // as "we reclaimed this worktree".
        if !reasons.is_empty() {
            pass.withheld.push(Withheld {
                path: candidate.path.clone(),
                source: candidate.source,
                reasons,
                bytes: approximate_size_bytes(&candidate.path),
            });
        }

        if execute {
            match reclaim_derived(&observer, &obs.derived) {
                Ok(report) => {
                    pass.removed.extend(report.removed);
                    pass.preserved.extend(report.preserved);
                    pass.failures.extend(report.failures);
                }
                Err(e) => pass.failures.push(e),
            }
        }
    }
    Ok(pass)
}

/// Print the pass in the operator's register.
///
/// Deliberately prints the withheld set **before** the selected one: the
/// question an operator has after running this is "what is still holding
/// disk?", and the answer must not be below a list of successes.
pub(crate) fn report(pass: &ReclaimPass) {
    println!("\n.worktrees/ reclamation ({}):", mode_label(pass));
    for e in &pass.enumeration_errors {
        println!("  ENUMERATION INCOMPLETE — {}", e.describe());
    }

    println!(
        "  {} worktree(s) withheld, {}:",
        pass.withheld.len(),
        describe_size(pass.withheld_bytes())
    );
    for w in &pass.withheld {
        println!("    - {} [{}]", w.path.display(), w.source.label());
        for reason in &w.reasons {
            println!("        {reason}");
        }
    }

    println!(
        "  {} worktree(s) with reclaimable derived output, {}:",
        pass.selected.len(),
        describe_size(pass.selected_bytes())
    );
    for s in &pass.selected {
        for root in &s.roots {
            println!("    - {} ({})", root.display(), describe_size(s.bytes));
        }
    }

    if pass.executed {
        println!("  removed {} entr(ies).", pass.removed.len());
        for p in &pass.preserved {
            println!("    preserved lock anchor: {}", p.display());
        }
        for f in &pass.failures {
            println!("    FAILED — {}", f.describe());
        }
    } else if !pass.selected.is_empty() {
        println!(
            "  Nothing was removed. Repeat with --allow-unharvested to reclaim \
             the derived output above."
        );
    }
    println!(
        "  No path in this command removes a worktree; durable content is \
         reported, never reclaimed (ADR-178)."
    );
}

/// `dry run` / `executing`, for the register's first line.
fn mode_label(pass: &ReclaimPass) -> &'static str {
    if pass.executed {
        "executing — derived output only"
    } else {
        "dry run — nothing will be removed"
    }
}

/// The pass as JSON, for `--json`.
pub(crate) fn to_json(pass: &ReclaimPass) -> serde_json::Value {
    serde_json::json!({
        "executed": pass.executed,
        "enumeration_errors": pass.enumeration_errors.iter()
            .map(ObservationError::describe).collect::<Vec<_>>(),
        "withheld": pass.withheld.iter().map(|w| serde_json::json!({
            "path": w.path.display().to_string(),
            "source": w.source.label(),
            "reasons": w.reasons,
            "bytes": w.bytes,
        })).collect::<Vec<_>>(),
        "selected": pass.selected.iter().map(|s| serde_json::json!({
            "path": s.path.display().to_string(),
            "source": s.source.label(),
            "roots": s.roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>(),
            "bytes": s.bytes,
        })).collect::<Vec<_>>(),
        "removed": pass.removed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "preserved": pass.preserved.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "failures": pass.failures.iter().map(ObservationError::describe).collect::<Vec<_>>(),
    })
}

/// The repository root, or `None` when not invoked inside one.
///
/// `None` is not an error: `cs purge` runs in galaxies that are not git
/// repositories, and there is then no `.worktrees/` to reclaim.
pub(crate) fn discover_repo_root() -> Option<PathBuf> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_owned()))
        .filter(|p| !p.as_os_str().is_empty())
}

/// The branch candidate worktrees are measured against.
///
/// The galaxy's configured trunk, else whichever of `main`/`master` the
/// repository actually has. A wrong base makes every ancestry observation
/// `Unknown`, which withholds — the failure direction that keeps bytes.
pub(crate) fn base_branch(ctx: &super::Context, repo_root: &Path) -> String {
    if let Some(trunk) =
        cosmon_filestore::load_project_config(&super::resolve_config_from_context(ctx))
            .ok()
            .and_then(|cfg| cfg.project.trunk_branch)
    {
        return trunk;
    }
    for candidate in ["main", "master"] {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{candidate}"))
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            return candidate.to_owned();
        }
    }
    "main".to_owned()
}

/// The `[worktree_reclaim].evict` set for this invocation, with its default.
pub(crate) fn evict_roots(ctx: &super::Context) -> Vec<String> {
    cosmon_filestore::load_project_config(&super::resolve_config_from_context(ctx)).map_or_else(
        |_| cosmon_core::config::WorktreeReclaimConfig::default().evict,
        |cfg| cfg.worktree_reclaim.evict,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::molecule::MoleculeStatus;

    /// A candidate observation with every axis at its most permissive value,
    /// so each test below changes exactly one thing.
    fn permissive(path: &str) -> CandidateObservation {
        let path = PathBuf::from(path);
        CandidateObservation {
            derived: cosmon_core::worktree_reclaim::DerivedObservation {
                path: path.clone(),
                molecule: MoleculeRecordObservation::Present,
                status: StatusObservation::Known(MoleculeStatus::Collapsed),
                lock: LockObservation::Acquired,
                derived_set: DerivedRoots::Validated(BTreeSet::from([path.join("target")])),
            },
            durable: cosmon_core::worktree_reclaim::DurableObservation {
                path: path.clone(),
                molecule: MoleculeRecordObservation::Present,
                status: StatusObservation::Known(MoleculeStatus::Collapsed),
                registration: RegistrationObservation::Registered,
                ahead: AheadObservation::Zero,
                dirty: DirtyObservation::Clean,
                ignored_durable: IgnoredDurableObservation::Absent,
            },
        }
    }

    fn err() -> ObservationError {
        ObservationError::new(
            "git status --porcelain",
            "/w/x",
            "exit 128: not a git repository",
        )
    }

    /// Nothing to say when both predicates are satisfied — the register does
    /// not manufacture a reason for a candidate it did not withhold.
    #[test]
    fn a_fully_permitting_candidate_has_no_reasons() {
        let obs = permissive("/w/x");
        assert_eq!(derived_reason(&obs), None);
        assert_eq!(durable_reason(&obs), None);
    }

    /// Falsifier 3, per axis: every withhold names the thing that decided it,
    /// in the concrete register. Each assertion reads the rendered sentence.
    #[test]
    fn every_withheld_axis_names_its_own_reason() {
        // The G veto — a resumable molecule.
        let mut obs = permissive("/w/x");
        obs.derived.status = StatusObservation::Known(MoleculeStatus::Frozen);
        obs.durable.status = obs.derived.status.clone();
        let d = derived_reason(&obs).expect("frozen must withhold derived");
        assert!(d.contains("molecule is"), "{d}");
        assert!(d.contains("may still be resumed"), "{d}");
        assert_eq!(durable_reason(&obs), Some(d));

        // Lock contention.
        let mut obs = permissive("/w/x");
        obs.derived.lock = LockObservation::Held;
        let d = derived_reason(&obs).unwrap();
        assert!(d.contains("target/debug/.cargo-lock"), "{d}");
        assert!(d.contains("holds the build lock"), "{d}");

        // Lock probe failure.
        let mut obs = permissive("/w/x");
        obs.derived.lock = LockObservation::ProbeFailed(err());
        let d = derived_reason(&obs).unwrap();
        assert!(d.contains("could not be probed"), "{d}");
        assert!(d.contains("not a git repository"), "{d}");

        // Nothing present to reclaim is a reason too, not a silent omission.
        let mut obs = permissive("/w/x");
        obs.derived.derived_set = DerivedRoots::Validated(BTreeSet::new());
        assert_eq!(
            derived_reason(&obs),
            Some("no configured derived root is present here".to_owned())
        );

        // Registration.
        let mut obs = permissive("/w/x");
        obs.durable.registration = RegistrationObservation::Unregistered;
        let h = durable_reason(&obs).unwrap();
        assert!(
            h.contains("git does not know this directory as a worktree"),
            "{h}"
        );

        // Ancestry — the count, not a vague "unmerged".
        let mut obs = permissive("/w/x");
        obs.durable.ahead = AheadObservation::Positive(3);
        let h = durable_reason(&obs).unwrap();
        assert!(h.contains("3 commit(s) are not reachable"), "{h}");

        // Ancestry unknown is not ancestry zero.
        let mut obs = permissive("/w/x");
        obs.durable.ahead = AheadObservation::Unknown(err());
        let h = durable_reason(&obs).unwrap();
        assert!(h.contains("ancestry is unknown"), "{h}");
        assert!(h.contains("cannot be proven merged is not merged"), "{h}");

        // Dirt — the files, named.
        let mut obs = permissive("/w/x");
        obs.durable.dirty = DirtyObservation::Dirty(vec![" M src/lib.rs".to_owned()]);
        let h = durable_reason(&obs).unwrap();
        assert!(h.contains("src/lib.rs"), "{h}");

        // A failed status probe is neither clean nor dirty.
        let mut obs = permissive("/w/x");
        obs.durable.dirty = DirtyObservation::Unknown(err());
        let h = durable_reason(&obs).unwrap();
        assert!(h.contains("worktree status is unknown"), "{h}");

        // The ignored note that is the only copy of something.
        let mut obs = permissive("/w/x");
        obs.durable.ignored_durable =
            IgnoredDurableObservation::Present(vec!["operator-note.txt".to_owned()]);
        let h = durable_reason(&obs).unwrap();
        assert!(h.contains("operator-note.txt"), "{h}");
        assert!(h.contains("ignored but durable"), "{h}");
    }

    /// An `Unknown` on an axis a predicate does not read must not leak into
    /// that predicate's reason — the non-infection property, at the surface.
    #[test]
    fn an_unknown_on_an_irrelevant_axis_produces_no_derived_reason() {
        let mut obs = permissive("/w/x");
        obs.durable.registration = RegistrationObservation::Unknown(err());
        obs.durable.ahead = AheadObservation::Unknown(err());
        obs.durable.dirty = DirtyObservation::Unknown(err());
        obs.durable.ignored_durable = IgnoredDurableObservation::Unknown(err());
        assert_eq!(
            derived_reason(&obs),
            None,
            "derived selection is registration-, ancestry- and dirt-blind"
        );
        assert!(durable_reason(&obs).is_some());
    }

    /// The register's mode line cannot say "executing" for a pass that was
    /// not asked to execute.
    #[test]
    fn mode_label_tracks_execution() {
        let dry = ReclaimPass::default();
        assert!(mode_label(&dry).contains("dry run"));
        let wet = ReclaimPass {
            executed: true,
            ..ReclaimPass::default()
        };
        assert!(mode_label(&wet).contains("derived output only"));
    }
}
