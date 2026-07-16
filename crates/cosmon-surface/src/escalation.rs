// SPDX-License-Identifier: AGPL-3.0-only

//! Mechanical-first escalation for surface projection.
//!
//! Mechanical-first escalation: see docs/architectural-invariants.md.
//!
//! `cs reconcile` projects internal state onto surface files (STATUS.md,
//! ISSUES.md, …). Between two projections, three kinds of drift can occur:
//!
//! 1. The source state changed (a molecule was tackled, evolved, completed).
//! 2. The surface file was hand-edited by a human.
//! 3. Both changed at the same time (true 3-way conflict).
//!
//! The mechanical-first discipline says: try to resolve the drift
//! deterministically without engaging a cognitive worker. Escalate only
//! when the drift cannot be resolved mechanically — the same principle
//! that governs `cs done`'s merge loop (see [`cmd::done`]).
//!
//! | Divergence         | Decision                                       |
//! |--------------------|------------------------------------------------|
//! | `UpToDate`         | [`SurfaceDecision::Write`] (no-op in practice) |
//! | `NeverProjected`   | [`SurfaceDecision::Write`]                     |
//! | `SourceChanged`    | [`SurfaceDecision::Write`] (mechanical)        |
//! | `SurfaceEdited`    | [`SurfaceDecision::Preserve`] (human wins)     |
//! | `Conflict`         | [`SurfaceDecision::Escalate { .. }`]           |
//!
//! Only the `Conflict` case reaches for cognitive resolution; everything
//! else stays in the transactional core.
//!
//! [`cmd::done`]: ../../cosmon_cli/cmd/done/index.html

use crate::snapshot::{detect_divergence, SurfaceDivergence};

/// Classification of a single surface after comparing snapshot, current
/// file content, and freshly rendered content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceDecision {
    /// Safe to overwrite the surface with the newly rendered content.
    ///
    /// Covers `UpToDate`, `NeverProjected`, and `SourceChanged` — either
    /// nothing changed, the file does not exist yet, or only the source
    /// changed while the file stayed as last projected.
    Write,
    /// Surface was edited by a human while the source was stable. The
    /// human edit wins; skip overwriting.
    Preserve,
    /// Both the source and the surface changed since the last projection.
    /// Mechanical merge is not safe — cognitive resolution is required.
    ///
    /// Carries both sides so the caller can either (a) nucleate a resolver
    /// molecule, or (b) fall back to writing a conflict block.
    Escalate {
        /// Human-edited content currently on disk.
        human_content: String,
        /// Content that would be written by a fresh mechanical projection.
        source_content: String,
    },
}

impl SurfaceDecision {
    /// Whether this decision will mutate the surface file.
    #[must_use]
    pub fn writes_file(&self) -> bool {
        matches!(self, Self::Write)
    }

    /// Whether this decision requires cognitive escalation.
    #[must_use]
    pub fn needs_escalation(&self) -> bool {
        matches!(self, Self::Escalate { .. })
    }
}

/// Classify a single surface given its snapshot hash, current on-disk
/// content, and the fresh mechanically-rendered content.
///
/// This is the single place where [`SurfaceDivergence`] is translated into
/// an actionable decision. Keeping it pure makes the escalation behaviour
/// testable without disk or subprocess plumbing.
#[must_use]
pub fn classify_surface(
    snapshot_hash: Option<&str>,
    current_file: &str,
    new_rendered: &str,
) -> SurfaceDecision {
    match detect_divergence(snapshot_hash, current_file, new_rendered) {
        SurfaceDivergence::UpToDate
        | SurfaceDivergence::SourceChanged
        | SurfaceDivergence::NeverProjected => SurfaceDecision::Write,
        SurfaceDivergence::SurfaceEdited => SurfaceDecision::Preserve,
        SurfaceDivergence::Conflict => SurfaceDecision::Escalate {
            human_content: current_file.to_owned(),
            source_content: new_rendered.to_owned(),
        },
    }
}

/// Format a git-style conflict block — the fallback written to the surface
/// file when no cognitive resolver is available (`--no-escalate`, or after
/// retries are exhausted).
///
/// The block uses the same marker syntax as `git merge`'s conflict output,
/// so any tool that understands conflict markers (editors, pre-commit hooks,
/// CI gates) will flag the file automatically.
#[must_use]
pub fn format_conflict_block(human_content: &str, source_content: &str) -> String {
    let human = human_content.trim_end_matches('\n');
    let source = source_content.trim_end_matches('\n');
    format!("<<<<<<< human (surface edit)\n{human}\n=======\n{source}\n>>>>>>> source (cs state)\n")
}

/// Summary of how the escalation module classified a batch of surfaces.
///
/// Produced by iterating over surfaces and calling [`classify_surface`] on
/// each. Kept as a plain aggregate so the CLI can format it for JSON or
/// human output without needing to re-walk the surfaces.
#[derive(Debug, Default)]
pub struct ClassificationSummary {
    /// Surface paths safe to overwrite.
    pub writeable: Vec<String>,
    /// Surface paths preserving human edits.
    pub preserved: Vec<String>,
    /// Surface paths in true 3-way conflict.
    pub conflicted: Vec<ConflictRecord>,
}

/// Per-surface conflict record captured during classification, suitable for
/// persisting in the molecule audit trail or for briefing a resolver.
#[derive(Debug, Clone)]
pub struct ConflictRecord {
    /// Path of the conflicted surface (relative to the project root).
    pub path: String,
    /// Human-edited content currently on disk.
    pub human_content: String,
    /// Content that would be written by a fresh mechanical projection.
    pub source_content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hash(s: &str) -> String {
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        format!("{:x}", h.finalize())
    }

    #[test]
    fn test_classify_up_to_date_writes_identity() {
        let content = "# Status\nSame.";
        let h = hash(content);
        assert_eq!(
            classify_surface(Some(&h), content, content),
            SurfaceDecision::Write
        );
    }

    #[test]
    fn test_classify_never_projected_writes() {
        assert_eq!(
            classify_surface(None, "", "# Status\nFirst run."),
            SurfaceDecision::Write
        );
    }

    #[test]
    fn test_classify_source_changed_writes_mechanically() {
        let old = "# Status\nOld.";
        let h = hash(old);
        let new = "# Status\nNew.";
        assert_eq!(classify_surface(Some(&h), old, new), SurfaceDecision::Write);
    }

    #[test]
    fn test_classify_surface_edited_preserves_human() {
        let original = "# Status\nOriginal.";
        let h = hash(original);
        let edited = "# Status\nOriginal.\n\n## Human note";
        assert_eq!(
            classify_surface(Some(&h), edited, original),
            SurfaceDecision::Preserve
        );
    }

    #[test]
    fn test_classify_true_conflict_escalates() {
        let original = "# Status\nOriginal.";
        let h = hash(original);
        let edited = "# Status\nHuman wrote this.";
        let fresh = "# Status\nSource changed too.";
        let decision = classify_surface(Some(&h), edited, fresh);
        match decision {
            SurfaceDecision::Escalate {
                human_content,
                source_content,
            } => {
                assert_eq!(human_content, edited);
                assert_eq!(source_content, fresh);
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
    }

    #[test]
    fn test_decision_predicates() {
        assert!(SurfaceDecision::Write.writes_file());
        assert!(!SurfaceDecision::Write.needs_escalation());
        assert!(!SurfaceDecision::Preserve.writes_file());
        assert!(!SurfaceDecision::Preserve.needs_escalation());
        let esc = SurfaceDecision::Escalate {
            human_content: "a".into(),
            source_content: "b".into(),
        };
        assert!(!esc.writes_file());
        assert!(esc.needs_escalation());
    }

    #[test]
    fn test_format_conflict_block_has_git_markers() {
        let block = format_conflict_block("human line", "source line");
        assert!(block.starts_with("<<<<<<< human"));
        assert!(block.contains("\n=======\n"));
        assert!(block.trim_end().ends_with(">>>>>>> source (cs state)"));
        assert!(block.contains("human line"));
        assert!(block.contains("source line"));
    }

    #[test]
    fn test_format_conflict_block_strips_trailing_newlines() {
        // Both sides have trailing newlines — the block must not double them
        // (so downstream diff tools get clean markers).
        let block = format_conflict_block("a\n", "b\n\n");
        assert!(!block.contains("a\n\n=======\n"));
        assert!(!block.contains("b\n\n>>>>>>>"));
        assert!(block.contains("a\n=======\nb\n"));
    }
}
