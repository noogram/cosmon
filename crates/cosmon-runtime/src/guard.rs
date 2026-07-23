// SPDX-License-Identifier: AGPL-3.0-only

//! Backlog-sanity guard — the Autonomous-regime invariant specified by
//! [ADR-048](../../../docs/adr/048-backlog-sanity-invariant.md).
//!
//! # Why this exists
//!
//! On 2026-04-17, two `cs tackle` invocations on DAG roots auto-upgraded to
//! runtime mode and the resident runtime resurrected 14+ pending molecules
//! from 2026-04-14 that had been sedimenting in the backlog without any
//! `temp:*` tag — ~40 min of worker cycles burned on zombie work. This is
//! the *convoy cascade* class first named on 2026-04-12. The target-level
//! [`warn_if_stale_untagged`](../../cosmon-cli/src/cmd/guard.rs) nag was
//! silent that night because the *targets* (78bf, 87cd) were fresh; the
//! runtime walked past them into the 3-day-old untagged pendings.
//!
//! This module closes that gap. [`check_backlog`] is the precondition
//! called by both `cs tackle --runtime` and `cs run` before the walker
//! starts. It is a pure read over [`StateStore::list_molecules`]; the CLI
//! layer converts the returned [`SedimentReport`] into a typed refusal
//! (`GuardError::DirtyBacklogRuntimeRefusal`, exit code `12`).
//!
//! # Predicate (ADR-048 §2)
//!
//! A molecule is **sediment** iff
//!
//! ```text
//! status ∈ {Pending, Queued}
//!   ∧ age > 48h
//!   ∧ ¬∃ t ∈ tags : t.key == "temp"
//! ```
//!
//! A backlog is **dirty** iff `|sediment| ≥ N`, where `N` defaults to 5
//! and is overridable via `COSMON_RUNTIME_GUARD_STALE_THRESHOLD`.
//!
//! # Single perimeter
//!
//! The guard lives in `cosmon-runtime` rather than in either CLI command so
//! both entry points (`cs tackle` runtime branch, `cs run` bootstrap) call
//! one function. Duplicating the predicate at each call site would violate
//! the CLAUDE.md §*Architectural Discipline* coherence invariant #4
//! ("single perimeter").

use std::path::Path;

use chrono::{Duration, Utc};
use cosmon_core::error::CosmonError;
use cosmon_core::event_v2::{EventV2, Seq};
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::molecule_class::MoleculeClass;
use cosmon_state::{event_log, MoleculeData, MoleculeFilter, StateStore};

/// Default sediment-cardinality threshold above which the backlog is
/// considered *dirty* and runtime bootstrap is refused.
///
/// Tuned against the observed pathology (14 zombies on 2026-04-17, 13 on
/// 2026-04-12) while leaving headroom for normal workflow noise.
pub const DEFAULT_STALE_THRESHOLD: usize = 5;

/// Minimum age (in hours) a pending molecule must have reached before it
/// is counted as sediment.
///
/// Aligns with the CLAUDE.md §*Molecule Temperature Tags* curation rule
/// so operators have one mental model across the manual discipline and
/// the automatic guard.
pub const SEDIMENT_AGE_HOURS: i64 = 48;

/// Environment variable that overrides [`DEFAULT_STALE_THRESHOLD`].
///
/// Values that fail to parse as `usize` are ignored (the default is used).
/// Explicit `0` is accepted and disables the guard entirely.
pub const THRESHOLD_ENV_VAR: &str = "COSMON_RUNTIME_GUARD_STALE_THRESHOLD";

/// Maximum number of sediment molecule IDs carried in a refusal's sample.
const SAMPLE_LIMIT: usize = 5;

/// Summary of the sediment set observed by [`check_backlog`].
///
/// Emitted both as the body of a refusal (`GuardError::DirtyBacklogRuntimeRefusal`)
/// and as part of the audit event when `--force-runtime` is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SedimentReport {
    /// Total number of sediment molecules observed.
    pub count: usize,
    /// Up to `SAMPLE_LIMIT` molecule IDs, for operator-facing messages.
    pub sample: Vec<MoleculeId>,
    /// Threshold in force when the scan ran.
    pub threshold: usize,
}

impl SedimentReport {
    /// A clean backlog — no sediment observed.
    #[must_use]
    pub fn clean(threshold: usize) -> Self {
        Self {
            count: 0,
            sample: Vec::new(),
            threshold,
        }
    }

    /// Whether this report represents a *dirty* backlog per ADR-048.
    ///
    /// A `threshold` of `0` is treated as "guard disabled" and always
    /// reports clean.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.threshold > 0 && self.count >= self.threshold
    }
}

/// Error returned by [`check_backlog`] when the backlog is dirty and the
/// operator did not pass `--force-runtime`.
///
/// The CLI layer maps this into `GuardError::DirtyBacklogRuntimeRefusal`
/// to surface exit-code 12 and the canonical refusal message.
#[derive(Debug, thiserror::Error)]
pub enum BacklogGuardError {
    /// The backlog is dirty — refuse runtime bootstrap.
    #[error("backlog contains {} pending molecule(s) older than {} h without a temp:* tag (threshold {})", .0.count, SEDIMENT_AGE_HOURS, .0.threshold)]
    DirtyBacklog(SedimentReport),

    /// The store read failed.
    #[error("state store error: {0}")]
    State(#[from] CosmonError),
}

/// Return the sediment threshold in force — either the value of
/// [`THRESHOLD_ENV_VAR`] if it parses, or [`DEFAULT_STALE_THRESHOLD`].
#[must_use]
pub fn current_threshold() -> usize {
    threshold_from(std::env::var(THRESHOLD_ENV_VAR).ok().as_deref())
}

/// Resolve a raw override value into the effective sediment threshold.
///
/// Pure counterpart of [`current_threshold`]: takes the would-be
/// [`THRESHOLD_ENV_VAR`] value as a parameter so the parsing contract is
/// testable without mutating the process environment (process-wide env
/// writes race with concurrent test threads — the 2026-07-18 verify-gate
/// flake, diagnosed in task-20260719-ef32).
#[must_use]
pub fn threshold_from(raw: Option<&str>) -> usize {
    raw.and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(DEFAULT_STALE_THRESHOLD)
}

/// Return `true` if `m` satisfies the ADR-048 sediment predicate as of `now`.
#[must_use]
pub fn is_sediment(m: &MoleculeData, now: chrono::DateTime<Utc>) -> bool {
    let status_is_waiting = matches!(m.status, MoleculeStatus::Pending | MoleculeStatus::Queued);
    if !status_is_waiting {
        return false;
    }
    let age = now - m.updated_at;
    if age < Duration::hours(SEDIMENT_AGE_HOURS) {
        return false;
    }
    let has_temp_tag = m.tags.iter().any(|t| t.key() == "temp");
    !has_temp_tag
}

/// Compute the sediment report from a list of molecules and a threshold.
///
/// Pure: no I/O, no clock read. Call with `Utc::now()` in production;
/// tests inject a fixed clock to exercise boundary conditions.
#[must_use]
pub fn compute_sediment(
    molecules: &[MoleculeData],
    now: chrono::DateTime<Utc>,
    threshold: usize,
) -> SedimentReport {
    let mut sample = Vec::with_capacity(SAMPLE_LIMIT);
    let mut count = 0usize;
    for m in molecules {
        if !is_sediment(m, now) {
            continue;
        }
        count += 1;
        if sample.len() < SAMPLE_LIMIT {
            sample.push(m.id.clone());
        }
    }
    SedimentReport {
        count,
        sample,
        threshold,
    }
}

/// Check the backlog for sediment and return a refusal if it is dirty.
///
/// `force = true` bypasses the refusal but still returns the report so the
/// caller can emit the `runtime_guard_override` audit event with accurate
/// numbers.
///
/// # Errors
///
/// - [`BacklogGuardError::DirtyBacklog`] when `force` is false and the
///   sediment count is at or above the configured threshold.
/// - [`BacklogGuardError::State`] if the underlying store read fails.
pub fn check_backlog(
    store: &dyn StateStore,
    force: bool,
) -> Result<SedimentReport, BacklogGuardError> {
    check_backlog_with_threshold(store, force, current_threshold())
}

/// [`check_backlog`] with the threshold injected instead of read from the
/// environment.
///
/// This is the seam tests use to exercise tightened or disabled thresholds
/// without `std::env::set_var` — a process-wide write that races with
/// parallel test threads (the 2026-07-18 flake, task-20260719-ef32).
/// Production callers go through [`check_backlog`], which resolves the
/// operator override once and delegates here.
///
/// # Errors
///
/// Same contract as [`check_backlog`].
pub fn check_backlog_with_threshold(
    store: &dyn StateStore,
    force: bool,
    threshold: usize,
) -> Result<SedimentReport, BacklogGuardError> {
    if threshold == 0 {
        // Explicit opt-out: operator set the env var to 0. Skip the read.
        return Ok(SedimentReport::clean(0));
    }
    let molecules = store.list_molecules(&MoleculeFilter::default())?;
    let report = compute_sediment(&molecules, Utc::now(), threshold);
    if !force && report.is_dirty() {
        return Err(BacklogGuardError::DirtyBacklog(report));
    }
    Ok(report)
}

// ---------------------------------------------------------------------------
// Stress-test prior-seal guard (ADR-085 §M2 — Layer 1)
// ---------------------------------------------------------------------------

/// Outcome of a [`check_prior_seal`] read.
///
/// Carries enough detail for the CLI layer to surface a precise refusal
/// (`prior.md` missing vs `prior.b3` missing vs no [`EventV2::SealAttested`]
/// matching `prior.b3`) without re-reading the molecule directory or the
/// event log.
///
/// `is_sealed()` is the single boolean the dispatch path consults; the
/// other fields are diagnostic and feed the audit trail when an operator
/// chooses to bypass via `--bypass-seal --bypass-reason "<…>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealReport {
    /// Class observed at the time of the check.
    pub class: MoleculeClass,
    /// Whether `<molecule_dir>/prior.md` is present on disk.
    pub prior_md_present: bool,
    /// Whether `<molecule_dir>/prior.b3` is present on disk.
    pub prior_b3_present: bool,
    /// BLAKE3 hash read from `<molecule_dir>/prior.b3` (trimmed),
    /// `None` if the file is absent or unreadable.
    pub prior_b3_hash: Option<String>,
    /// Whether the event log contains a matching
    /// [`EventV2::SealAttested`] (same molecule id and same `prior_b3`).
    pub attestation_found: bool,
}

/// Sentinel string written into [`SealReport::missing_condition`] when
/// every layer-1 precondition holds (or the class does not require a
/// seal at all).
const SEAL_OK: &str = "ok";

impl SealReport {
    /// Build a report for a class that does not require a stress-test
    /// seal — the layer-1 gate is a no-op for [`MoleculeClass::Standard`]
    /// and [`MoleculeClass::Infra`].
    #[must_use]
    pub fn not_required(class: MoleculeClass) -> Self {
        Self {
            class,
            prior_md_present: false,
            prior_b3_present: false,
            prior_b3_hash: None,
            attestation_found: true,
        }
    }

    /// `true` when the molecule may be dispatched per ADR-085 §M2.
    ///
    /// Non-stress-test classes always pass (the gate is opt-in by class
    /// declaration). Stress-test molecules must satisfy all four
    /// conditions of the ADR §Decision §2 predicate.
    #[must_use]
    pub fn is_sealed(&self) -> bool {
        if !self.class.requires_seal() {
            return true;
        }
        self.prior_md_present && self.prior_b3_present && self.attestation_found
    }

    /// First failing condition encountered, or `SEAL_OK` when the
    /// report passes. Stable strings — the CLI surfaces them in
    /// `BypassReceipt.bypassed_condition` for forensic audit.
    #[must_use]
    pub fn missing_condition(&self) -> &'static str {
        if !self.class.requires_seal() {
            return SEAL_OK;
        }
        if !self.prior_md_present {
            "prior-md-missing"
        } else if !self.prior_b3_present {
            "prior-b3-missing"
        } else if !self.attestation_found {
            "seal-attestation-missing"
        } else {
            SEAL_OK
        }
    }
}

/// Errors returned by [`check_prior_seal`].
///
/// The CLI layer translates [`SealGuardError::SealMissing`] into the
/// canonical refusal exit code `13` (extending the ADR-048 family:
/// 12 = dirty backlog, 13 = missing seal — see ADR-085 §Decision §2).
#[derive(Debug, thiserror::Error)]
pub enum SealGuardError {
    /// A stress-test molecule reached dispatch without a complete seal.
    #[error(
        "stress-test molecule {molecule_id} cannot be dispatched: {reason} \
         (prior.md={prior_md}, prior.b3={prior_b3}, attestation={attestation})"
    )]
    SealMissing {
        /// The molecule that failed the gate.
        molecule_id: MoleculeId,
        /// Stable identifier of the first failing condition — one of
        /// `prior-md-missing`, `prior-b3-missing`,
        /// `seal-attestation-missing`. Suitable for use as
        /// `BypassReceipt.bypassed_condition`.
        reason: &'static str,
        /// Whether `prior.md` was present at the time of the check.
        prior_md: bool,
        /// Whether `prior.b3` was present at the time of the check.
        prior_b3: bool,
        /// Whether a matching attestation event was found.
        attestation: bool,
    },

    /// Reading `prior.b3` failed for an I/O reason that was not "file
    /// absent" (which is reported as a missing condition, not an error).
    #[error("io error while reading prior.b3: {0}")]
    Io(#[from] std::io::Error),
}

/// Layer-1 precondition for `cs tackle` of a stress-test molecule
/// (ADR-085 §Decision §2).
///
/// Pure read over the filesystem and the event log — does not mutate
/// state. Call sites:
///
/// - `cs tackle` — invoked before dispatch; refusal aborts the verb.
/// - Future `cs run` walker — invoked per ready node before scheduling
///   a worker; refusal collapses the molecule with a typed reason.
///
/// `force = true` bypasses the refusal but still returns the report so
/// the CLI can stamp the [`BypassReceipt`](cosmon_core::molecule_class::BypassReceipt)
/// fields with accurate observations and emit
/// [`EventV2::SealBypassed`] via [`emit_seal_bypassed`].
///
/// # Predicate
///
/// A stress-test molecule is dispatchable iff:
///
/// 1. `<molecule_dir>/prior.md` exists.
/// 2. `<molecule_dir>/prior.b3` exists and contains a BLAKE3 hash.
/// 3. The event log contains a [`EventV2::SealAttested`] whose
///    `molecule_id` matches the molecule and whose `prior_b3` matches
///    the on-disk `prior.b3`.
///
/// Non-stress-test classes always pass through (the gate is opt-in by
/// class declaration, per ADR-085 §1).
///
/// # Errors
///
/// - [`SealGuardError::SealMissing`] if `force` is `false` and any
///   condition fails.
/// - [`SealGuardError::Io`] if `prior.b3` exists but cannot be read.
///   A missing `prior.b3` file is *not* an I/O error — it is reported
///   via the `prior_b3_present: false` condition.
pub fn check_prior_seal(
    molecule: &MoleculeData,
    molecule_dir: &Path,
    events_log: &Path,
    force: bool,
) -> Result<SealReport, SealGuardError> {
    if !molecule.class.requires_seal() {
        return Ok(SealReport::not_required(molecule.class));
    }

    let prior_md_present = molecule_dir.join("prior.md").is_file();
    let prior_b3_path = molecule_dir.join("prior.b3");
    let prior_b3_hash: Option<String> = if prior_b3_path.is_file() {
        let raw = std::fs::read_to_string(&prior_b3_path)?;
        Some(raw.trim().to_owned())
    } else {
        None
    };
    let prior_b3_present = prior_b3_hash.as_deref().is_some_and(|h| !h.is_empty());

    // Tolerate a missing event log: cold projects have no events.jsonl
    // until the first verb fires. Treat unreadable as "no attestation
    // found" rather than failing the check — the operator's path to
    // recovery is to author the seal, not to debug the log.
    let attestation_found = if let Some(ref hash) = prior_b3_hash {
        match event_log::read_all(events_log) {
            Ok(envelopes) => envelopes.iter().any(|env| {
                matches!(
                    &env.event,
                    EventV2::SealAttested { molecule_id, prior_b3, .. }
                        if molecule_id == &molecule.id && prior_b3 == hash
                )
            }),
            Err(_) => false,
        }
    } else {
        false
    };

    let report = SealReport {
        class: molecule.class,
        prior_md_present,
        prior_b3_present,
        prior_b3_hash,
        attestation_found,
    };

    if !force && !report.is_sealed() {
        return Err(SealGuardError::SealMissing {
            molecule_id: molecule.id.clone(),
            reason: report.missing_condition(),
            prior_md: report.prior_md_present,
            prior_b3: report.prior_b3_present,
            attestation: report.attestation_found,
        });
    }
    Ok(report)
}

/// Emit an [`EventV2::SealBypassed`] event to the project's `events.jsonl`.
///
/// Per ADR-085 §3.5 the bypass path is **structured, not free-text**:
/// this helper is the single seam through which the runtime / CLI may
/// record a deliberate override. It enforces that `reason` is non-empty
/// (matching the `--bypass-reason "<…>"` requirement of M4); a blank
/// reason short-circuits to `Ok(None)` without writing anything, so the
/// CLI cannot accidentally normalise a silent bypass.
///
/// `bypass_receipt_b3` is the BLAKE3 hash of the on-disk
/// `bypass-receipt.json` written by the CLI when M4 lands; for M2 the
/// helper accepts any 64-char hex string the caller supplies.
///
/// # Errors
///
/// Forwards `std::io::Error` from
/// [`cosmon_state::event_log::emit_one`].
pub fn emit_seal_bypassed(
    events_log: &Path,
    molecule_id: &MoleculeId,
    bypass_receipt_b3: String,
    reason: &str,
) -> std::io::Result<Option<Seq>> {
    if reason.trim().is_empty() {
        return Ok(None);
    }
    if let Some(parent) = events_log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let event = EventV2::SealBypassed {
        molecule_id: molecule_id.clone(),
        bypass_receipt_b3,
        reason: reason.to_owned(),
    };
    let seq = event_log::emit_one(events_log, event, None)?;
    Ok(Some(seq))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::tag::Tag;
    use std::collections::{BTreeSet, HashMap};

    fn mk_mol_at(
        now: chrono::DateTime<Utc>,
        id: &str,
        status: MoleculeStatus,
        age_hours: i64,
        tags: &[&str],
    ) -> MoleculeData {
        let mut t = BTreeSet::new();
        for raw in tags {
            t.insert(Tag::new((*raw).to_owned()).unwrap());
        }
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::default(),
            assigned_worker: None,
            created_at: now - Duration::hours(age_hours + 1),
            updated_at: now - Duration::hours(age_hours),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: t,
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
            adapter: None,
        }
    }

    // Convenience when the test uses `Utc::now()` internally. Callers
    // that need boundary-sharp age assertions must use `mk_mol_at` with
    // a frozen clock — otherwise `updated_at` drifts between the
    // `mk_mol` call and the `is_sediment` call and 48h exactly flips.
    fn mk_mol(id: &str, status: MoleculeStatus, age_hours: i64, tags: &[&str]) -> MoleculeData {
        mk_mol_at(Utc::now(), id, status, age_hours, tags)
    }

    #[test]
    fn sediment_requires_pending_or_queued() {
        let now = Utc::now();
        assert!(is_sediment(
            &mk_mol("task-20260411-a001", MoleculeStatus::Pending, 72, &[]),
            now
        ));
        assert!(is_sediment(
            &mk_mol("task-20260411-a002", MoleculeStatus::Queued, 72, &[]),
            now
        ));
        assert!(!is_sediment(
            &mk_mol("task-20260411-a003", MoleculeStatus::Running, 72, &[]),
            now
        ));
        assert!(!is_sediment(
            &mk_mol("task-20260411-a004", MoleculeStatus::Completed, 72, &[]),
            now
        ));
    }

    #[test]
    fn sediment_requires_age_above_48h() {
        // Freeze a single `now` so boundary-sharp age assertions don't
        // race the wall clock between mol construction and the check.
        let now = Utc::now();
        // Boundary: exactly 48h IS sediment (predicate uses `< 48h`
        // returns early, so the 48h mark passes through).
        assert!(is_sediment(
            &mk_mol_at(now, "task-20260411-b001", MoleculeStatus::Pending, 48, &[]),
            now
        ));
        assert!(!is_sediment(
            &mk_mol_at(now, "task-20260411-b002", MoleculeStatus::Pending, 47, &[]),
            now
        ));
        assert!(!is_sediment(
            &mk_mol_at(now, "task-20260411-b003", MoleculeStatus::Pending, 0, &[]),
            now
        ));
    }

    #[test]
    fn sediment_respects_any_temp_tag() {
        let now = Utc::now();
        for tag in &["temp:hot", "temp:warm", "temp:cold", "temp:frozen"] {
            assert!(
                !is_sediment(
                    &mk_mol("task-20260411-c001", MoleculeStatus::Pending, 72, &[tag]),
                    now
                ),
                "tag {tag} should exempt from sediment"
            );
        }
        assert!(is_sediment(
            &mk_mol("task-20260411-c002", MoleculeStatus::Pending, 72, &["bug"]),
            now
        ));
    }

    #[test]
    fn compute_sediment_caps_sample_at_five() {
        let now = Utc::now();
        let mols: Vec<_> = (0..10)
            .map(|i| {
                let id = format!("task-20260411-d{i:03}");
                mk_mol(&id, MoleculeStatus::Pending, 72, &[])
            })
            .collect();
        let report = compute_sediment(&mols, now, DEFAULT_STALE_THRESHOLD);
        assert_eq!(report.count, 10);
        assert_eq!(report.sample.len(), SAMPLE_LIMIT);
        assert!(report.is_dirty());
    }

    #[test]
    fn clean_backlog_below_threshold() {
        let now = Utc::now();
        let mols = vec![mk_mol(
            "task-20260411-e001",
            MoleculeStatus::Pending,
            72,
            &[],
        )];
        let report = compute_sediment(&mols, now, DEFAULT_STALE_THRESHOLD);
        assert_eq!(report.count, 1);
        assert!(!report.is_dirty());
    }

    #[test]
    fn threshold_zero_disables_guard() {
        let report = SedimentReport {
            count: 1000,
            sample: Vec::new(),
            threshold: 0,
        };
        assert!(!report.is_dirty());
    }

    #[test]
    fn clean_report_is_never_dirty() {
        assert!(!SedimentReport::clean(DEFAULT_STALE_THRESHOLD).is_dirty());
        assert!(!SedimentReport::clean(0).is_dirty());
    }

    // Boundary proptest: the dirtiness flip happens exactly at `count ==
    // threshold`. This is the semantics callers depend on to script
    // exit-code 12 handling, so freeze it.
    //
    // Uses the core `proptest!` macro via the workspace dep; enabled
    // here as a unit test to keep the crate dev-dep footprint small.
    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config { cases: 64, .. proptest::test_runner::Config::default() })]

        #[test]
        fn dirtiness_boundary_is_exact(
            count in 0_usize..1000,
            threshold in 1_usize..50,
        ) {
            let report = SedimentReport {
                count,
                sample: Vec::new(),
                threshold,
            };
            proptest::prop_assert_eq!(report.is_dirty(), count >= threshold);
        }
    }

    #[test]
    fn check_backlog_on_empty_store_is_clean() {
        use cosmon_filestore::FileStore;
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let report = check_backlog(&store, false).unwrap();
        assert_eq!(report.count, 0);
    }

    #[test]
    fn check_backlog_refuses_when_dirty() {
        use cosmon_filestore::FileStore;
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        for i in 0..DEFAULT_STALE_THRESHOLD {
            let m = mk_mol(
                &format!("task-20260411-f{i:03}"),
                MoleculeStatus::Pending,
                96,
                &[],
            );
            store.save_molecule(&m.id, &m).unwrap();
        }
        let err = check_backlog(&store, false).unwrap_err();
        match err {
            BacklogGuardError::DirtyBacklog(r) => {
                assert_eq!(r.count, DEFAULT_STALE_THRESHOLD);
                assert!(r.is_dirty());
            }
            other @ BacklogGuardError::State(_) => panic!("expected DirtyBacklog, got {other:?}"),
        }
    }

    #[test]
    fn check_backlog_force_returns_report_without_refusing() {
        use cosmon_filestore::FileStore;
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        for i in 0..DEFAULT_STALE_THRESHOLD {
            let m = mk_mol(
                &format!("task-20260411-g{i:03}"),
                MoleculeStatus::Pending,
                96,
                &[],
            );
            store.save_molecule(&m.id, &m).unwrap();
        }
        let report = check_backlog(&store, true).unwrap();
        assert_eq!(report.count, DEFAULT_STALE_THRESHOLD);
        assert!(report.is_dirty(), "force still reports dirty for audit");
    }

    #[test]
    fn check_backlog_ignores_tagged_and_fresh_pendings() {
        use cosmon_filestore::FileStore;
        let tmp = tempfile::TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        // 10 pendings, but all tagged or fresh: none count as sediment.
        for i in 0..5 {
            let m = mk_mol(
                &format!("task-20260411-h{i:03}"),
                MoleculeStatus::Pending,
                96,
                &["temp:cold"],
            );
            store.save_molecule(&m.id, &m).unwrap();
        }
        for i in 0..5 {
            let m = mk_mol(
                &format!("task-20260411-i{i:03}"),
                MoleculeStatus::Pending,
                0,
                &[],
            );
            store.save_molecule(&m.id, &m).unwrap();
        }
        let report = check_backlog(&store, false).unwrap();
        assert_eq!(report.count, 0);
    }

    // -- ADR-085 §M2 — stress-test prior-seal guard ------------------------

    /// Build a molecule with the given class for prior-seal tests.
    /// Status / age are irrelevant to the seal predicate (the gate
    /// fires regardless of backlog freshness).
    fn mk_class_mol(id: &str, class: MoleculeClass) -> MoleculeData {
        let now = Utc::now();
        let mut m = mk_mol_at(now, id, MoleculeStatus::Pending, 1, &[]);
        m.class = class;
        m
    }

    /// Write a 64-char hex string to `<dir>/prior.b3`. Helper for the
    /// `SealAttested` matching tests.
    fn write_seal(dir: &Path, hash: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("prior.md"), b"prior content").unwrap();
        std::fs::write(dir.join("prior.b3"), hash).unwrap();
    }

    /// Append a `SealAttested` envelope referencing `(molecule_id,
    /// prior_b3)` to `events_log`.
    fn write_attestation(events_log: &Path, molecule_id: &MoleculeId, prior_b3: &str) {
        let event = EventV2::SealAttested {
            molecule_id: molecule_id.clone(),
            prior_b3: prior_b3.to_owned(),
            sealed_at: Utc::now(),
            witness_id: "tmux:witness".to_owned(),
            attestation_b3: "0".repeat(64),
        };
        if let Some(parent) = events_log.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        event_log::emit_one(events_log, event, None).unwrap();
    }

    #[test]
    fn seal_report_not_required_passes_for_standard_class() {
        let r = SealReport::not_required(MoleculeClass::Standard);
        assert!(r.is_sealed());
        assert_eq!(r.missing_condition(), SEAL_OK);
    }

    #[test]
    fn seal_report_not_required_passes_for_infra_class() {
        let r = SealReport::not_required(MoleculeClass::Infra);
        assert!(r.is_sealed());
    }

    #[test]
    fn seal_report_missing_condition_chain() {
        let mut r = SealReport {
            class: MoleculeClass::StressTest,
            prior_md_present: false,
            prior_b3_present: false,
            prior_b3_hash: None,
            attestation_found: false,
        };
        assert_eq!(r.missing_condition(), "prior-md-missing");
        r.prior_md_present = true;
        assert_eq!(r.missing_condition(), "prior-b3-missing");
        r.prior_b3_present = true;
        assert_eq!(r.missing_condition(), "seal-attestation-missing");
        r.attestation_found = true;
        assert_eq!(r.missing_condition(), SEAL_OK);
        assert!(r.is_sealed());
    }

    #[test]
    fn check_prior_seal_skips_standard_class() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mol = mk_class_mol("task-20260504-aa01", MoleculeClass::Standard);
        let report =
            check_prior_seal(&mol, tmp.path(), &tmp.path().join("events.jsonl"), false).unwrap();
        assert_eq!(report.class, MoleculeClass::Standard);
        assert!(report.is_sealed());
    }

    /// Integration test: refuse-dispatch-without-seal.
    /// A stress-test molecule with no `prior.md` / `prior.b3` is
    /// refused by the layer-1 gate.
    #[test]
    fn check_prior_seal_refuse_dispatch_without_seal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mol = mk_class_mol("delib-20260504-bb01", MoleculeClass::StressTest);
        let err = check_prior_seal(&mol, tmp.path(), &tmp.path().join("events.jsonl"), false)
            .unwrap_err();
        match err {
            SealGuardError::SealMissing {
                molecule_id,
                reason,
                prior_md,
                prior_b3,
                attestation,
            } => {
                assert_eq!(molecule_id.as_str(), "delib-20260504-bb01");
                assert_eq!(reason, "prior-md-missing");
                assert!(!prior_md);
                assert!(!prior_b3);
                assert!(!attestation);
            }
            other @ SealGuardError::Io(_) => panic!("expected SealMissing, got {other:?}"),
        }
    }

    #[test]
    fn check_prior_seal_refuse_when_prior_md_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("prior.md"), b"prior content").unwrap();
        let mol = mk_class_mol("delib-20260504-bb02", MoleculeClass::StressTest);
        let err = check_prior_seal(&mol, tmp.path(), &tmp.path().join("events.jsonl"), false)
            .unwrap_err();
        match err {
            SealGuardError::SealMissing {
                reason, prior_b3, ..
            } => {
                assert_eq!(reason, "prior-b3-missing");
                assert!(!prior_b3);
            }
            other @ SealGuardError::Io(_) => {
                panic!("expected SealMissing prior-b3-missing, got {other:?}")
            }
        }
    }

    #[test]
    fn check_prior_seal_refuse_when_attestation_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hash = "a".repeat(64);
        write_seal(tmp.path(), &hash);
        let mol = mk_class_mol("delib-20260504-bb03", MoleculeClass::StressTest);
        let err = check_prior_seal(&mol, tmp.path(), &tmp.path().join("events.jsonl"), false)
            .unwrap_err();
        match err {
            SealGuardError::SealMissing {
                reason,
                prior_md,
                prior_b3,
                attestation,
                ..
            } => {
                assert_eq!(reason, "seal-attestation-missing");
                assert!(prior_md);
                assert!(prior_b3);
                assert!(!attestation);
            }
            other @ SealGuardError::Io(_) => panic!("expected attestation-missing, got {other:?}"),
        }
    }

    /// Integration test: allow-dispatch-with-attestation.
    /// A stress-test molecule with `prior.md`, `prior.b3`, and a
    /// matching `SealAttested` event passes the layer-1 gate.
    #[test]
    fn check_prior_seal_allow_dispatch_with_attestation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hash = "b".repeat(64);
        write_seal(tmp.path(), &hash);
        let mol = mk_class_mol("delib-20260504-cc01", MoleculeClass::StressTest);
        let events_log = tmp.path().join("events.jsonl");
        write_attestation(&events_log, &mol.id, &hash);

        let report = check_prior_seal(&mol, tmp.path(), &events_log, false).unwrap();
        assert!(report.is_sealed(), "expected sealed: {report:?}");
        assert!(report.prior_md_present);
        assert!(report.prior_b3_present);
        assert!(report.attestation_found);
        assert_eq!(report.prior_b3_hash.as_deref(), Some(hash.as_str()));
    }

    #[test]
    fn check_prior_seal_attestation_for_other_molecule_does_not_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hash = "c".repeat(64);
        write_seal(tmp.path(), &hash);
        let mol = mk_class_mol("delib-20260504-cc02", MoleculeClass::StressTest);
        let other = MoleculeId::new("delib-20260504-zzzz").unwrap();
        let events_log = tmp.path().join("events.jsonl");
        write_attestation(&events_log, &other, &hash);

        let err = check_prior_seal(&mol, tmp.path(), &events_log, false).unwrap_err();
        assert!(matches!(
            err,
            SealGuardError::SealMissing {
                reason: "seal-attestation-missing",
                ..
            }
        ));
    }

    #[test]
    fn check_prior_seal_attestation_with_wrong_hash_does_not_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hash_on_disk = "d".repeat(64);
        write_seal(tmp.path(), &hash_on_disk);
        let mol = mk_class_mol("delib-20260504-cc03", MoleculeClass::StressTest);
        let events_log = tmp.path().join("events.jsonl");
        let other_hash = "e".repeat(64);
        write_attestation(&events_log, &mol.id, &other_hash);

        let err = check_prior_seal(&mol, tmp.path(), &events_log, false).unwrap_err();
        assert!(matches!(
            err,
            SealGuardError::SealMissing {
                reason: "seal-attestation-missing",
                ..
            }
        ));
    }

    #[test]
    fn check_prior_seal_force_returns_report_without_refusing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mol = mk_class_mol("delib-20260504-dd01", MoleculeClass::StressTest);
        // No seal on disk, no attestation — but force=true short-circuits
        // the refusal so the audit trail can still record observations.
        let report =
            check_prior_seal(&mol, tmp.path(), &tmp.path().join("events.jsonl"), true).unwrap();
        assert!(!report.is_sealed());
        assert_eq!(report.missing_condition(), "prior-md-missing");
    }

    #[test]
    fn check_prior_seal_tolerates_missing_events_log() {
        let tmp = tempfile::TempDir::new().unwrap();
        let hash = "f".repeat(64);
        write_seal(tmp.path(), &hash);
        let mol = mk_class_mol("delib-20260504-ee01", MoleculeClass::StressTest);
        // No events.jsonl on disk: attestation_found = false, gate refuses
        // — but the unwrap_err proves the guard did not panic on the
        // missing log.
        let err = check_prior_seal(
            &mol,
            tmp.path(),
            &tmp.path().join("does-not-exist.jsonl"),
            false,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SealGuardError::SealMissing {
                reason: "seal-attestation-missing",
                ..
            }
        ));
    }

    #[test]
    fn emit_seal_bypassed_returns_none_for_blank_reason() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events_log = tmp.path().join("events.jsonl");
        let mol = MoleculeId::new("delib-20260504-ff01").unwrap();
        let receipt_b3 = "0".repeat(64);

        let result = emit_seal_bypassed(&events_log, &mol, receipt_b3.clone(), "").unwrap();
        assert!(result.is_none());
        assert!(!events_log.exists(), "no event file should be touched");

        let result = emit_seal_bypassed(&events_log, &mol, receipt_b3, "   ").unwrap();
        assert!(result.is_none(), "whitespace-only reason is also rejected");
    }

    #[test]
    fn emit_seal_bypassed_writes_event_when_reason_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events_log = tmp.path().join("state").join("events.jsonl");
        let mol = MoleculeId::new("delib-20260504-ff02").unwrap();
        let receipt_b3 = "1".repeat(64);

        let seq = emit_seal_bypassed(
            &events_log,
            &mol,
            receipt_b3.clone(),
            "emergency dispatch — incident triage",
        )
        .unwrap();
        assert!(seq.is_some());

        let envs = event_log::read_all(&events_log).unwrap();
        assert_eq!(envs.len(), 1);
        match &envs[0].event {
            EventV2::SealBypassed {
                molecule_id,
                bypass_receipt_b3,
                reason,
            } => {
                assert_eq!(molecule_id, &mol);
                assert_eq!(bypass_receipt_b3, &receipt_b3);
                assert_eq!(reason, "emergency dispatch — incident triage");
            }
            other => panic!("expected SealBypassed, got {other:?}"),
        }
    }
}
