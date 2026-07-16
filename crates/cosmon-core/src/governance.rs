// SPDX-License-Identifier: AGPL-3.0-only

//! Governance tiers for bounded-context configuration.
//!
//! Each bounded context (rig) is assigned a [`GovernanceTier`] that determines
//! its branch strategy, review requirements, CI gates, and merge policy. Tiers
//! are configured declaratively in TOML and loaded at startup.
//!
//! See ADR-009 for the full rationale and tier definitions.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::governance::{GovernanceTier, GovernanceConfig, BoundedContextConfig};
//!
//! let mut config = GovernanceConfig::new();
//! config.contexts.insert("cosmon".into(), BoundedContextConfig {
//!     tier: GovernanceTier::Full,
//!     override_review_required: None,
//!     override_ci_gates: None,
//! });
//!
//! let ctx = config.contexts.get("cosmon").unwrap();
//! assert_eq!(ctx.tier, GovernanceTier::Full);
//! assert!(ctx.tier.review_required());
//! assert!(ctx.tier.ci_gates_required());
//! assert!(ctx.effective_review_required());
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::agent::ParseEnumError;

/// Governance tier assigned to a bounded context.
///
/// Tiers form a spectrum from maximum ceremony (`Full`) to minimum ceremony
/// (`AppendOnly`). Each tier defines defaults for branch strategy, review,
/// CI gates, and merge policy. Per-context overrides can adjust individual
/// settings without changing the tier.
///
/// Ordered by ceremony level: `Full` > `Light` > `GuardedMain` > `Micro` > `AppendOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceTier {
    /// Maximum ceremony: feature branches, mandatory review, full CI, merge queue.
    ///
    /// For production-critical bounded contexts with multiple contributors.
    /// Branch strategy: feature branches off main, no direct pushes.
    Full,

    /// Moderate ceremony: feature branches, optional review, CI required.
    ///
    /// For important bounded contexts where velocity matters but quality gates
    /// are still needed. Review is recommended but not enforced.
    Light,

    /// Minimal ceremony: direct commits to main, no review, basic CI.
    ///
    /// For experimental, single-contributor, or low-risk bounded contexts.
    /// CI runs but failures are warnings, not blockers.
    Micro,

    /// Protected main with gated merges but relaxed branch policies.
    ///
    /// Main branch is protected (no direct pushes), but feature branch
    /// naming and review are relaxed. CI is required to merge.
    /// Useful for repos with automated contributors (bots, agents).
    GuardedMain,

    /// Append-only: no deletions or force-pushes, minimal other constraints.
    ///
    /// For audit logs, configuration stores, and append-only data repos.
    /// The only hard constraint is immutability of existing content.
    AppendOnly,
}

impl GovernanceTier {
    /// Whether this tier requires code review before merge by default.
    #[must_use]
    pub fn review_required(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Whether this tier requires CI gates to pass before merge by default.
    #[must_use]
    pub fn ci_gates_required(self) -> bool {
        matches!(self, Self::Full | Self::Light | Self::GuardedMain)
    }

    /// Whether this tier uses feature branches (vs. direct main commits).
    #[must_use]
    pub fn feature_branches(self) -> bool {
        matches!(self, Self::Full | Self::Light | Self::GuardedMain)
    }

    /// Whether this tier protects main from direct pushes.
    #[must_use]
    pub fn main_protected(self) -> bool {
        !matches!(self, Self::Micro)
    }

    /// Whether this tier uses a merge queue (vs. direct merge).
    #[must_use]
    pub fn merge_queue(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Whether this tier forbids force-pushes and history rewriting.
    #[must_use]
    pub fn append_only(self) -> bool {
        matches!(self, Self::AppendOnly)
    }

    /// All tier variants, ordered by ceremony level (highest first).
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Full,
            Self::Light,
            Self::GuardedMain,
            Self::Micro,
            Self::AppendOnly,
        ]
    }
}

impl fmt::Display for GovernanceTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("full"),
            Self::Light => f.write_str("light"),
            Self::Micro => f.write_str("micro"),
            Self::GuardedMain => f.write_str("guarded_main"),
            Self::AppendOnly => f.write_str("append_only"),
        }
    }
}

impl FromStr for GovernanceTier {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full" => Ok(Self::Full),
            "light" => Ok(Self::Light),
            "micro" => Ok(Self::Micro),
            "guarded_main" => Ok(Self::GuardedMain),
            "append_only" => Ok(Self::AppendOnly),
            _ => Err(ParseEnumError {
                type_name: "GovernanceTier",
                value: s.to_owned(),
            }),
        }
    }
}

/// Configuration for a single bounded context (rig).
///
/// The tier provides defaults; optional overrides allow fine-tuning
/// individual policies without changing the tier assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedContextConfig {
    /// The governance tier for this context.
    pub tier: GovernanceTier,

    /// Override the tier's default review requirement.
    ///
    /// `None` means use the tier default. `Some(true)` forces review
    /// even if the tier doesn't require it; `Some(false)` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_review_required: Option<bool>,

    /// Override the tier's default CI gate requirement.
    ///
    /// `None` means use the tier default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_ci_gates: Option<bool>,
}

impl BoundedContextConfig {
    /// Effective review requirement (override takes precedence over tier default).
    #[must_use]
    pub fn effective_review_required(&self) -> bool {
        self.override_review_required
            .unwrap_or_else(|| self.tier.review_required())
    }

    /// Effective CI gate requirement (override takes precedence over tier default).
    #[must_use]
    pub fn effective_ci_gates_required(&self) -> bool {
        self.override_ci_gates
            .unwrap_or_else(|| self.tier.ci_gates_required())
    }
}

/// Top-level governance configuration mapping bounded contexts to tiers.
///
/// Loaded from a TOML file at startup. Unknown contexts are allowed (they
/// use the `default_tier`).
///
/// # TOML format
///
/// ```toml
/// default_tier = "light"
///
/// [contexts.cosmon]
/// tier = "full"
///
/// [contexts.beads]
/// tier = "guarded_main"
///
/// [contexts.scratch]
/// tier = "micro"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceConfig {
    /// Default tier for contexts not explicitly listed.
    #[serde(default = "default_tier")]
    pub default_tier: GovernanceTier,

    /// Per-context tier assignments.
    #[serde(default)]
    pub contexts: BTreeMap<String, BoundedContextConfig>,
}

/// Default tier when none is specified: Light.
fn default_tier() -> GovernanceTier {
    GovernanceTier::Light
}

impl GovernanceConfig {
    /// Create an empty config with the default tier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_tier: default_tier(),
            contexts: BTreeMap::new(),
        }
    }

    /// Look up the config for a named bounded context.
    ///
    /// Returns `None` if the context is not explicitly configured.
    /// Use [`effective_tier`](Self::effective_tier) to fall back to the default.
    #[must_use]
    pub fn get(&self, context: &str) -> Option<&BoundedContextConfig> {
        self.contexts.get(context)
    }

    /// The effective tier for a context, falling back to `default_tier`.
    #[must_use]
    pub fn effective_tier(&self, context: &str) -> GovernanceTier {
        self.contexts
            .get(context)
            .map_or(self.default_tier, |c| c.tier)
    }

    /// Parse a governance config from a TOML string.
    ///
    /// # Errors
    ///
    /// Returns a parse error if the TOML is malformed or contains
    /// invalid tier names.
    pub fn from_toml(toml_str: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_str)
    }

    /// Serialize this config to a TOML string.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the config cannot be represented
    /// as valid TOML.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ── Review Delegation Matrix ─────────────────────────────────────────

/// Classification of a change for review delegation purposes.
///
/// The change type determines which reviewers are required. This is orthogonal
/// to the governance tier — a `Full` tier repo still delegates bug-fix reviews
/// to critic agents, while domain type changes always require human review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// Domain model types (structs, enums, traits that define the domain).
    ///
    /// These are the "nouns" of the system — changes here reshape the
    /// conceptual model and require human judgment about correctness.
    DomainType,

    /// Founding thesis, ADRs, and architectural decision records.
    ///
    /// Changes to the intellectual foundation of the project. These are
    /// irreversible in the sense that downstream code depends on them.
    FoundingThesis,

    /// Production code: business logic, state machines, protocol handlers.
    ///
    /// Code that runs in production. Requires both automated analysis
    /// (critic agent) and human sign-off.
    ProductionCode,

    /// Research code: experiments, prototypes, analysis scripts.
    ///
    /// Exploratory code with limited blast radius. Critic agent review
    /// is sufficient — human review would slow the feedback loop.
    ResearchCode,

    /// Vault notes: documentation, design docs, knowledge base entries.
    ///
    /// Written artifacts that inform but don't execute. Critic agent
    /// catches quality issues; human review is not required.
    VaultNotes,

    /// Bug fix with a regression test proving the fix.
    ///
    /// The regression test IS the review — it encodes the invariant that
    /// was violated. Critic agent validates test quality.
    BugFixWithTest,

    /// Ops configuration: CI pipelines, deploy configs, monitoring rules.
    ///
    /// Validated by patrol auto-checks. No human or critic agent needed
    /// when the patrol confirms the config is structurally sound.
    OpsConfig,
}

impl ChangeType {
    /// All change type variants.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::DomainType,
            Self::FoundingThesis,
            Self::ProductionCode,
            Self::ResearchCode,
            Self::VaultNotes,
            Self::BugFixWithTest,
            Self::OpsConfig,
        ]
    }
}

impl fmt::Display for ChangeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DomainType => f.write_str("domain_type"),
            Self::FoundingThesis => f.write_str("founding_thesis"),
            Self::ProductionCode => f.write_str("production_code"),
            Self::ResearchCode => f.write_str("research_code"),
            Self::VaultNotes => f.write_str("vault_notes"),
            Self::BugFixWithTest => f.write_str("bug_fix_with_test"),
            Self::OpsConfig => f.write_str("ops_config"),
        }
    }
}

impl FromStr for ChangeType {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "domain_type" => Ok(Self::DomainType),
            "founding_thesis" => Ok(Self::FoundingThesis),
            "production_code" => Ok(Self::ProductionCode),
            "research_code" => Ok(Self::ResearchCode),
            "vault_notes" => Ok(Self::VaultNotes),
            "bug_fix_with_test" => Ok(Self::BugFixWithTest),
            "ops_config" => Ok(Self::OpsConfig),
            _ => Err(ParseEnumError {
                type_name: "ChangeType",
                value: s.to_owned(),
            }),
        }
    }
}

/// Who must review a change before it can merge.
///
/// Reviewers are ordered by authority: `Human` outranks `CriticAgent` outranks
/// `PatrolAuto`. A review policy may require multiple reviewer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reviewer {
    // Variant order determines Ord: lowest authority first.
    /// Automated patrol validation (structural checks, schema validation).
    ///
    /// No human or AI judgment involved — purely rule-based validation.
    PatrolAuto,

    /// AI critic agent review (code quality, style, correctness analysis).
    ///
    /// Automated but judgment-based. The critic agent reads the diff and
    /// provides structured feedback with a grade.
    CriticAgent,

    /// Human reviewer (maintainer, domain expert, or designated approver).
    ///
    /// Required for changes that affect the conceptual model, architecture,
    /// or production behavior in ways that require human judgment.
    Human,
}

impl fmt::Display for Reviewer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PatrolAuto => f.write_str("patrol_auto"),
            Self::CriticAgent => f.write_str("critic_agent"),
            Self::Human => f.write_str("human"),
        }
    }
}

impl FromStr for Reviewer {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "patrol_auto" => Ok(Self::PatrolAuto),
            "critic_agent" => Ok(Self::CriticAgent),
            "human" => Ok(Self::Human),
            _ => Err(ParseEnumError {
                type_name: "Reviewer",
                value: s.to_owned(),
            }),
        }
    }
}

/// Review policy for a specific change type: which reviewers are required.
///
/// The policy is the row in the review delegation matrix. It captures
/// the minimum set of reviewers needed for a change to be approved.
///
/// # Examples
///
/// ```
/// use cosmon_core::governance::{ReviewPolicy, Reviewer};
///
/// let policy = ReviewPolicy::human_required();
/// assert!(policy.requires_human());
/// assert!(!policy.requires_critic_agent());
///
/// let policy = ReviewPolicy::critic_and_human();
/// assert!(policy.requires_human());
/// assert!(policy.requires_critic_agent());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewPolicy {
    /// The set of required reviewers, sorted by authority.
    reviewers: Vec<Reviewer>,
}

impl ReviewPolicy {
    /// Policy requiring only human review (domain types, founding thesis).
    #[must_use]
    pub fn human_required() -> Self {
        Self {
            reviewers: vec![Reviewer::Human],
        }
    }

    /// Policy requiring both critic agent and human review (production code).
    #[must_use]
    pub fn critic_and_human() -> Self {
        Self {
            reviewers: vec![Reviewer::CriticAgent, Reviewer::Human],
        }
    }

    /// Policy requiring only critic agent review (research, vault, bug fixes).
    #[must_use]
    pub fn critic_only() -> Self {
        Self {
            reviewers: vec![Reviewer::CriticAgent],
        }
    }

    /// Policy requiring only patrol auto-validation (ops config).
    #[must_use]
    pub fn patrol_auto() -> Self {
        Self {
            reviewers: vec![Reviewer::PatrolAuto],
        }
    }

    /// Whether this policy requires a human reviewer.
    #[must_use]
    pub fn requires_human(&self) -> bool {
        self.reviewers.contains(&Reviewer::Human)
    }

    /// Whether this policy requires a critic agent.
    #[must_use]
    pub fn requires_critic_agent(&self) -> bool {
        self.reviewers.contains(&Reviewer::CriticAgent)
    }

    /// Whether this policy requires patrol auto-validation.
    #[must_use]
    pub fn requires_patrol(&self) -> bool {
        self.reviewers.contains(&Reviewer::PatrolAuto)
    }

    /// The set of required reviewers.
    #[must_use]
    pub fn reviewers(&self) -> &[Reviewer] {
        &self.reviewers
    }

    /// The highest-authority reviewer required by this policy.
    #[must_use]
    pub fn highest_reviewer(&self) -> Option<Reviewer> {
        self.reviewers.iter().max().copied()
    }
}

impl fmt::Display for ReviewPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .reviewers
            .iter()
            .map(|r| match r {
                Reviewer::PatrolAuto => "patrol",
                Reviewer::CriticAgent => "critic",
                Reviewer::Human => "human",
            })
            .collect();
        write!(f, "{}", names.join("+"))
    }
}

/// The review delegation matrix: maps change types to review policies.
///
/// This is the core decision table that determines who reviews what.
/// The matrix is associated with `GovernanceTier` — only `Full` and `Light`
/// tiers consult the matrix (lower tiers have relaxed review requirements).
///
/// # The Matrix
///
/// | Change Type | Required Reviewers |
/// |---|---|
/// | Domain types | Human |
/// | Founding thesis / ADRs | Human |
/// | Production code | Critic agent + Human |
/// | Research code | Critic agent |
/// | Vault notes | Critic agent |
/// | Bug fix with regression test | Critic agent |
/// | Ops config | Patrol auto-validation |
///
/// # Examples
///
/// ```
/// use cosmon_core::governance::{ChangeType, ReviewDelegationMatrix, Reviewer};
///
/// let policy = ReviewDelegationMatrix::policy_for(ChangeType::DomainType);
/// assert!(policy.requires_human());
///
/// let policy = ReviewDelegationMatrix::policy_for(ChangeType::OpsConfig);
/// assert!(policy.requires_patrol());
/// assert!(!policy.requires_human());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ReviewDelegationMatrix;

impl ReviewDelegationMatrix {
    /// Look up the review policy for a given change type.
    ///
    /// This encodes the founding decision: domain types and thesis changes
    /// require human judgment; production code needs both AI and human review;
    /// research and documentation need only AI review; ops config is
    /// machine-validated.
    #[must_use]
    pub fn policy_for(change_type: ChangeType) -> ReviewPolicy {
        match change_type {
            // Human required: changes to the conceptual model
            ChangeType::DomainType | ChangeType::FoundingThesis => ReviewPolicy::human_required(),

            // Critic agent + human: production code needs both
            ChangeType::ProductionCode => ReviewPolicy::critic_and_human(),

            // Critic agent only: exploratory/documentation/tested fixes
            ChangeType::ResearchCode | ChangeType::VaultNotes | ChangeType::BugFixWithTest => {
                ReviewPolicy::critic_only()
            }

            // Patrol auto-validation: structural checks suffice
            ChangeType::OpsConfig => ReviewPolicy::patrol_auto(),
        }
    }

    /// Check whether a governance tier consults the delegation matrix.
    ///
    /// Only `Full` and `Light` tiers use the matrix. Lower-ceremony tiers
    /// have relaxed review requirements that don't depend on change type.
    #[must_use]
    pub fn applies_to_tier(tier: GovernanceTier) -> bool {
        matches!(tier, GovernanceTier::Full | GovernanceTier::Light)
    }

    /// Get the effective review policy for a change within a governance tier.
    ///
    /// For tiers that don't consult the matrix, returns `None` — the caller
    /// should fall back to the tier's default `review_required()` setting.
    #[must_use]
    pub fn effective_policy(tier: GovernanceTier, change_type: ChangeType) -> Option<ReviewPolicy> {
        if Self::applies_to_tier(tier) {
            Some(Self::policy_for(change_type))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_display_roundtrip() {
        for tier in GovernanceTier::all() {
            let s = tier.to_string();
            let parsed: GovernanceTier = s.parse().unwrap();
            assert_eq!(parsed, *tier, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_tier_parse_invalid() {
        let err = "invalid".parse::<GovernanceTier>().unwrap_err();
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn test_all_tiers_has_five() {
        assert_eq!(GovernanceTier::all().len(), 5);
    }

    #[test]
    fn test_full_tier_defaults() {
        let tier = GovernanceTier::Full;
        assert!(tier.review_required());
        assert!(tier.ci_gates_required());
        assert!(tier.feature_branches());
        assert!(tier.main_protected());
        assert!(tier.merge_queue());
        assert!(!tier.append_only());
    }

    #[test]
    fn test_light_tier_defaults() {
        let tier = GovernanceTier::Light;
        assert!(!tier.review_required());
        assert!(tier.ci_gates_required());
        assert!(tier.feature_branches());
        assert!(tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(!tier.append_only());
    }

    #[test]
    fn test_micro_tier_defaults() {
        let tier = GovernanceTier::Micro;
        assert!(!tier.review_required());
        assert!(!tier.ci_gates_required());
        assert!(!tier.feature_branches());
        assert!(!tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(!tier.append_only());
    }

    #[test]
    fn test_guarded_main_tier_defaults() {
        let tier = GovernanceTier::GuardedMain;
        assert!(!tier.review_required());
        assert!(tier.ci_gates_required());
        assert!(tier.feature_branches());
        assert!(tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(!tier.append_only());
    }

    #[test]
    fn test_append_only_tier_defaults() {
        let tier = GovernanceTier::AppendOnly;
        assert!(!tier.review_required());
        assert!(!tier.ci_gates_required());
        assert!(!tier.feature_branches());
        assert!(tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(tier.append_only());
    }

    #[test]
    fn test_bounded_context_config_effective_defaults() {
        let ctx = BoundedContextConfig {
            tier: GovernanceTier::Full,
            override_review_required: None,
            override_ci_gates: None,
        };
        assert!(ctx.effective_review_required());
        assert!(ctx.effective_ci_gates_required());
    }

    #[test]
    fn test_bounded_context_config_overrides() {
        let ctx = BoundedContextConfig {
            tier: GovernanceTier::Full,
            override_review_required: Some(false),
            override_ci_gates: Some(false),
        };
        assert!(!ctx.effective_review_required());
        assert!(!ctx.effective_ci_gates_required());

        let ctx = BoundedContextConfig {
            tier: GovernanceTier::Micro,
            override_review_required: Some(true),
            override_ci_gates: Some(true),
        };
        assert!(ctx.effective_review_required());
        assert!(ctx.effective_ci_gates_required());
    }

    #[test]
    fn test_governance_config_default() {
        let config = GovernanceConfig::new();
        assert_eq!(config.default_tier, GovernanceTier::Light);
        assert!(config.contexts.is_empty());
    }

    #[test]
    fn test_governance_config_effective_tier() {
        let mut config = GovernanceConfig::new();
        config.contexts.insert(
            "cosmon".into(),
            BoundedContextConfig {
                tier: GovernanceTier::Full,
                override_review_required: None,
                override_ci_gates: None,
            },
        );

        assert_eq!(config.effective_tier("cosmon"), GovernanceTier::Full);
        assert_eq!(config.effective_tier("unknown"), GovernanceTier::Light);
    }

    #[test]
    fn test_toml_roundtrip() {
        let mut config = GovernanceConfig::new();
        config.default_tier = GovernanceTier::Light;
        config.contexts.insert(
            "cosmon".into(),
            BoundedContextConfig {
                tier: GovernanceTier::Full,
                override_review_required: None,
                override_ci_gates: None,
            },
        );
        config.contexts.insert(
            "beads".into(),
            BoundedContextConfig {
                tier: GovernanceTier::GuardedMain,
                override_review_required: Some(true),
                override_ci_gates: None,
            },
        );

        let toml_str = config.to_toml().unwrap();
        let parsed = GovernanceConfig::from_toml(&toml_str).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_toml_parse_minimal() {
        let toml_str = r#"
default_tier = "micro"

[contexts.scratch]
tier = "micro"
"#;
        let config = GovernanceConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.default_tier, GovernanceTier::Micro);
        assert_eq!(config.effective_tier("scratch"), GovernanceTier::Micro);
    }

    #[test]
    fn test_toml_parse_full_example() {
        let toml_str = r#"
default_tier = "light"

[contexts.cosmon]
tier = "full"

[contexts.beads]
tier = "guarded_main"
override_review_required = true

[contexts.scratch]
tier = "micro"

[contexts.audit-log]
tier = "append_only"

[contexts.gastown]
tier = "light"
override_ci_gates = false
"#;
        let config = GovernanceConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.contexts.len(), 5);

        let cosmon = config.get("cosmon").unwrap();
        assert_eq!(cosmon.tier, GovernanceTier::Full);
        assert!(cosmon.effective_review_required());

        let beads = config.get("beads").unwrap();
        assert_eq!(beads.tier, GovernanceTier::GuardedMain);
        assert!(beads.effective_review_required()); // overridden to true

        let gastown = config.get("gastown").unwrap();
        assert!(!gastown.effective_ci_gates_required()); // overridden to false
    }

    #[test]
    fn test_toml_defaults_when_empty() {
        let toml_str = "";
        let config = GovernanceConfig::from_toml(toml_str).unwrap();
        assert_eq!(config.default_tier, GovernanceTier::Light);
        assert!(config.contexts.is_empty());
    }

    #[test]
    fn test_serde_json_roundtrip() {
        let mut config = GovernanceConfig::new();
        config.contexts.insert(
            "cosmon".into(),
            BoundedContextConfig {
                tier: GovernanceTier::Full,
                override_review_required: None,
                override_ci_gates: None,
            },
        );
        let json = serde_json::to_string(&config).unwrap();
        let back: GovernanceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn test_serde_skips_none_overrides() {
        let ctx = BoundedContextConfig {
            tier: GovernanceTier::Full,
            override_review_required: None,
            override_ci_gates: None,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("override_review_required"));
        assert!(!json.contains("override_ci_gates"));
    }

    // ── Review Delegation Matrix tests ───────────────────────────────

    #[test]
    fn test_change_type_display_roundtrip() {
        for ct in ChangeType::all() {
            let s = ct.to_string();
            let parsed: ChangeType = s.parse().unwrap();
            assert_eq!(parsed, *ct, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_change_type_parse_invalid() {
        let err = "not_a_type".parse::<ChangeType>().unwrap_err();
        assert!(err.to_string().contains("not_a_type"));
    }

    #[test]
    fn test_change_type_all_has_seven() {
        assert_eq!(ChangeType::all().len(), 7);
    }

    #[test]
    fn test_reviewer_ordering() {
        assert!(Reviewer::PatrolAuto < Reviewer::CriticAgent);
        assert!(Reviewer::CriticAgent < Reviewer::Human);
    }

    #[test]
    fn test_reviewer_display_roundtrip() {
        for r in [Reviewer::PatrolAuto, Reviewer::CriticAgent, Reviewer::Human] {
            let s = r.to_string();
            let parsed: Reviewer = s.parse().unwrap();
            assert_eq!(parsed, r);
        }
    }

    #[test]
    fn test_matrix_domain_type_requires_human() {
        let policy = ReviewDelegationMatrix::policy_for(ChangeType::DomainType);
        assert!(policy.requires_human());
        assert!(!policy.requires_critic_agent());
        assert!(!policy.requires_patrol());
    }

    #[test]
    fn test_matrix_founding_thesis_requires_human() {
        let policy = ReviewDelegationMatrix::policy_for(ChangeType::FoundingThesis);
        assert!(policy.requires_human());
        assert!(!policy.requires_critic_agent());
    }

    #[test]
    fn test_matrix_production_code_requires_critic_and_human() {
        let policy = ReviewDelegationMatrix::policy_for(ChangeType::ProductionCode);
        assert!(policy.requires_human());
        assert!(policy.requires_critic_agent());
        assert!(!policy.requires_patrol());
    }

    #[test]
    fn test_matrix_research_code_critic_only() {
        let policy = ReviewDelegationMatrix::policy_for(ChangeType::ResearchCode);
        assert!(policy.requires_critic_agent());
        assert!(!policy.requires_human());
    }

    #[test]
    fn test_matrix_vault_notes_critic_only() {
        let policy = ReviewDelegationMatrix::policy_for(ChangeType::VaultNotes);
        assert!(policy.requires_critic_agent());
        assert!(!policy.requires_human());
    }

    #[test]
    fn test_matrix_bug_fix_with_test_critic_only() {
        let policy = ReviewDelegationMatrix::policy_for(ChangeType::BugFixWithTest);
        assert!(policy.requires_critic_agent());
        assert!(!policy.requires_human());
    }

    #[test]
    fn test_matrix_ops_config_patrol_only() {
        let policy = ReviewDelegationMatrix::policy_for(ChangeType::OpsConfig);
        assert!(policy.requires_patrol());
        assert!(!policy.requires_human());
        assert!(!policy.requires_critic_agent());
    }

    #[test]
    fn test_matrix_applies_to_full_and_light() {
        assert!(ReviewDelegationMatrix::applies_to_tier(
            GovernanceTier::Full
        ));
        assert!(ReviewDelegationMatrix::applies_to_tier(
            GovernanceTier::Light
        ));
        assert!(!ReviewDelegationMatrix::applies_to_tier(
            GovernanceTier::Micro
        ));
        assert!(!ReviewDelegationMatrix::applies_to_tier(
            GovernanceTier::GuardedMain
        ));
        assert!(!ReviewDelegationMatrix::applies_to_tier(
            GovernanceTier::AppendOnly
        ));
    }

    #[test]
    fn test_matrix_effective_policy_full_tier() {
        let policy = ReviewDelegationMatrix::effective_policy(
            GovernanceTier::Full,
            ChangeType::ProductionCode,
        )
        .expect("Full tier should consult the matrix");
        assert!(policy.requires_critic_agent());
        assert!(policy.requires_human());
    }

    #[test]
    fn test_matrix_effective_policy_micro_tier_returns_none() {
        assert!(ReviewDelegationMatrix::effective_policy(
            GovernanceTier::Micro,
            ChangeType::ProductionCode,
        )
        .is_none());
    }

    #[test]
    fn test_review_policy_display() {
        assert_eq!(ReviewPolicy::human_required().to_string(), "human");
        assert_eq!(ReviewPolicy::critic_and_human().to_string(), "critic+human");
        assert_eq!(ReviewPolicy::critic_only().to_string(), "critic");
        assert_eq!(ReviewPolicy::patrol_auto().to_string(), "patrol");
    }

    #[test]
    fn test_review_policy_highest_reviewer() {
        assert_eq!(
            ReviewPolicy::human_required().highest_reviewer(),
            Some(Reviewer::Human)
        );
        assert_eq!(
            ReviewPolicy::critic_and_human().highest_reviewer(),
            Some(Reviewer::Human)
        );
        assert_eq!(
            ReviewPolicy::critic_only().highest_reviewer(),
            Some(Reviewer::CriticAgent)
        );
        assert_eq!(
            ReviewPolicy::patrol_auto().highest_reviewer(),
            Some(Reviewer::PatrolAuto)
        );
    }

    #[test]
    fn test_review_policy_serde_roundtrip() {
        let policy = ReviewPolicy::critic_and_human();
        let json = serde_json::to_string(&policy).unwrap();
        let back: ReviewPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, back);
    }

    #[test]
    fn test_matrix_covers_all_change_types() {
        for ct in ChangeType::all() {
            let policy = ReviewDelegationMatrix::policy_for(*ct);
            assert!(!policy.reviewers().is_empty(), "no reviewers for {ct}");
        }
    }
}
