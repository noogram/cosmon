// SPDX-License-Identifier: AGPL-3.0-only

//! Chamber types for bounded-context workspaces.
//!
//! A chamber is a workspace where an agent performs work within a bounded
//! context. The chamber's [`ChamberTier`] determines the governance ceremony
//! (branch strategy, review, CI, merge policy) while [`ChamberState`] tracks
//! the lifecycle from creation through merge or abandonment.
//!
//! The tier names follow the physics vocabulary of the cosmon domain:
//!
//! - **Sealed** — full governance (feature branches, mandatory review, CI, merge queue)
//! - **Open** — CI required but no human review
//! - **Cloud** — branch + linter + auto-merge
//! - **`BeamLine`** — direct main commits with schema validation
//! - **`DetectorLog`** — append-only with format validation
//!
//! # Examples
//!
//! ```
//! use cosmon_core::chamber::{Chamber, ChamberTier, ChamberState};
//! use cosmon_core::id::{ChamberId, AgentId};
//!
//! let chamber = Chamber::new(
//!     ChamberId::new("ch-001").unwrap(),
//!     "Implement agent lifecycle".into(),
//!     AgentId::new("onyx").unwrap(),
//!     ChamberTier::Sealed,
//! );
//!
//! assert_eq!(chamber.state(), ChamberState::Created);
//! assert!(chamber.tier().review_required());
//! assert!(chamber.worktree().is_none());
//! ```

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;
use crate::id::{AgentId, ChamberId};

// ---------------------------------------------------------------------------
// ChamberTier — governance ceremony level
// ---------------------------------------------------------------------------

/// Governance tier for a chamber's bounded context.
///
/// Determines the branch strategy, review requirements, CI gates, and merge
/// policy. Tiers are ordered from maximum ceremony (Sealed) to minimum
/// ceremony (`DetectorLog`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChamberTier {
    /// Full governance: feature branches, mandatory review, full CI, merge queue.
    ///
    /// For production-critical bounded contexts with multiple contributors.
    Sealed,

    /// CI required but no human review gate.
    ///
    /// For important bounded contexts where automated quality gates suffice.
    Open,

    /// Branch + linter + auto-merge.
    ///
    /// For moderate-risk contexts where fast iteration matters. Work happens
    /// on branches, linter must pass, but merge is automatic on green CI.
    Cloud,

    /// Direct main commits with schema validation.
    ///
    /// For low-risk, single-contributor contexts. No branching overhead,
    /// but commits must pass schema validation before landing.
    BeamLine,

    /// Append-only with format validation.
    ///
    /// For audit logs, configuration stores, and immutable data repos.
    /// Existing content cannot be modified or deleted; new content must
    /// pass format validation.
    DetectorLog,
}

impl ChamberTier {
    /// Whether this tier requires human code review before merge.
    #[must_use]
    pub fn review_required(self) -> bool {
        matches!(self, Self::Sealed)
    }

    /// Whether this tier requires CI gates to pass before merge.
    #[must_use]
    pub fn ci_gates_required(self) -> bool {
        matches!(self, Self::Sealed | Self::Open | Self::Cloud)
    }

    /// Whether this tier uses feature branches (vs. direct main commits).
    #[must_use]
    pub fn feature_branches(self) -> bool {
        matches!(self, Self::Sealed | Self::Open | Self::Cloud)
    }

    /// Whether this tier protects main from direct pushes.
    #[must_use]
    pub fn main_protected(self) -> bool {
        !matches!(self, Self::BeamLine)
    }

    /// Whether this tier uses a merge queue (vs. direct merge).
    #[must_use]
    pub fn merge_queue(self) -> bool {
        matches!(self, Self::Sealed)
    }

    /// Whether this tier enforces append-only semantics.
    #[must_use]
    pub fn append_only(self) -> bool {
        matches!(self, Self::DetectorLog)
    }

    /// Whether this tier runs a linter as part of its CI gates.
    #[must_use]
    pub fn linter_required(self) -> bool {
        matches!(self, Self::Sealed | Self::Open | Self::Cloud)
    }

    /// Whether this tier validates schemas on commit.
    #[must_use]
    pub fn schema_validation(self) -> bool {
        matches!(self, Self::BeamLine | Self::DetectorLog)
    }

    /// All tier variants, ordered by ceremony level (highest first).
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Sealed,
            Self::Open,
            Self::Cloud,
            Self::BeamLine,
            Self::DetectorLog,
        ]
    }
}

impl fmt::Display for ChamberTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sealed => f.write_str("sealed"),
            Self::Open => f.write_str("open"),
            Self::Cloud => f.write_str("cloud"),
            Self::BeamLine => f.write_str("beam_line"),
            Self::DetectorLog => f.write_str("detector_log"),
        }
    }
}

impl FromStr for ChamberTier {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sealed" => Ok(Self::Sealed),
            "open" => Ok(Self::Open),
            "cloud" => Ok(Self::Cloud),
            "beam_line" => Ok(Self::BeamLine),
            "detector_log" => Ok(Self::DetectorLog),
            _ => Err(ParseEnumError {
                type_name: "ChamberTier",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// ChamberState — lifecycle status
// ---------------------------------------------------------------------------

/// Lifecycle state of a chamber.
///
/// Tracks progression from creation through active work, review, and a
/// terminal state (merged or abandoned). Mirrors the rig lifecycle but
/// is specific to chamber-scoped work units.
///
/// ```text
/// Created ──▶ Active ──▶ Review ──▶ Merged
///                │          │
///                └──────────┴──▶ Abandoned
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChamberState {
    /// Chamber has been created but work has not started.
    Created,
    /// Chamber is actively being worked on.
    Active,
    /// Chamber work has been submitted for review.
    Review,
    /// Chamber changes have been merged (terminal).
    Merged,
    /// Chamber has been abandoned (terminal).
    Abandoned,
}

impl ChamberState {
    /// Whether this is a terminal state (no further transitions).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Abandoned)
    }

    /// All state variants in lifecycle order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Created,
            Self::Active,
            Self::Review,
            Self::Merged,
            Self::Abandoned,
        ]
    }
}

impl fmt::Display for ChamberState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created => f.write_str("created"),
            Self::Active => f.write_str("active"),
            Self::Review => f.write_str("review"),
            Self::Merged => f.write_str("merged"),
            Self::Abandoned => f.write_str("abandoned"),
        }
    }
}

impl FromStr for ChamberState {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "created" => Ok(Self::Created),
            "active" => Ok(Self::Active),
            "review" => Ok(Self::Review),
            "merged" => Ok(Self::Merged),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(ParseEnumError {
                type_name: "ChamberState",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Chamber
// ---------------------------------------------------------------------------

/// A bounded-context workspace where an agent performs a unit of work.
///
/// The chamber carries all metadata needed to track and govern an agent's
/// work: the task description, the assigned agent, the governance tier,
/// lifecycle state, git worktree and branch info, and provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chamber {
    /// Unique identifier for this chamber.
    id: ChamberId,
    /// Description of the task being performed in this chamber.
    task: String,
    /// The agent assigned to work in this chamber.
    agent: AgentId,
    /// Governance tier controlling ceremony level.
    tier: ChamberTier,
    /// Current lifecycle state.
    state: ChamberState,
    /// Path to the git worktree, if one has been created.
    worktree: Option<String>,
    /// Git branch name for this chamber's work.
    branch: Option<String>,
    /// The bounded context (write domain) this chamber operates within.
    write_domain: Option<String>,
    /// When the chamber was created.
    created_at: DateTime<Utc>,
    /// How this chamber was created (e.g. "dispatched by mayor", "manual").
    provenance: Option<String>,
}

impl Chamber {
    /// Create a new chamber in the [`ChamberState::Created`] state.
    #[must_use]
    pub fn new(id: ChamberId, task: String, agent: AgentId, tier: ChamberTier) -> Self {
        Self {
            id,
            task,
            agent,
            tier,
            state: ChamberState::Created,
            worktree: None,
            branch: None,
            write_domain: None,
            created_at: Utc::now(),
            provenance: None,
        }
    }

    /// The chamber's unique identifier.
    #[must_use]
    pub fn id(&self) -> &ChamberId {
        &self.id
    }

    /// Description of the task being performed.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// The assigned agent.
    #[must_use]
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// The governance tier.
    #[must_use]
    pub fn tier(&self) -> ChamberTier {
        self.tier
    }

    /// The current lifecycle state.
    #[must_use]
    pub fn state(&self) -> ChamberState {
        self.state
    }

    /// Path to the git worktree, if created.
    #[must_use]
    pub fn worktree(&self) -> Option<&str> {
        self.worktree.as_deref()
    }

    /// Git branch name for this chamber's work.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    /// The bounded context (write domain) this chamber operates within.
    #[must_use]
    pub fn write_domain(&self) -> Option<&str> {
        self.write_domain.as_deref()
    }

    /// When the chamber was created.
    #[must_use]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// How this chamber was created.
    #[must_use]
    pub fn provenance(&self) -> Option<&str> {
        self.provenance.as_deref()
    }

    /// Set the git worktree path.
    pub fn set_worktree(&mut self, path: impl Into<String>) {
        self.worktree = Some(path.into());
    }

    /// Set the git branch name.
    pub fn set_branch(&mut self, branch: impl Into<String>) {
        self.branch = Some(branch.into());
    }

    /// Set the write domain (bounded context).
    pub fn set_write_domain(&mut self, domain: impl Into<String>) {
        self.write_domain = Some(domain.into());
    }

    /// Set the provenance string.
    pub fn set_provenance(&mut self, provenance: impl Into<String>) {
        self.provenance = Some(provenance.into());
    }

    /// Transition to a new state.
    ///
    /// Returns `Err` if the transition is invalid according to the lifecycle:
    /// - `Created` -> `Active`
    /// - `Active` -> `Review` | `Abandoned`
    /// - `Review` -> `Merged` | `Abandoned`
    /// - `Merged`, `Abandoned` -> (terminal, no transitions)
    ///
    /// # Errors
    ///
    /// Returns the attempted target state if the transition is not allowed.
    pub fn transition(&mut self, target: ChamberState) -> Result<(), InvalidTransition> {
        let valid = matches!(
            (self.state, target),
            (ChamberState::Created, ChamberState::Active)
                | (
                    ChamberState::Active,
                    ChamberState::Review | ChamberState::Abandoned
                )
                | (
                    ChamberState::Review,
                    ChamberState::Merged | ChamberState::Abandoned
                )
        );

        if valid {
            self.state = target;
            Ok(())
        } else {
            Err(InvalidTransition {
                from: self.state,
                to: target,
            })
        }
    }
}

/// Error returned when a chamber state transition is not allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    /// The current state.
    pub from: ChamberState,
    /// The attempted target state.
    pub to: ChamberState,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid chamber transition: {} -> {}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidTransition {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_chamber() -> Chamber {
        Chamber::new(
            ChamberId::new("ch-001").unwrap(),
            "Implement agent lifecycle".into(),
            AgentId::new("onyx").unwrap(),
            ChamberTier::Sealed,
        )
    }

    // -- ChamberTier --

    #[test]
    fn test_tier_display_roundtrip() {
        for tier in ChamberTier::all() {
            let s = tier.to_string();
            let parsed: ChamberTier = s.parse().unwrap();
            assert_eq!(parsed, *tier, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_tier_parse_invalid() {
        assert!("invalid".parse::<ChamberTier>().is_err());
    }

    #[test]
    fn test_tier_serde_roundtrip() {
        for tier in ChamberTier::all() {
            let json = serde_json::to_string(tier).unwrap();
            let back: ChamberTier = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *tier);
        }
    }

    #[test]
    fn test_all_tiers_has_five() {
        assert_eq!(ChamberTier::all().len(), 5);
    }

    #[test]
    fn test_sealed_tier_properties() {
        let tier = ChamberTier::Sealed;
        assert!(tier.review_required());
        assert!(tier.ci_gates_required());
        assert!(tier.feature_branches());
        assert!(tier.main_protected());
        assert!(tier.merge_queue());
        assert!(!tier.append_only());
        assert!(tier.linter_required());
        assert!(!tier.schema_validation());
    }

    #[test]
    fn test_open_tier_properties() {
        let tier = ChamberTier::Open;
        assert!(!tier.review_required());
        assert!(tier.ci_gates_required());
        assert!(tier.feature_branches());
        assert!(tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(!tier.append_only());
    }

    #[test]
    fn test_cloud_tier_properties() {
        let tier = ChamberTier::Cloud;
        assert!(!tier.review_required());
        assert!(tier.ci_gates_required());
        assert!(tier.feature_branches());
        assert!(tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(!tier.append_only());
        assert!(tier.linter_required());
    }

    #[test]
    fn test_beam_line_tier_properties() {
        let tier = ChamberTier::BeamLine;
        assert!(!tier.review_required());
        assert!(!tier.ci_gates_required());
        assert!(!tier.feature_branches());
        assert!(!tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(!tier.append_only());
        assert!(tier.schema_validation());
    }

    #[test]
    fn test_detector_log_tier_properties() {
        let tier = ChamberTier::DetectorLog;
        assert!(!tier.review_required());
        assert!(!tier.ci_gates_required());
        assert!(!tier.feature_branches());
        assert!(tier.main_protected());
        assert!(!tier.merge_queue());
        assert!(tier.append_only());
        assert!(tier.schema_validation());
    }

    // -- ChamberState --

    #[test]
    fn test_state_display_roundtrip() {
        for state in ChamberState::all() {
            let s = state.to_string();
            let parsed: ChamberState = s.parse().unwrap();
            assert_eq!(parsed, *state, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn test_state_parse_invalid() {
        assert!("invalid".parse::<ChamberState>().is_err());
    }

    #[test]
    fn test_state_serde_roundtrip() {
        for state in ChamberState::all() {
            let json = serde_json::to_string(state).unwrap();
            let back: ChamberState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *state);
        }
    }

    #[test]
    fn test_state_terminal() {
        assert!(!ChamberState::Created.is_terminal());
        assert!(!ChamberState::Active.is_terminal());
        assert!(!ChamberState::Review.is_terminal());
        assert!(ChamberState::Merged.is_terminal());
        assert!(ChamberState::Abandoned.is_terminal());
    }

    // -- Chamber --

    #[test]
    fn test_chamber_creation() {
        let chamber = test_chamber();
        assert_eq!(chamber.id().as_str(), "ch-001");
        assert_eq!(chamber.task(), "Implement agent lifecycle");
        assert_eq!(chamber.agent().as_str(), "onyx");
        assert_eq!(chamber.tier(), ChamberTier::Sealed);
        assert_eq!(chamber.state(), ChamberState::Created);
        assert!(chamber.worktree().is_none());
        assert!(chamber.branch().is_none());
        assert!(chamber.write_domain().is_none());
        assert!(chamber.provenance().is_none());
        assert!(chamber.created_at() <= Utc::now());
    }

    #[test]
    fn test_chamber_setters() {
        let mut chamber = test_chamber();
        chamber.set_worktree("/tmp/worktree");
        chamber.set_branch("polecat/onyx/cs-8mq");
        chamber.set_write_domain("cosmon");
        chamber.set_provenance("dispatched by mayor");

        assert_eq!(chamber.worktree(), Some("/tmp/worktree"));
        assert_eq!(chamber.branch(), Some("polecat/onyx/cs-8mq"));
        assert_eq!(chamber.write_domain(), Some("cosmon"));
        assert_eq!(chamber.provenance(), Some("dispatched by mayor"));
    }

    // -- Transitions --

    #[test]
    fn test_valid_transitions() {
        let mut chamber = test_chamber();

        assert!(chamber.transition(ChamberState::Active).is_ok());
        assert_eq!(chamber.state(), ChamberState::Active);

        assert!(chamber.transition(ChamberState::Review).is_ok());
        assert_eq!(chamber.state(), ChamberState::Review);

        assert!(chamber.transition(ChamberState::Merged).is_ok());
        assert_eq!(chamber.state(), ChamberState::Merged);
    }

    #[test]
    fn test_abandon_from_active() {
        let mut chamber = test_chamber();
        chamber.transition(ChamberState::Active).unwrap();
        assert!(chamber.transition(ChamberState::Abandoned).is_ok());
        assert_eq!(chamber.state(), ChamberState::Abandoned);
    }

    #[test]
    fn test_abandon_from_review() {
        let mut chamber = test_chamber();
        chamber.transition(ChamberState::Active).unwrap();
        chamber.transition(ChamberState::Review).unwrap();
        assert!(chamber.transition(ChamberState::Abandoned).is_ok());
        assert_eq!(chamber.state(), ChamberState::Abandoned);
    }

    #[test]
    fn test_invalid_transition_from_created() {
        let mut chamber = test_chamber();
        let err = chamber.transition(ChamberState::Merged).unwrap_err();
        assert_eq!(err.from, ChamberState::Created);
        assert_eq!(err.to, ChamberState::Merged);
        assert!(err.to_string().contains("created -> merged"));
    }

    #[test]
    fn test_invalid_transition_from_terminal() {
        let mut chamber = test_chamber();
        chamber.transition(ChamberState::Active).unwrap();
        chamber.transition(ChamberState::Review).unwrap();
        chamber.transition(ChamberState::Merged).unwrap();
        assert!(chamber.transition(ChamberState::Active).is_err());
    }

    #[test]
    fn test_invalid_transition_skip_state() {
        let mut chamber = test_chamber();
        // Cannot skip from Created to Review
        assert!(chamber.transition(ChamberState::Review).is_err());
    }

    #[test]
    fn test_chamber_serde_roundtrip() {
        let mut chamber = test_chamber();
        chamber.set_branch("polecat/onyx/cs-8mq");
        chamber.set_provenance("dispatched by mayor");

        let json = serde_json::to_string(&chamber).unwrap();
        let back: Chamber = serde_json::from_str(&json).unwrap();

        assert_eq!(back.id(), chamber.id());
        assert_eq!(back.task(), chamber.task());
        assert_eq!(back.agent(), chamber.agent());
        assert_eq!(back.tier(), chamber.tier());
        assert_eq!(back.state(), chamber.state());
        assert_eq!(back.branch(), chamber.branch());
        assert_eq!(back.provenance(), chamber.provenance());
    }
}
