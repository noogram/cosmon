// SPDX-License-Identifier: AGPL-3.0-only

//! Monotone, provenance-bearing task criticality (ADR-148).
//!
//! Declarations are immutable ledger facts.  The effective criticality is a
//! fold (the maximum level), never a mutable field that a worker can lower.
//! Tags and formula variables are deliberately treated as projections only.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Ordered review stakes.  The declaration fold uses this order verbatim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticalityLevel {
    /// Ordinary work; no adversarial committee is required.
    #[default]
    Routine,
    /// Root-cause or performance claim requiring cross-provider refutation.
    Root,
    /// Security-sensitive work.
    Security,
    /// Maximum assurance requested by an operator or policy baseline.
    Max,
}

impl CriticalityLevel {
    /// Canonical derived tag for this level.
    #[must_use]
    pub const fn stake_tag(self) -> &'static str {
        match self {
            Self::Routine => "stake:routine",
            Self::Root => "stake:root",
            Self::Security => "stake:security",
            Self::Max => "stake:max",
        }
    }

    /// Whether this level triggers adversarial cross-provider review.
    #[must_use]
    pub const fn requires_committee(self) -> bool {
        !matches!(self, Self::Routine)
    }
}

/// Where a declaration originated.  This is attribution, not authority:
/// every source may raise the fold and none may subtract from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticalitySource {
    /// An exogenous project/fleet baseline evaluated before dispatch.
    Baseline,
    /// A human operator declaration.
    Operator,
    /// A formula requirement.
    Formula,
    /// An automated rule or classifier.
    Policy,
    /// A worker declaration (permitted to raise its own assurance floor).
    Worker,
}

/// One immutable criticality declaration with complete provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticalityDeclaration {
    /// Stable subject identifier (normally a molecule id).
    pub subject: String,
    /// Revision of the subject to which this fact applies.
    pub revision: String,
    /// Assurance floor asserted by this fact.
    pub level: CriticalityLevel,
    /// Class of declarer.
    pub source: CriticalitySource,
    /// Stable actor identity or policy id.
    pub actor: String,
    /// Human-auditable reason for the assertion.
    pub reason: String,
    /// Ledger observation time.
    pub declared_at: DateTime<Utc>,
}

/// A declaration that tried to lower the already-effective floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DowngradeAttempt<'a> {
    /// Effective level immediately before the attempted downgrade.
    pub floor: CriticalityLevel,
    /// The retained declaration (it remains evidence even though ineffective).
    pub declaration: &'a CriticalityDeclaration,
}

/// Result of folding immutable declarations for one subject revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveCriticality<'a> {
    /// Maximum of all matching declarations.
    pub level: CriticalityLevel,
    /// All declarations tied at the effective maximum (provenance is not lost).
    pub decisive: Vec<&'a CriticalityDeclaration>,
    /// Lower declarations observed after a higher floor was established.
    pub downgrade_attempts: Vec<DowngradeAttempt<'a>>,
}

/// Fold declarations monotonically for `subject` at `revision`.
#[must_use]
pub fn effective<'a>(
    declarations: &'a [CriticalityDeclaration],
    subject: &str,
    revision: &str,
) -> EffectiveCriticality<'a> {
    let mut level = CriticalityLevel::Routine;
    let mut decisive = Vec::new();
    let mut downgrade_attempts = Vec::new();
    for declaration in declarations
        .iter()
        .filter(|d| d.subject == subject && d.revision == revision)
    {
        match declaration.level.cmp(&level) {
            std::cmp::Ordering::Greater => {
                level = declaration.level;
                decisive.clear();
                decisive.push(declaration);
            }
            std::cmp::Ordering::Equal => decisive.push(declaration),
            std::cmp::Ordering::Less => downgrade_attempts.push(DowngradeAttempt {
                floor: level,
                declaration,
            }),
        }
    }
    EffectiveCriticality {
        level,
        decisive,
        downgrade_attempts,
    }
}

/// Drift visible on projected surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriticalityDrift {
    /// `stake:*` differs from the ledger fold.
    StakeTag,
    /// Formula variable `stake` is lower than the ledger fold.
    FormulaStake,
    /// Policy expected classification but no declaration exists.
    UnclassifiedExpectedCriticality,
    /// Effective criticality requires a committee but none is linked.
    MissingCommittee,
}

/// Compare derived surfaces with the authoritative declaration fold.
#[must_use]
pub fn projection_drift(
    effective: &EffectiveCriticality<'_>,
    stake_tag: Option<&str>,
    formula_stake: Option<CriticalityLevel>,
    classification_expected: bool,
    committee_linked: bool,
) -> Vec<CriticalityDrift> {
    let mut drift = Vec::new();
    if stake_tag != Some(effective.level.stake_tag()) {
        drift.push(CriticalityDrift::StakeTag);
    }
    if formula_stake.is_some_and(|level| level < effective.level) {
        drift.push(CriticalityDrift::FormulaStake);
    }
    if classification_expected && effective.decisive.is_empty() {
        drift.push(CriticalityDrift::UnclassifiedExpectedCriticality);
    }
    if effective.level.requires_committee() && !committee_linked {
        drift.push(CriticalityDrift::MissingCommittee);
    }
    drift
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration(level: CriticalityLevel, actor: &str) -> CriticalityDeclaration {
        CriticalityDeclaration {
            subject: "task-1".into(),
            revision: "abc123".into(),
            level,
            source: CriticalitySource::Policy,
            actor: actor.into(),
            reason: "test".into(),
            declared_at: Utc::now(),
        }
    }

    #[test]
    fn fold_is_monotone_and_retains_downgrade_provenance() {
        let facts = vec![
            declaration(CriticalityLevel::Security, "baseline"),
            declaration(CriticalityLevel::Routine, "audited-worker"),
            declaration(CriticalityLevel::Max, "operator"),
        ];
        let folded = effective(&facts, "task-1", "abc123");
        assert_eq!(folded.level, CriticalityLevel::Max);
        assert_eq!(folded.decisive[0].actor, "operator");
        assert_eq!(folded.downgrade_attempts.len(), 1);
        assert_eq!(
            folded.downgrade_attempts[0].declaration.actor,
            "audited-worker"
        );
    }

    #[test]
    fn projections_are_not_sources_of_truth() {
        let facts = vec![declaration(CriticalityLevel::Security, "operator")];
        let folded = effective(&facts, "task-1", "abc123");
        let drift = projection_drift(
            &folded,
            Some("stake:routine"),
            Some(CriticalityLevel::Root),
            true,
            false,
        );
        assert_eq!(
            drift,
            vec![
                CriticalityDrift::StakeTag,
                CriticalityDrift::FormulaStake,
                CriticalityDrift::MissingCommittee,
            ]
        );
    }

    #[test]
    fn revision_scopes_declarations() {
        let facts = vec![declaration(CriticalityLevel::Max, "old-policy")];
        let folded = effective(&facts, "task-1", "new-revision");
        assert_eq!(folded.level, CriticalityLevel::Routine);
        assert!(folded.decisive.is_empty());
    }
}
