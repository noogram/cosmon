// SPDX-License-Identifier: AGPL-3.0-only

//! Rig lifecycle state machine using the typestate pattern.
//!
//! A rig represents a project workspace in the Gas Town topology. Its lifecycle
//! follows a governance model (ADR-GOV-002) where each rig transitions through
//! well-defined phases: creation, active work, review, and a terminal state
//! (merged or abandoned).
//!
//! The typestate pattern ensures that only valid transitions compile. Attempting
//! an invalid transition (e.g., `Rig<Created>::submit_for_review()` without
//! first activating) is a compile error, not a runtime panic.
//!
//! # State Diagram
//!
//! ```text
//! Created ──activate──▶ Active ──submit──▶ Review ──merge────▶ Merged
//!                         │                  │
//!                         │                  └──abandon──▶ Abandoned
//!                         │
//!                         └──abandon──▶ Abandoned
//! ```
//!
//! # Examples
//!
//! ```
//! use cosmon_core::id::RigId;
//! use cosmon_core::rig::{Rig, RigStatus};
//!
//! let id = RigId::new("cosmon").unwrap();
//! let rig = Rig::create(id, "Multi-agent orchestration framework".into());
//! assert_eq!(rig.status(), RigStatus::Created);
//!
//! // Activate the rig to begin work:
//! let rig = rig.activate();
//! assert_eq!(rig.status(), RigStatus::Active);
//!
//! // Submit for review:
//! let rig = rig.submit_for_review();
//! assert_eq!(rig.status(), RigStatus::Review);
//!
//! // Merge after approval:
//! let rig = rig.merge();
//! assert_eq!(rig.status(), RigStatus::Merged);
//! ```

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;
use crate::id::RigId;

// ---------------------------------------------------------------------------
// RigStatus — serializable enum for persistence / wire format
// ---------------------------------------------------------------------------

/// Serializable rig lifecycle status.
///
/// Mirrors the typestate variants for persistence and wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RigStatus {
    /// Rig has been defined but work has not started.
    Created,
    /// Rig is actively being worked on.
    Active,
    /// Rig work is submitted for review.
    Review,
    /// Rig changes have been merged (terminal).
    Merged,
    /// Rig has been abandoned (terminal).
    Abandoned,
}

impl fmt::Display for RigStatus {
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

impl FromStr for RigStatus {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "created" => Ok(Self::Created),
            "active" => Ok(Self::Active),
            "review" => Ok(Self::Review),
            "merged" => Ok(Self::Merged),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(ParseEnumError {
                type_name: "RigStatus",
                value: s.to_owned(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Typestate marker types
// ---------------------------------------------------------------------------

mod sealed {
    /// Prevents external crates from implementing `RigState`.
    pub trait Sealed {}
}

/// Trait bound for rig state markers. Sealed — only the five states
/// defined in this module implement it.
pub trait RigState: sealed::Sealed {
    /// The corresponding serializable status variant.
    fn status() -> RigStatus;
}

/// Created state — rig is defined but work has not begun.
#[derive(Debug, Clone)]
pub struct Created {
    _private: PhantomData<()>,
}

/// Active state — rig is being actively worked on.
#[derive(Debug, Clone)]
pub struct Active {
    _private: PhantomData<()>,
}

/// Review state — rig work has been submitted for review.
#[derive(Debug, Clone)]
pub struct Review {
    _private: PhantomData<()>,
}

/// Merged state — rig changes have been merged. Terminal.
#[derive(Debug, Clone)]
pub struct Merged {
    _private: PhantomData<()>,
}

/// Abandoned state — rig has been abandoned. Terminal.
///
/// Carries the reason for abandonment, eliminating the need for
/// `Option` + `expect()` on the `Rig` struct.
#[derive(Debug, Clone)]
pub struct Abandoned {
    reason: String,
}

impl sealed::Sealed for Created {}
impl sealed::Sealed for Active {}
impl sealed::Sealed for Review {}
impl sealed::Sealed for Merged {}
impl sealed::Sealed for Abandoned {}

impl RigState for Created {
    fn status() -> RigStatus {
        RigStatus::Created
    }
}
impl RigState for Active {
    fn status() -> RigStatus {
        RigStatus::Active
    }
}
impl RigState for Review {
    fn status() -> RigStatus {
        RigStatus::Review
    }
}
impl RigState for Merged {
    fn status() -> RigStatus {
        RigStatus::Merged
    }
}
impl RigState for Abandoned {
    fn status() -> RigStatus {
        RigStatus::Abandoned
    }
}

// ---------------------------------------------------------------------------
// Log entry
// ---------------------------------------------------------------------------

/// A timestamped log entry recording a rig lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigLogEntry {
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Human-readable description of the event.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Rig<S>
// ---------------------------------------------------------------------------

/// A rig instance parameterised by its lifecycle state.
///
/// Common fields are shared across all states. State-specific data
/// (abandonment reason, etc.) lives in the state marker and is only
/// accessible through the state-specific `impl` blocks.
///
/// The typestate marker ensures the compiler tracks the state type,
/// making invalid transitions (e.g., `Rig<Created>::merge()`) into
/// compile errors rather than runtime panics.
#[derive(Debug, Clone)]
pub struct Rig<S: RigState> {
    id: RigId,
    description: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    log: Vec<RigLogEntry>,
    state: S,
}

// --- Shared accessors (all states) ---

impl<S: RigState> Rig<S> {
    /// The rig's unique identifier.
    #[must_use]
    pub fn id(&self) -> &RigId {
        &self.id
    }

    /// Human-readable description of the rig's purpose.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// When the rig was created.
    #[must_use]
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// When the rig was last updated.
    #[must_use]
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    /// The lifecycle event log.
    #[must_use]
    pub fn log(&self) -> &[RigLogEntry] {
        &self.log
    }

    /// The serializable status corresponding to the current typestate.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn status(&self) -> RigStatus {
        S::status()
    }

    /// Internal: transfer common fields into a new state.
    fn transition_to<T: RigState>(self, new_state: T) -> Rig<T> {
        Rig {
            id: self.id,
            description: self.description,
            created_at: self.created_at,
            updated_at: Utc::now(),
            log: self.log,
            state: new_state,
        }
    }

    /// Internal: append a log entry.
    fn push_log(&mut self, message: impl Into<String>) {
        self.log.push(RigLogEntry {
            timestamp: Utc::now(),
            message: message.into(),
        });
    }
}

// ---------------------------------------------------------------------------
// Rig<Created> — the only state constructible from scratch
// ---------------------------------------------------------------------------

impl Rig<Created> {
    /// Create a new rig in the Created state.
    #[must_use]
    pub fn create(id: RigId, description: String) -> Self {
        let now = Utc::now();
        let mut rig = Self {
            id,
            description,
            created_at: now,
            updated_at: now,
            log: Vec::new(),
            state: Created {
                _private: PhantomData,
            },
        };
        rig.push_log("rig created");
        rig
    }

    /// Activate the rig to begin work.
    #[must_use]
    pub fn activate(mut self) -> Rig<Active> {
        self.push_log("rig activated");
        self.transition_to(Active {
            _private: PhantomData,
        })
    }
}

// ---------------------------------------------------------------------------
// Rig<Active>
// ---------------------------------------------------------------------------

impl Rig<Active> {
    /// Submit the rig's work for review.
    #[must_use]
    pub fn submit_for_review(mut self) -> Rig<Review> {
        self.push_log("submitted for review");
        self.transition_to(Review {
            _private: PhantomData,
        })
    }

    /// Abandon the rig, recording the reason.
    pub fn abandon(mut self, reason: impl Into<String>) -> Rig<Abandoned> {
        let reason = reason.into();
        self.push_log(format!("abandoned: {reason}"));
        self.transition_to(Abandoned { reason })
    }
}

// ---------------------------------------------------------------------------
// Rig<Review>
// ---------------------------------------------------------------------------

impl Rig<Review> {
    /// Merge the rig's changes after successful review.
    #[must_use]
    pub fn merge(mut self) -> Rig<Merged> {
        self.push_log("merged");
        self.transition_to(Merged {
            _private: PhantomData,
        })
    }

    /// Abandon the rig during review, recording the reason.
    pub fn abandon(mut self, reason: impl Into<String>) -> Rig<Abandoned> {
        let reason = reason.into();
        self.push_log(format!("abandoned during review: {reason}"));
        self.transition_to(Abandoned { reason })
    }
}

// ---------------------------------------------------------------------------
// Rig<Abandoned> — terminal, read-only accessors
// ---------------------------------------------------------------------------

impl Rig<Abandoned> {
    /// The reason the rig was abandoned.
    #[must_use]
    pub fn abandon_reason(&self) -> &str {
        &self.state.reason
    }
}

// Rig<Merged> — terminal, no additional methods needed.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_rig_id() -> RigId {
        RigId::new("cosmon").unwrap()
    }

    fn test_description() -> String {
        "Multi-agent orchestration framework".into()
    }

    // -- RigStatus serialization --

    #[test]
    fn test_rig_status_display_roundtrip() {
        for status in [
            RigStatus::Created,
            RigStatus::Active,
            RigStatus::Review,
            RigStatus::Merged,
            RigStatus::Abandoned,
        ] {
            let s = status.to_string();
            let parsed: RigStatus = s.parse().unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_rig_status_invalid_parse() {
        assert!("invalid".parse::<RigStatus>().is_err());
    }

    #[test]
    fn test_rig_status_serde_roundtrip() {
        for status in [
            RigStatus::Created,
            RigStatus::Active,
            RigStatus::Review,
            RigStatus::Merged,
            RigStatus::Abandoned,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: RigStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status);
        }
    }

    // -- Typestate tests: happy path --

    #[test]
    fn test_created_to_active() {
        let rig = Rig::create(test_rig_id(), test_description());
        assert_eq!(rig.status(), RigStatus::Created);

        let rig = rig.activate();
        assert_eq!(rig.status(), RigStatus::Active);
    }

    #[test]
    fn test_active_to_review() {
        let rig = Rig::create(test_rig_id(), test_description()).activate();

        let rig = rig.submit_for_review();
        assert_eq!(rig.status(), RigStatus::Review);
    }

    #[test]
    fn test_review_to_merged() {
        let rig = Rig::create(test_rig_id(), test_description())
            .activate()
            .submit_for_review();

        let rig = rig.merge();
        assert_eq!(rig.status(), RigStatus::Merged);
    }

    #[test]
    fn test_full_lifecycle_created_to_merged() {
        let rig = Rig::create(test_rig_id(), test_description())
            .activate()
            .submit_for_review()
            .merge();

        assert_eq!(rig.status(), RigStatus::Merged);
        assert_eq!(rig.id().as_str(), "cosmon");
        assert_eq!(rig.description(), "Multi-agent orchestration framework");
        // Log should have 4 entries: created, activated, submitted, merged
        assert_eq!(rig.log().len(), 4);
    }

    // -- Abandon paths --

    #[test]
    fn test_active_abandon() {
        let rig = Rig::create(test_rig_id(), test_description()).activate();

        let rig = rig.abandon("requirements changed");
        assert_eq!(rig.status(), RigStatus::Abandoned);
        assert_eq!(rig.abandon_reason(), "requirements changed");
    }

    #[test]
    fn test_review_abandon() {
        let rig = Rig::create(test_rig_id(), test_description())
            .activate()
            .submit_for_review();

        let rig = rig.abandon("review rejected");
        assert_eq!(rig.status(), RigStatus::Abandoned);
        assert_eq!(rig.abandon_reason(), "review rejected");
    }

    // -- Field accessors --

    #[test]
    fn test_rig_fields() {
        let rig = Rig::create(test_rig_id(), test_description());
        assert_eq!(rig.id().as_str(), "cosmon");
        assert_eq!(rig.description(), "Multi-agent orchestration framework");
        assert!(!rig.log().is_empty()); // has "rig created" entry
        assert!(rig.created_at() <= Utc::now());
        assert!(rig.updated_at() <= Utc::now());
    }

    #[test]
    fn test_log_entries_accumulate() {
        let rig = Rig::create(test_rig_id(), test_description());
        assert_eq!(rig.log().len(), 1);

        let rig = rig.activate();
        assert_eq!(rig.log().len(), 2);

        let rig = rig.submit_for_review();
        assert_eq!(rig.log().len(), 3);

        let rig = rig.merge();
        assert_eq!(rig.log().len(), 4);
    }

    // -- Compile-fail documentation --
    //
    // The following DO NOT COMPILE, proving typestate safety:
    //
    // ```compile_fail
    // // Cannot merge a Created rig:
    // let rig = Rig::create(RigId::new("x").unwrap(), "x".into());
    // rig.merge(); // ERROR: no method named `merge` found for `Rig<Created>`
    // ```
    //
    // ```compile_fail
    // // Cannot submit Created for review (must activate first):
    // let rig = Rig::create(RigId::new("x").unwrap(), "x".into());
    // rig.submit_for_review(); // ERROR: no method named `submit_for_review` found
    // ```
    //
    // ```compile_fail
    // // Cannot evolve a Merged rig (terminal):
    // let rig = Rig::create(RigId::new("x").unwrap(), "x".into())
    //     .activate().submit_for_review().merge();
    // rig.activate(); // ERROR: no method named `activate` found for `Rig<Merged>`
    // ```
    //
    // ```compile_fail
    // // Cannot evolve an Abandoned rig (terminal):
    // let rig = Rig::create(RigId::new("x").unwrap(), "x".into())
    //     .activate().abandon("done");
    // rig.activate(); // ERROR: no method named `activate` found for `Rig<Abandoned>`
    // ```
}
