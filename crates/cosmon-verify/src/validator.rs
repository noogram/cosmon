// SPDX-License-Identifier: AGPL-3.0-only

//! The replay loop — reads envelopes one at a time, runs every invariant
//! against the pre-transition state, then mutates the state.
//!
//! This is deliberately a *single-pass* linear scan: the whole point of
//! trace validation over model checking is that we only ever visit each
//! transition once. Short-circuit on first violation so CI surfaces the
//! earliest diagnostic information.

use std::fs;
use std::path::Path;

use cosmon_core::event_v2::{Envelope, EventV2, MergeResult};
use cosmon_core::molecule::MoleculeStatus;
use serde::Serialize;

use crate::error::{ValidationError, Violation};
use crate::invariants::InvariantBox;
use crate::model::{MoleculeTraceState, SchedulerState, WorkerTraceState};

/// Outcome of validating a trace.
///
/// `Ok` carries summary statistics (so CI can print "replayed N events");
/// `Violation` carries the first failing invariant's report. Any subsequent
/// violations are intentionally suppressed — the earliest one is the
/// actionable one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ValidationOutcome {
    /// Trace certified — every event passed every invariant.
    Ok {
        /// Number of events replayed (after skipping blanks).
        events_replayed: u64,
        /// Number of distinct molecules seen in the trace.
        molecules_seen: u64,
        /// Number of lines skipped because their shape was not recognised by
        /// either `EventV2` or the legacy migration helper. Always `0` when
        /// `skip_unknown` is disabled (unrecognised lines hard-fail instead).
        skipped_unknown: u64,
    },
    /// Trace violated an invariant — `violation` points at the first failure.
    Violation {
        /// Number of events successfully replayed *before* the failure.
        events_replayed_before: u64,
        /// The offending invariant and its message.
        violation: Violation,
    },
}

impl ValidationOutcome {
    /// `true` iff the trace was certified.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

/// Linear trace validator. Owns a set of invariants and replays events
/// against a [`SchedulerState`].
///
/// The validator is reusable — every call to `validate_*` builds a fresh
/// state internally, so a single validator instance can vet many traces.
pub struct TraceValidator {
    invariants: Vec<InvariantBox>,
    skip_unknown: bool,
}

impl TraceValidator {
    /// Build a validator from a set of invariants. Use
    /// [`crate::baseline_invariants`] for the shipped baseline.
    #[must_use]
    pub fn new(invariants: Vec<InvariantBox>) -> Self {
        Self {
            invariants,
            skip_unknown: false,
        }
    }

    /// If `true`, unrecognised line shapes are silently skipped (and counted
    /// in `ValidationOutcome::Ok::skipped_unknown`) instead of failing the
    /// whole replay. This is the pragmatic mode for historical
    /// `.cosmon/state/events.jsonl` logs that pre-date the `EventV2` schema.
    /// Default: `false` (strict).
    #[must_use]
    pub fn with_skip_unknown(mut self, skip_unknown: bool) -> Self {
        self.skip_unknown = skip_unknown;
        self
    }

    /// Validate a trace stored as a string (useful for tests and stdin).
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::Parse`] if any non-empty line cannot be
    /// parsed as an `EventV2` envelope (legacy shapes are tolerated via
    /// [`Envelope::from_line`]). When [`Self::with_skip_unknown`] is `true`
    /// such lines are counted and skipped instead.
    pub fn validate_str(&self, trace: &str) -> Result<ValidationOutcome, ValidationError> {
        let mut state = SchedulerState::new();
        let mut replayed: u64 = 0;
        let mut skipped_unknown: u64 = 0;

        for (idx, raw) in trace.lines().enumerate() {
            let line_no = idx + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let envelope = match Envelope::from_line(trimmed) {
                Ok(env) => env,
                Err(source) => {
                    if self.skip_unknown {
                        skipped_unknown += 1;
                        continue;
                    }
                    return Err(ValidationError::Parse {
                        line: line_no,
                        source,
                    });
                }
            };

            if let Err(v) = self.check_all(&state, &envelope, line_no) {
                return Ok(ValidationOutcome::Violation {
                    events_replayed_before: replayed,
                    violation: v,
                });
            }
            apply(&mut state, &envelope);
            replayed += 1;
        }

        Ok(ValidationOutcome::Ok {
            events_replayed: replayed,
            molecules_seen: state.molecule_count() as u64,
            skipped_unknown,
        })
    }

    /// Validate a trace stored on disk.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::Io`] if the file cannot be read, or
    /// [`ValidationError::Parse`] as in [`Self::validate_str`].
    pub fn validate_path(&self, path: &Path) -> Result<ValidationOutcome, ValidationError> {
        let contents = fs::read_to_string(path)?;
        self.validate_str(&contents)
    }

    fn check_all(
        &self,
        state: &SchedulerState,
        envelope: &Envelope,
        line: usize,
    ) -> Result<(), Violation> {
        for inv in &self.invariants {
            inv.check(state, envelope, line)?;
        }
        Ok(())
    }
}

/// Apply an event to the state. Kept private so only the replay loop can
/// mutate the scheduler projection — invariants see an immutable snapshot.
fn apply(state: &mut SchedulerState, envelope: &Envelope) {
    state.events_seen += 1;
    match &envelope.event {
        EventV2::MoleculeNucleated {
            molecule_id,
            formula_id,
            ..
        } => {
            state
                .molecules
                .entry(molecule_id.clone())
                .or_insert_with(|| MoleculeTraceState {
                    status: MoleculeStatus::Pending,
                    last_step: None,
                    total_steps: None,
                    formula_id: formula_id.clone(),
                });
        }
        EventV2::MoleculeStatusChanged {
            molecule_id, to, ..
        } => {
            if let Some(mol) = state.molecules.get_mut(molecule_id) {
                if let Ok(parsed) = to.parse::<MoleculeStatus>() {
                    mol.status = parsed;
                }
            }
        }
        EventV2::MoleculeStepCompleted {
            molecule_id,
            step,
            total,
            ..
        } => {
            if let Some(mol) = state.molecules.get_mut(molecule_id) {
                mol.last_step = Some(*step);
                mol.total_steps = Some(*total);
            }
        }
        EventV2::MoleculeCompleted { molecule_id, .. } => {
            if let Some(mol) = state.molecules.get_mut(molecule_id) {
                mol.status = MoleculeStatus::Completed;
            }
        }
        EventV2::MoleculeCollapsed { molecule_id, .. } => {
            if let Some(mol) = state.molecules.get_mut(molecule_id) {
                mol.status = MoleculeStatus::Collapsed;
            }
        }
        EventV2::WorkerSpawned { worker_id, .. } => {
            state
                .workers
                .entry(worker_id.clone())
                .or_insert(WorkerTraceState { alive: true });
        }
        EventV2::WorkerKilled { worker_id, .. } => {
            if let Some(w) = state.workers.get_mut(worker_id) {
                w.alive = false;
            }
        }
        EventV2::MergeDispatched {
            molecule, branch, ..
        } => {
            state
                .pending_merges
                .insert(molecule.clone(), branch.clone());
        }
        EventV2::MergeCompleted {
            molecule, result, ..
        } => {
            state.pending_merges.remove(molecule);
            state
                .completed_merges
                .insert(molecule.clone(), result.clone());
        }
        EventV2::DecaySpliced { children, .. } => {
            // Children become visible to MoleculeExistsBeforeUse as soon as
            // a DecaySpliced is observed — the splice itself is the creation
            // event for the children from the trace's perspective.
            for child in children {
                state
                    .molecules
                    .entry(child.clone())
                    .or_insert_with(|| MoleculeTraceState {
                        status: MoleculeStatus::Pending,
                        last_step: None,
                        total_steps: None,
                        formula_id: String::new(),
                    });
            }
        }
        // Remaining variants are observability-only — they update counters
        // implicitly via `events_seen` but carry no state projection.
        _ => {}
    }

    // Explicit use of MergeResult to avoid unused-import when features toggle.
    let _: Option<&MergeResult> = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invariants::baseline_invariants;

    fn validator() -> TraceValidator {
        TraceValidator::new(baseline_invariants())
    }

    const NUCLEATE_LINE: &str = r#"{"seq":0,"timestamp":"2026-04-14T10:00:00Z","type":"molecule_nucleated","molecule_id":"cs-20260414-aaaa","formula_id":"task-work"}"#;

    #[test]
    fn empty_trace_is_ok() {
        let outcome = validator().validate_str("").unwrap();
        assert!(outcome.is_ok());
    }

    #[test]
    fn blank_lines_are_skipped() {
        let trace = format!("\n{NUCLEATE_LINE}\n\n");
        let outcome = validator().validate_str(&trace).unwrap();
        match outcome {
            ValidationOutcome::Ok {
                events_replayed, ..
            } => assert_eq!(events_replayed, 1),
            other @ ValidationOutcome::Violation { .. } => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn nucleate_then_status_change_is_ok() {
        let trace = format!(
            "{NUCLEATE_LINE}\n{}",
            r#"{"seq":1,"timestamp":"2026-04-14T10:00:01Z","type":"molecule_status_changed","molecule_id":"cs-20260414-aaaa","from":"pending","to":"running"}"#
        );
        assert!(validator().validate_str(&trace).unwrap().is_ok());
    }

    #[test]
    fn completed_without_nucleate_is_rejected() {
        let trace = r#"{"seq":0,"timestamp":"2026-04-14T10:00:00Z","type":"molecule_completed","molecule_id":"cs-20260414-aaaa","reason":"ok"}"#;
        let outcome = validator().validate_str(trace).unwrap();
        match outcome {
            ValidationOutcome::Violation { violation, .. } => {
                assert_eq!(violation.invariant, "molecule_exists_before_use");
            }
            other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
        }
    }

    #[test]
    fn illegal_status_transition_is_rejected() {
        // Pending → Completed is not in `can_transition_to`.
        let trace = format!(
            "{NUCLEATE_LINE}\n{}",
            r#"{"seq":1,"timestamp":"2026-04-14T10:00:01Z","type":"molecule_status_changed","molecule_id":"cs-20260414-aaaa","from":"pending","to":"completed"}"#
        );
        let outcome = validator().validate_str(&trace).unwrap();
        match outcome {
            ValidationOutcome::Violation { violation, .. } => {
                assert_eq!(violation.invariant, "status_transition_legal");
            }
            other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
        }
    }

    #[test]
    fn step_regression_is_rejected() {
        let trace = format!(
            "{NUCLEATE_LINE}\n{}\n{}\n{}",
            r#"{"seq":1,"timestamp":"2026-04-14T10:00:01Z","type":"molecule_status_changed","molecule_id":"cs-20260414-aaaa","from":"pending","to":"running"}"#,
            r#"{"seq":2,"timestamp":"2026-04-14T10:00:02Z","type":"molecule_step_completed","molecule_id":"cs-20260414-aaaa","step":2,"total":5}"#,
            r#"{"seq":3,"timestamp":"2026-04-14T10:00:03Z","type":"molecule_step_completed","molecule_id":"cs-20260414-aaaa","step":1,"total":5}"#
        );
        let outcome = validator().validate_str(&trace).unwrap();
        match outcome {
            ValidationOutcome::Violation { violation, .. } => {
                assert_eq!(violation.invariant, "step_monotone");
            }
            other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
        }
    }

    #[test]
    fn step_beyond_total_is_rejected() {
        let trace = format!(
            "{NUCLEATE_LINE}\n{}\n{}",
            r#"{"seq":1,"timestamp":"2026-04-14T10:00:01Z","type":"molecule_status_changed","molecule_id":"cs-20260414-aaaa","from":"pending","to":"running"}"#,
            r#"{"seq":2,"timestamp":"2026-04-14T10:00:02Z","type":"molecule_step_completed","molecule_id":"cs-20260414-aaaa","step":5,"total":5}"#
        );
        let outcome = validator().validate_str(&trace).unwrap();
        match outcome {
            ValidationOutcome::Violation { violation, .. } => {
                assert_eq!(violation.invariant, "step_within_total");
            }
            other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
        }
    }

    #[test]
    fn worker_killed_without_spawn_is_rejected() {
        let trace = r#"{"seq":0,"timestamp":"2026-04-14T10:00:00Z","type":"worker_killed","worker_id":"quartz","reason":"purge"}"#;
        let outcome = validator().validate_str(trace).unwrap();
        match outcome {
            ValidationOutcome::Violation { violation, .. } => {
                assert_eq!(violation.invariant, "worker_spawned_before_killed");
            }
            other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
        }
    }

    #[test]
    fn events_after_completed_are_rejected() {
        let trace = format!(
            "{NUCLEATE_LINE}\n{}\n{}\n{}",
            r#"{"seq":1,"timestamp":"2026-04-14T10:00:01Z","type":"molecule_status_changed","molecule_id":"cs-20260414-aaaa","from":"pending","to":"running"}"#,
            r#"{"seq":2,"timestamp":"2026-04-14T10:00:02Z","type":"molecule_completed","molecule_id":"cs-20260414-aaaa","reason":"ok"}"#,
            r#"{"seq":3,"timestamp":"2026-04-14T10:00:03Z","type":"molecule_step_completed","molecule_id":"cs-20260414-aaaa","step":0,"total":2}"#
        );
        let outcome = validator().validate_str(&trace).unwrap();
        match outcome {
            ValidationOutcome::Violation { violation, .. } => {
                assert_eq!(violation.invariant, "no_events_after_terminal");
            }
            other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
        }
    }

    #[test]
    fn merge_completed_without_dispatch_is_rejected() {
        let trace = format!(
            "{NUCLEATE_LINE}\n{}",
            r#"{"seq":1,"timestamp":"2026-04-14T10:00:01Z","type":"merge_completed","molecule":"cs-20260414-aaaa","branch":"feat/x","result":"ok"}"#
        );
        let outcome = validator().validate_str(&trace).unwrap();
        match outcome {
            ValidationOutcome::Violation { violation, .. } => {
                assert_eq!(violation.invariant, "merge_completion_pairs_dispatch");
            }
            other @ ValidationOutcome::Ok { .. } => panic!("expected violation, got {other:?}"),
        }
    }

    #[test]
    #[allow(clippy::match_wildcard_for_single_variants)]
    fn parse_error_on_bad_line() {
        let trace = "this is not json\n";
        match validator().validate_str(trace) {
            Err(ValidationError::Parse { line, .. }) => assert_eq!(line, 1),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn skip_unknown_tolerates_drifted_shapes() {
        // A legacy pre-EventV2 line using the `kind` discriminator with a
        // shape the migration helper does not support. Strict mode rejects;
        // skip_unknown mode counts it as skipped and proceeds.
        let legacy = r#"{"timestamp":"2026-04-08T05:50:35.500634Z","kind":"molecule_transitioned","molecule_id":"cs-20260408-7272","from":"running","to":"completed"}"#;
        let trace = format!("{NUCLEATE_LINE}\n{legacy}");

        assert!(validator().validate_str(&trace).is_err());

        let lenient = TraceValidator::new(baseline_invariants()).with_skip_unknown(true);
        match lenient.validate_str(&trace).unwrap() {
            ValidationOutcome::Ok {
                events_replayed,
                skipped_unknown,
                ..
            } => {
                assert_eq!(events_replayed, 1);
                assert_eq!(skipped_unknown, 1);
            }
            other @ ValidationOutcome::Violation { .. } => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn validates_path_from_disk() {
        use std::io::Write;
        use tempfile::NamedTempFile;
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "{NUCLEATE_LINE}").unwrap();
        let outcome = validator().validate_path(f.path()).unwrap();
        assert!(outcome.is_ok());
    }
}
