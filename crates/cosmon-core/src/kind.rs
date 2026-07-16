// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule kind — the cognitive nature of a work unit.
//!
//! `MoleculeKind` classifies *what* a molecule represents, orthogonal to
//! the `Formula` which defines *how* it executes. An idea needs cognitive
//! work to evolve; a task needs concrete steps; a decision synthesizes
//! evidence. The kind determines which interactions are valid (decay,
//! merge, transform) and which surface it projects onto (IDEAS.md vs
//! ISSUES.md vs docs/adr/).
//!
//! See ADR-013 and THESIS.md Part V (Vocabulary Stack).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;

/// The cognitive nature of a molecule.
///
/// Kind is WHAT, Formula is HOW. Orthogonal axes.
///
/// # Interaction rules
///
/// | Kind | Can decay | Can merge | Valid transforms |
/// |------|-----------|-----------|------------------|
/// | Idea | yes → Task, Issue | yes | → Task, Decision, Issue |
/// | Task | no | yes → Decision | → Issue |
/// | Decision | no | no | (terminal kind) |
/// | Issue | yes → Task | yes | → Task |
/// | Signal | no | no | (ephemeral, no transforms) |
/// | Deliberation | yes → Task, Decision, Idea | no | → Decision |
/// | Constellation | no | no | (pattern artifact, no transforms) |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// The molecule-kind taxonomy grows (idea/task/decision/issue/signal/deliberation
// today; more will follow). `#[non_exhaustive]` so a new kind is not a breaking
// change for external matchers. cosmon-core's own matches stay exhaustive.
#[non_exhaustive]
pub enum MoleculeKind {
    /// Unstructured insight requiring cognitive work to evolve.
    /// Can decay into tasks or transform into a decision.
    Idea,
    /// Actionable work item with concrete steps.
    Task,
    /// Architecture decision — typically produced by merging evidence.
    /// Terminal kind: decisions don't transform further.
    Decision,
    /// A tracked problem requiring resolution.
    Issue,
    /// Ephemeral observation — zero steps, auto-completes after recording.
    /// Carries information (like a photon), then is absorbed.
    Signal,
    /// Structured multi-perspective panel deliberation.
    ///
    /// A deliberation frames a question, mobilizes a panel of expert
    /// personas (subagents) in parallel, and synthesizes convergences and
    /// divergences into actionable outcomes. Produces child molecules via
    /// decay (tasks/ideas) or a merged decision.
    Deliberation,
    /// Fil-rouge artifact — names a pattern connecting N existing molecules
    /// that the operator already sees.
    ///
    /// A constellation carries a single artifact (`constellation.md`) and
    /// emits typed [`Refines`](crate::interaction::MoleculeLink::Refines)
    /// edges to each cited molecule, making the semantic connection
    /// visible to the DAG and survivable across compaction. It is the
    /// *human-decides* counterpart of a deliberation: a deliberation
    /// assembles a panel to **discover** a pattern; a constellation
    /// **records** a pattern already seen.
    Constellation,
}

impl MoleculeKind {
    /// Can this kind decay into child molecules?
    #[must_use]
    pub fn can_decay(self) -> bool {
        matches!(self, Self::Idea | Self::Issue | Self::Deliberation)
    }

    /// Can this kind be a source in a merge operation?
    #[must_use]
    pub fn can_merge(self) -> bool {
        matches!(self, Self::Task | Self::Idea | Self::Issue)
    }

    /// Valid transform targets from this kind.
    #[must_use]
    pub fn valid_transforms(self) -> &'static [MoleculeKind] {
        match self {
            Self::Idea => &[Self::Task, Self::Decision, Self::Issue],
            Self::Issue => &[Self::Task],
            Self::Task => &[Self::Issue],
            Self::Deliberation => &[Self::Decision],
            Self::Signal | Self::Decision | Self::Constellation => &[],
        }
    }

    /// Can this kind transform into the target kind?
    #[must_use]
    pub fn can_transform_to(self, target: Self) -> bool {
        self.valid_transforms().contains(&target)
    }

    /// Emoji glyph for this kind — used in CLI output, docs, and commits.
    ///
    /// | Kind | Emoji | Recallic |
    /// |------|-------|----------|
    /// | Idea | 💡 | lightbulb = insight |
    /// | Task | 🔧 | wrench = actionable work |
    /// | Decision | 📐 | triangular ruler = architecture |
    /// | Issue | 🐛 | bug = tracked problem |
    /// | Signal | ⚡ | lightning = ephemeral |
    /// | Deliberation | 🧠 | brain = structured panel thinking |
    /// | Constellation | 🌌 | galaxy = fil-rouge across molecules |
    #[must_use]
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Idea => "💡",
            Self::Task => "🔧",
            Self::Decision => "📐",
            Self::Issue => "🐛",
            Self::Signal => "⚡",
            Self::Deliberation => "🧠",
            Self::Constellation => "🌌",
        }
    }

    /// The default surface referent for this kind.
    ///
    /// Used by `cosmon-surface` to determine which surface file
    /// a molecule of this kind projects onto.
    #[must_use]
    pub fn surface_referent(self) -> &'static str {
        match self {
            Self::Idea => "project.ideas",
            Self::Task | Self::Issue => "project.issues",
            Self::Decision => "project.decisions",
            Self::Signal => "project.status",
            Self::Deliberation => "project.deliberations",
            Self::Constellation => "project.constellations",
        }
    }
}

impl fmt::Display for MoleculeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idea => f.write_str("idea"),
            Self::Task => f.write_str("task"),
            Self::Decision => f.write_str("decision"),
            Self::Issue => f.write_str("issue"),
            Self::Signal => f.write_str("signal"),
            Self::Deliberation => f.write_str("deliberation"),
            Self::Constellation => f.write_str("constellation"),
        }
    }
}

impl FromStr for MoleculeKind {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idea" => Ok(Self::Idea),
            "task" => Ok(Self::Task),
            "decision" | "adr" => Ok(Self::Decision),
            "issue" | "bug" => Ok(Self::Issue),
            "signal" => Ok(Self::Signal),
            "deliberation" | "delib" | "think" => Ok(Self::Deliberation),
            "constellation" | "const" | "fil-rouge" => Ok(Self::Constellation),
            _ => Err(ParseEnumError {
                type_name: "MoleculeKind",
                value: s.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_roundtrip() {
        for kind in [
            MoleculeKind::Idea,
            MoleculeKind::Task,
            MoleculeKind::Decision,
            MoleculeKind::Issue,
            MoleculeKind::Signal,
            MoleculeKind::Deliberation,
            MoleculeKind::Constellation,
        ] {
            let s = kind.to_string();
            let parsed: MoleculeKind = s.parse().unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn test_constellation_parse_aliases() {
        assert_eq!(
            "constellation".parse::<MoleculeKind>().unwrap(),
            MoleculeKind::Constellation
        );
        assert_eq!(
            "const".parse::<MoleculeKind>().unwrap(),
            MoleculeKind::Constellation
        );
        assert_eq!(
            "fil-rouge".parse::<MoleculeKind>().unwrap(),
            MoleculeKind::Constellation
        );
    }

    #[test]
    fn test_constellation_display_and_emoji() {
        assert_eq!(MoleculeKind::Constellation.to_string(), "constellation");
        assert_eq!(MoleculeKind::Constellation.emoji(), "🌌");
    }

    #[test]
    fn test_constellation_is_terminal_kind() {
        assert!(!MoleculeKind::Constellation.can_decay());
        assert!(!MoleculeKind::Constellation.can_merge());
        assert!(MoleculeKind::Constellation.valid_transforms().is_empty());
    }

    #[test]
    fn test_constellation_surface_referent() {
        assert_eq!(
            MoleculeKind::Constellation.surface_referent(),
            "project.constellations"
        );
    }

    #[test]
    fn test_constellation_serde_roundtrip() {
        let kind = MoleculeKind::Constellation;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"constellation\"");
        let parsed: MoleculeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn test_deliberation_parse_aliases() {
        assert_eq!(
            "deliberation".parse::<MoleculeKind>().unwrap(),
            MoleculeKind::Deliberation
        );
        assert_eq!(
            "delib".parse::<MoleculeKind>().unwrap(),
            MoleculeKind::Deliberation
        );
        assert_eq!(
            "think".parse::<MoleculeKind>().unwrap(),
            MoleculeKind::Deliberation
        );
    }

    #[test]
    fn test_deliberation_display_and_emoji() {
        assert_eq!(MoleculeKind::Deliberation.to_string(), "deliberation");
        assert_eq!(MoleculeKind::Deliberation.emoji(), "🧠");
    }

    #[test]
    fn test_deliberation_serde_roundtrip() {
        let kind = MoleculeKind::Deliberation;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"deliberation\"");
        let parsed: MoleculeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn test_deliberation_can_decay_and_transform_to_decision() {
        assert!(MoleculeKind::Deliberation.can_decay());
        assert!(!MoleculeKind::Deliberation.can_merge());
        assert!(MoleculeKind::Deliberation.can_transform_to(MoleculeKind::Decision));
    }

    #[test]
    fn test_deliberation_surface_referent() {
        assert_eq!(
            MoleculeKind::Deliberation.surface_referent(),
            "project.deliberations"
        );
    }

    #[test]
    fn test_idea_can_decay_and_transform() {
        assert!(MoleculeKind::Idea.can_decay());
        assert!(MoleculeKind::Idea.can_transform_to(MoleculeKind::Task));
        assert!(MoleculeKind::Idea.can_transform_to(MoleculeKind::Decision));
    }

    #[test]
    fn test_decision_is_terminal() {
        assert!(!MoleculeKind::Decision.can_decay());
        assert!(!MoleculeKind::Decision.can_merge());
        assert!(MoleculeKind::Decision.valid_transforms().is_empty());
    }

    #[test]
    fn test_signal_is_ephemeral() {
        assert!(!MoleculeKind::Signal.can_decay());
        assert!(!MoleculeKind::Signal.can_merge());
        assert!(MoleculeKind::Signal.valid_transforms().is_empty());
    }

    #[test]
    fn test_serde_roundtrip() {
        let kind = MoleculeKind::Idea;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"idea\"");
        let parsed: MoleculeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }
}
