// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule evolution — advancing a molecule through its formula steps.
//!
//! The evolve operation is the core lifecycle advancement: given a molecule's
//! current state and its formula definition, it validates evidence, advances
//! to the next step (or completes the molecule), and produces artifacts
//! (log entries, briefing content) for persistence.

use chrono::{DateTime, Utc};

use crate::formula::{Formula, Step};
use crate::id::StepId;
use crate::molecule::MoleculeStatus;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during molecule evolution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvolveError {
    /// The molecule is not in a runnable state.
    #[error("molecule is {status}, must be running to evolve")]
    NotRunnable {
        /// Current status of the molecule.
        status: MoleculeStatus,
    },

    /// No evidence was provided for completing the step.
    #[error("--evidence is required to evolve past step \"{step_id}\"")]
    MissingEvidence {
        /// The step that requires evidence.
        step_id: String,
    },

    /// The molecule's current step doesn't match any formula step.
    #[error("current step index {index} is out of range (formula has {total} steps)")]
    StepOutOfRange {
        /// The invalid step index.
        index: usize,
        /// Total steps in the formula.
        total: usize,
    },

    /// A dependency of the current step has not been completed.
    #[error("step \"{step}\" depends on \"{dependency}\" which is not yet completed")]
    UnmetDependency {
        /// The step that has the unmet dependency.
        step: String,
        /// The dependency that hasn't been completed.
        dependency: String,
    },
}

// ---------------------------------------------------------------------------
// Request / Result types
// ---------------------------------------------------------------------------

/// Input for the evolve operation.
#[derive(Debug, Clone)]
pub struct EvolveRequest {
    /// Evidence documenting why the current step is complete.
    pub evidence: String,
    /// Timestamp for the evolution event.
    pub timestamp: DateTime<Utc>,
}

/// Outcome of a successful evolution.
#[derive(Debug, Clone)]
pub struct EvolveOutcome {
    /// The step that was just completed.
    pub completed_step: CompletedStepInfo,
    /// The new state after evolution.
    pub new_state: NewState,
    /// Warnings (e.g., exit criteria not mentioned in evidence).
    pub warnings: Vec<String>,
    /// Markdown log entry to append to log.md.
    pub log_entry: String,
    /// Regenerated briefing.md content (None if molecule completed).
    pub briefing: Option<String>,
}

/// Info about the step that was just completed.
#[derive(Debug, Clone)]
pub struct CompletedStepInfo {
    /// Step ID.
    pub id: String,
    /// Step title.
    pub title: String,
    /// The step's index in the formula.
    pub index: usize,
}

/// The molecule's new state after evolution.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum NewState {
    /// Still active, now on this step.
    Active {
        /// New current step index.
        current_step: usize,
        /// The new current step's ID.
        step_id: String,
        /// The new current step's title.
        step_title: String,
    },
    /// All steps complete — molecule is now Completed.
    Completed,
}

// ---------------------------------------------------------------------------
// Core evolve logic
// ---------------------------------------------------------------------------

/// Validate and compute the evolution of a molecule to its next step.
///
/// This is a pure function: it takes the molecule's current state (as flat
/// data) and the formula, validates the request, and returns the outcome.
/// The caller is responsible for persisting the changes.
///
/// # Errors
///
/// Returns [`EvolveError`] if:
/// - The molecule is not active
/// - Evidence is missing
/// - The current step index is out of range
/// - A dependency of the current step hasn't been completed
pub fn evolve(
    status: MoleculeStatus,
    current_step: usize,
    completed_steps: &[StepId],
    formula: &Formula,
    request: &EvolveRequest,
) -> Result<EvolveOutcome, EvolveError> {
    // 1. Validate molecule is Running (or Queued — first evolve promotes to running).
    if !matches!(status, MoleculeStatus::Running | MoleculeStatus::Queued) {
        return Err(EvolveError::NotRunnable { status });
    }

    // 2. Validate step index is within range.
    if current_step >= formula.steps.len() {
        return Err(EvolveError::StepOutOfRange {
            index: current_step,
            total: formula.steps.len(),
        });
    }

    let step = &formula.steps[current_step];

    // 3. Validate evidence is provided.
    if request.evidence.trim().is_empty() {
        return Err(EvolveError::MissingEvidence {
            step_id: step.id.clone(),
        });
    }

    // 4. Validate dependencies are met.
    let completed_ids: Vec<&str> = completed_steps.iter().map(StepId::as_str).collect();
    for dep in &step.depends_on {
        if !completed_ids.contains(&dep.as_str()) {
            return Err(EvolveError::UnmetDependency {
                step: step.id.clone(),
                dependency: dep.clone(),
            });
        }
    }

    // 5. Check exit criteria against evidence (warning only).
    let mut warnings = Vec::new();
    if let Some(ref criteria) = step.exit_criteria {
        let evidence_lower = request.evidence.to_lowercase();
        let criteria_words: Vec<&str> = criteria.split_whitespace().collect();
        let significant_words: Vec<&&str> = criteria_words.iter().filter(|w| w.len() > 3).collect();

        if !significant_words.is_empty() {
            let matched = significant_words
                .iter()
                .any(|w| evidence_lower.contains(&w.to_lowercase()));
            if !matched {
                warnings.push(format!(
                    "exit criteria for step \"{}\" may not be addressed in evidence: \"{}\"",
                    step.id, criteria
                ));
            }
        }
    }

    // 6. Determine new state.
    let is_last_step = current_step + 1 >= formula.steps.len();
    let completed_step = CompletedStepInfo {
        id: step.id.clone(),
        title: step.title.clone(),
        index: current_step,
    };

    let new_state = if is_last_step {
        NewState::Completed
    } else {
        let next = &formula.steps[current_step + 1];
        NewState::Active {
            current_step: current_step + 1,
            step_id: next.id.clone(),
            step_title: next.title.clone(),
        }
    };

    // 7. Generate log entry.
    let log_entry = format_log_entry(step, &request.evidence, &request.timestamp, &new_state);

    // 8. Generate briefing (None if completed).
    let briefing = match &new_state {
        NewState::Completed => None,
        NewState::Active {
            current_step: idx, ..
        } => Some(format_briefing(&formula.steps[*idx], *idx, formula)),
    };

    Ok(EvolveOutcome {
        completed_step,
        new_state,
        warnings,
        log_entry,
        briefing,
    })
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_log_entry(
    step: &Step,
    evidence: &str,
    timestamp: &DateTime<Utc>,
    new_state: &NewState,
) -> String {
    use std::fmt::Write;

    let mut entry = String::new();
    let _ = writeln!(
        entry,
        "## Step {}: {} — COMPLETED",
        step.order + 1,
        step.title
    );
    let _ = writeln!(entry, "**Timestamp:** {timestamp}");
    let _ = writeln!(entry, "**Evidence:** {evidence}");

    match new_state {
        NewState::Active { step_title, .. } => {
            let _ = writeln!(entry, "**Next:** {step_title}");
        }
        NewState::Completed => {
            entry.push_str("**Status:** MOLECULE COMPLETED\n");
        }
    }
    entry.push('\n');
    entry
}

fn format_briefing(step: &Step, index: usize, formula: &Formula) -> String {
    use std::fmt::Write;

    let mut brief = String::new();
    brief.push_str("# Molecule Briefing\n\n");
    let _ = writeln!(
        brief,
        "## Current Step {} of {}: {}\n",
        index + 1,
        formula.steps.len(),
        step.title
    );

    if !step.description.is_empty() {
        brief.push_str(&step.description);
        brief.push_str("\n\n");
    }

    if let Some(ref criteria) = step.exit_criteria {
        let _ = write!(brief, "### Exit Criteria\n\n{criteria}\n\n");
    }

    if !step.depends_on.is_empty() {
        brief.push_str("### Dependencies\n\n");
        for dep in &step.depends_on {
            let _ = writeln!(brief, "- {dep}");
        }
        brief.push('\n');
    }

    if !step.skills.is_empty() {
        brief.push_str("### Skills\n\n");
        for skill in &step.skills {
            let _ = writeln!(brief, "- {skill}");
        }
        brief.push('\n');
    }

    brief
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_formula(steps_toml: &str) -> Formula {
        let toml = format!(
            r#"
formula = "test-formula"
version = 1
description = "Test formula"

{steps_toml}
"#
        );
        Formula::parse(&toml).unwrap()
    }

    fn linear_formula() -> Formula {
        make_formula(
            r#"
[[steps]]
id = "step-1"
title = "Load context"
description = "Initialize session."
acceptance = "Context loaded"

[[steps]]
id = "step-2"
title = "Implement"
description = "Do the work."
needs = ["step-1"]
acceptance = "Code compiles"

[[steps]]
id = "step-3"
title = "Submit"
description = "Ship it."
needs = ["step-2"]
acceptance = "Submitted to MQ"
"#,
        )
    }

    fn formula_with_deps() -> Formula {
        make_formula(
            r#"
[[steps]]
id = "setup"
title = "Setup"
description = "Initial setup."

[[steps]]
id = "build"
title = "Build"
description = "Compile."
needs = ["setup"]
acceptance = "Code compiles"

[[steps]]
id = "test"
title = "Test"
description = "Run tests."
needs = ["setup"]
acceptance = "Tests pass"

[[steps]]
id = "deploy"
title = "Deploy"
description = "Ship."
needs = ["build", "test"]
acceptance = "Deployed"
"#,
        )
    }

    fn req(evidence: &str) -> EvolveRequest {
        EvolveRequest {
            evidence: evidence.to_owned(),
            timestamp: Utc::now(),
        }
    }

    fn step_id(s: &str) -> StepId {
        StepId::new(s).unwrap()
    }

    #[test]
    fn test_evolve_simple_linear() {
        let formula = linear_formula();
        let result = evolve(
            MoleculeStatus::Running,
            0,
            &[],
            &formula,
            &req("Context loaded successfully"),
        )
        .unwrap();

        assert_eq!(result.completed_step.id, "step-1");
        assert_eq!(result.completed_step.index, 0);
        match &result.new_state {
            NewState::Active {
                current_step,
                step_id,
                ..
            } => {
                assert_eq!(*current_step, 1);
                assert_eq!(step_id, "step-2");
            }
            NewState::Completed => panic!("should still be active"),
        }
        assert!(result.briefing.is_some());
        assert!(result.log_entry.contains("COMPLETED"));
    }

    #[test]
    fn test_evolve_with_dependencies() {
        let formula = formula_with_deps();

        // Complete "setup" (step 0)
        let result = evolve(
            MoleculeStatus::Running,
            0,
            &[],
            &formula,
            &req("Setup complete"),
        )
        .unwrap();
        assert_eq!(result.completed_step.id, "setup");

        // Complete "build" (step 1) — depends on "setup" which is completed
        let result = evolve(
            MoleculeStatus::Running,
            1,
            &[step_id("setup")],
            &formula,
            &req("Code compiles cleanly"),
        )
        .unwrap();
        assert_eq!(result.completed_step.id, "build");

        // Complete "test" (step 2) — depends on "setup" which is completed
        let result = evolve(
            MoleculeStatus::Running,
            2,
            &[step_id("setup"), step_id("build")],
            &formula,
            &req("Tests pass"),
        )
        .unwrap();
        assert_eq!(result.completed_step.id, "test");
    }

    #[test]
    fn test_evolve_completes_molecule() {
        let formula = linear_formula();
        let completed = vec![step_id("step-1"), step_id("step-2")];
        let result = evolve(
            MoleculeStatus::Running,
            2,
            &completed,
            &formula,
            &req("Submitted to MQ successfully"),
        )
        .unwrap();

        assert_eq!(result.completed_step.id, "step-3");
        assert!(matches!(result.new_state, NewState::Completed));
        assert!(result.briefing.is_none());
        assert!(result.log_entry.contains("MOLECULE COMPLETED"));
    }

    #[test]
    fn test_evolve_without_evidence_errors() {
        let formula = linear_formula();
        let err = evolve(MoleculeStatus::Running, 0, &[], &formula, &req("")).unwrap_err();

        assert!(matches!(err, EvolveError::MissingEvidence { .. }));
        assert!(err.to_string().contains("step-1"));
    }

    #[test]
    fn test_evolve_on_completed_molecule_errors() {
        let formula = linear_formula();
        let err = evolve(
            MoleculeStatus::Completed,
            0,
            &[],
            &formula,
            &req("Some evidence"),
        )
        .unwrap_err();

        assert!(matches!(err, EvolveError::NotRunnable { .. }));
        assert!(
            err.to_string().contains("completed"),
            "error should mention status: {err}"
        );
    }

    #[test]
    fn test_evolve_on_frozen_molecule_errors() {
        let formula = linear_formula();
        let err = evolve(
            MoleculeStatus::Frozen,
            0,
            &[],
            &formula,
            &req("Some evidence"),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            EvolveError::NotRunnable {
                status: MoleculeStatus::Frozen,
            }
        ));
    }

    #[test]
    fn test_evolve_on_collapsed_molecule_errors() {
        let formula = linear_formula();
        let err = evolve(
            MoleculeStatus::Collapsed,
            0,
            &[],
            &formula,
            &req("Some evidence"),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            EvolveError::NotRunnable {
                status: MoleculeStatus::Collapsed,
            }
        ));
    }

    #[test]
    fn test_evolve_step_out_of_range() {
        let formula = linear_formula();
        let err = evolve(MoleculeStatus::Running, 99, &[], &formula, &req("Evidence")).unwrap_err();

        assert!(matches!(
            err,
            EvolveError::StepOutOfRange {
                index: 99,
                total: 3
            }
        ));
    }

    #[test]
    fn test_evolve_unmet_dependency() {
        let formula = formula_with_deps();
        // Try to complete "build" (step 1) without completing "setup"
        let err = evolve(
            MoleculeStatus::Running,
            1,
            &[], // no completed steps
            &formula,
            &req("Code compiles"),
        )
        .unwrap_err();

        assert!(matches!(err, EvolveError::UnmetDependency { .. }));
        assert!(err.to_string().contains("setup"));
    }

    #[test]
    fn test_evolve_warns_on_missing_exit_criteria() {
        let formula = linear_formula();
        // Step 1 has acceptance "Context loaded" — provide unrelated evidence
        let result = evolve(
            MoleculeStatus::Running,
            0,
            &[],
            &formula,
            &req("I did something completely unrelated"),
        )
        .unwrap();

        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].contains("exit criteria"));
    }

    #[test]
    fn test_evolve_no_warning_when_criteria_addressed() {
        let formula = linear_formula();
        // Step 1 acceptance is "Context loaded"
        let result = evolve(
            MoleculeStatus::Running,
            0,
            &[],
            &formula,
            &req("Context loaded and verified"),
        )
        .unwrap();

        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_evolve_whitespace_only_evidence_errors() {
        let formula = linear_formula();
        let err = evolve(MoleculeStatus::Running, 0, &[], &formula, &req("   \n\t  ")).unwrap_err();

        assert!(matches!(err, EvolveError::MissingEvidence { .. }));
    }

    #[test]
    fn test_log_entry_format() {
        let formula = linear_formula();
        let result = evolve(
            MoleculeStatus::Running,
            0,
            &[],
            &formula,
            &req("Context loaded"),
        )
        .unwrap();

        assert!(result
            .log_entry
            .contains("Step 1: Load context — COMPLETED"));
        assert!(result.log_entry.contains("**Evidence:** Context loaded"));
        assert!(result.log_entry.contains("**Next:** Implement"));
    }

    #[test]
    fn test_briefing_content() {
        let formula = linear_formula();
        let result = evolve(
            MoleculeStatus::Running,
            0,
            &[],
            &formula,
            &req("Context loaded"),
        )
        .unwrap();

        let briefing = result.briefing.unwrap();
        assert!(briefing.contains("Current Step 2 of 3: Implement"));
        assert!(briefing.contains("Do the work."));
        assert!(briefing.contains("Exit Criteria"));
        assert!(briefing.contains("Code compiles"));
    }
}
