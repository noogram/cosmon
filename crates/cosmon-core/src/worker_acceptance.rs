// SPDX-License-Identifier: AGPL-3.0-only

//! Pure acceptance decisions for an owned agent-loop turn.
//!
//! Provider termination is transport evidence, not lifecycle authority.  This
//! module combines a normal terminal response with the *current formula step's*
//! declared deliverable so callers can decide whether that one step may
//! advance.  Filesystem publication and verification remain injected CLI
//! effects.

use std::path::{Component, Path};

/// The evidence class that authorizes one worker-step transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptanceEvidence {
    /// Non-empty final text will be published at the declared relative path.
    ResponseArtifact,
    /// Every artifact declared by the current step was freshly observed.
    DeclaredArtifacts,
    /// A legacy code step changed its worktree during this turn.
    LegacyWorktreeChange,
}

/// Provider-neutral termination evidence presented to acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationEvidence {
    /// The provider reported ordinary completion.
    Normal,
    /// The provider stopped for any non-success disposition.
    NonNormal,
}

/// Observation of the current step's declared file artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaredArtifactEvidence {
    /// The current step declares no file artifacts.
    NotDeclared,
    /// Every current-step declaration is fresh and satisfied.
    Satisfied,
    /// At least one current-step declaration is absent, stale, empty, or unsafe.
    Missing,
}

/// Turn-scoped observation of the worker's worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeEvidence {
    /// The worktree changed during this turn.
    Changed,
    /// No worktree change was observed during this turn.
    Unchanged,
}

/// Why a completed provider turn cannot advance its formula step.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcceptanceError {
    /// Only ordinary provider termination is eligible for acceptance.
    #[error("provider response did not terminate normally")]
    NonNormalTermination,
    /// A declared text result must contain payload bytes.
    #[error("step declares response_artifact but the final response is empty")]
    EmptyResponseArtifact,
    /// A current-step artifact declaration was not satisfied by this turn.
    #[error("current step is missing fresh declared artifacts")]
    MissingDeclaredArtifacts,
    /// Legacy formulas need a concrete worktree witness rather than tool use.
    #[error(
        "legacy worker step has no declared deliverable and made no worktree change; declare response_artifact for final-text output"
    )]
    AmbiguousLegacyOutput,
}

/// Decide which formula-bound evidence permits the current step to advance.
///
/// `response_artifact` is explicit and therefore takes precedence over the
/// legacy worktree rule. Artifact declarations are checked only for the
/// current step; an empty list never implies that the whole formula is done.
///
/// # Errors
///
/// Returns [`AcceptanceError`] when termination is not normal or the current
/// step's explicitly selected evidence class is absent or empty.
pub fn decide(
    termination: TerminationEvidence,
    response_artifact: Option<&str>,
    response_text: &str,
    declared_artifacts: DeclaredArtifactEvidence,
    worktree: WorktreeEvidence,
) -> Result<AcceptanceEvidence, AcceptanceError> {
    if termination != TerminationEvidence::Normal {
        return Err(AcceptanceError::NonNormalTermination);
    }
    if response_artifact.is_some() {
        return if response_text.trim().is_empty() {
            Err(AcceptanceError::EmptyResponseArtifact)
        } else {
            Ok(AcceptanceEvidence::ResponseArtifact)
        };
    }
    if declared_artifacts != DeclaredArtifactEvidence::NotDeclared {
        return if declared_artifacts == DeclaredArtifactEvidence::Satisfied {
            Ok(AcceptanceEvidence::DeclaredArtifacts)
        } else {
            Err(AcceptanceError::MissingDeclaredArtifacts)
        };
    }
    if worktree == WorktreeEvidence::Changed {
        Ok(AcceptanceEvidence::LegacyWorktreeChange)
    } else {
        Err(AcceptanceError::AmbiguousLegacyOutput)
    }
}

/// Validate a response-artifact destination without touching the filesystem.
///
/// The accepted grammar is a non-empty relative path made only of normal path
/// components. The CLI repeats this check at publication and then refuses
/// symlinked ancestors, because admission-time lexical safety and runtime
/// filesystem authority are separate boundaries.
#[must_use]
pub fn validate_response_artifact_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_text_accepts_without_tools_or_worktree_change() {
        assert_eq!(
            decide(
                TerminationEvidence::Normal,
                Some("result.md"),
                "answer",
                DeclaredArtifactEvidence::NotDeclared,
                WorktreeEvidence::Unchanged,
            ),
            Ok(AcceptanceEvidence::ResponseArtifact)
        );
    }

    #[test]
    fn non_normal_termination_never_advances_even_with_complete_evidence() {
        // An output-limited or refused response must not be accepted, even when
        // the step's declared deliverable looks present (issue #151, ADR-184).
        assert_eq!(
            decide(
                TerminationEvidence::NonNormal,
                Some("result.md"),
                "partial answer",
                DeclaredArtifactEvidence::Satisfied,
                WorktreeEvidence::Changed,
            ),
            Err(AcceptanceError::NonNormalTermination)
        );
    }

    #[test]
    fn failed_or_read_only_legacy_turn_does_not_advance() {
        assert_eq!(
            decide(
                TerminationEvidence::Normal,
                None,
                "summary",
                DeclaredArtifactEvidence::NotDeclared,
                WorktreeEvidence::Unchanged,
            ),
            Err(AcceptanceError::AmbiguousLegacyOutput)
        );
    }

    #[test]
    fn response_paths_cannot_escape_custody() {
        assert!(validate_response_artifact_path("responses/result.md"));
        for invalid in ["", "/tmp/result.md", "../result.md", "a/../result.md"] {
            assert!(!validate_response_artifact_path(invalid), "{invalid}");
        }
    }
}
