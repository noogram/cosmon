// SPDX-License-Identifier: AGPL-3.0-only

//! Bead types for referencing Gas Town work items.
//!
//! Beads are the unit of tracked work in Gas Town. Real bead state lives in
//! Dolt (the git-for-data store); these types are thin, validated references
//! that Cosmon uses to link molecules and agents to their originating work
//! items without duplicating the source of truth.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::bead::{BeadId, BeadRef, BeadStatus};
//!
//! let id = BeadId::new("cs-4tu").unwrap();
//! assert_eq!(id.prefix(), "cs");
//! assert_eq!(id.suffix(), "4tu");
//!
//! let bead = BeadRef::new(id.clone(), "implement bead types".to_owned());
//! assert_eq!(bead.id(), &id);
//! assert_eq!(bead.title(), "implement bead types");
//! assert_eq!(bead.status(), BeadStatus::Open);
//!
//! // Invalid IDs are rejected:
//! assert!(BeadId::new("").is_err());
//! assert!(BeadId::new("nohyphen").is_err());
//! ```

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::id::IdError;

// ---------------------------------------------------------------------------
// BeadId — PREFIX-SUFFIX
// ---------------------------------------------------------------------------

/// Identifies a bead (work item) in Gas Town.
///
/// Format: `PREFIX-SUFFIX` where both parts are non-empty ASCII alphanumeric
/// strings. Examples: `cs-4tu`, `gt-pvx`, `cs-99m`.
///
/// This is a thin reference — the bead's full state lives in Dolt.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BeadId {
    raw: String,
    prefix: String,
    suffix: String,
}

impl BeadId {
    /// Parse and validate a bead ID string.
    ///
    /// # Errors
    /// Returns [`IdError`] if the string does not match `PREFIX-SUFFIX`.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        Self::parse_inner(&s)
    }

    fn parse_inner(s: &str) -> Result<Self, IdError> {
        if s.is_empty() {
            return Err(IdError::Empty { kind: "BeadId" });
        }

        // Split on first hyphen only — prefix is before, suffix is everything after.
        let Some((prefix, suffix)) = s.split_once('-') else {
            return Err(IdError::Invalid {
                kind: "BeadId",
                reason: format!("expected PREFIX-SUFFIX, got \"{s}\""),
            });
        };

        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(IdError::Invalid {
                kind: "BeadId",
                reason: format!("prefix must be non-empty alphanumeric, got \"{prefix}\""),
            });
        }

        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(IdError::Invalid {
                kind: "BeadId",
                reason: format!("suffix must be non-empty alphanumeric, got \"{suffix}\""),
            });
        }

        Ok(Self {
            raw: s.to_owned(),
            prefix: prefix.to_owned(),
            suffix: suffix.to_owned(),
        })
    }

    /// Return the full ID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Return the prefix part (e.g. `"cs"` from `"cs-4tu"`).
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Return the suffix part (e.g. `"4tu"` from `"cs-4tu"`).
    #[must_use]
    pub fn suffix(&self) -> &str {
        &self.suffix
    }
}

impl fmt::Display for BeadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for BeadId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_inner(s)
    }
}

impl TryFrom<String> for BeadId {
    type Error = IdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<BeadId> for String {
    fn from(id: BeadId) -> Self {
        id.raw
    }
}

// ---------------------------------------------------------------------------
// BeadStatus
// ---------------------------------------------------------------------------

/// Lifecycle status of a bead.
///
/// Maps to the valid statuses in the Gas Town beads system. This is the
/// subset that Cosmon tracks — the authoritative status lives in Dolt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadStatus {
    /// Bead is open and available for work.
    Open,
    /// Bead is actively being worked.
    InProgress,
    /// Bead work is complete.
    Closed,
    /// Bead is blocked on a dependency.
    Blocked,
}

impl BeadStatus {
    /// Return the status as a `snake_case` string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Closed => "closed",
            Self::Blocked => "blocked",
        }
    }

    /// Return whether this status represents an active (non-terminal) state.
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Open | Self::InProgress)
    }
}

impl fmt::Display for BeadStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BeadStatus {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(Self::Open),
            "in_progress" => Ok(Self::InProgress),
            "closed" => Ok(Self::Closed),
            "blocked" => Ok(Self::Blocked),
            _ => Err(IdError::Invalid {
                kind: "BeadStatus",
                reason: format!("expected open|in_progress|closed|blocked, got \"{s}\""),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// BeadRef — thin reference to a bead
// ---------------------------------------------------------------------------

/// A thin reference to a bead (work item) in Dolt.
///
/// Contains just enough information to identify and display a bead without
/// duplicating the full state that lives in the Dolt database. Used by
/// molecules and agents to track which work item they are associated with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadRef {
    id: BeadId,
    title: String,
    status: BeadStatus,
}

impl BeadRef {
    /// Create a new bead reference with the given ID and title.
    ///
    /// Status defaults to [`BeadStatus::Open`].
    #[must_use]
    pub fn new(id: BeadId, title: String) -> Self {
        Self {
            id,
            title,
            status: BeadStatus::Open,
        }
    }

    /// Create a new bead reference with an explicit status.
    #[must_use]
    pub fn with_status(id: BeadId, title: String, status: BeadStatus) -> Self {
        Self { id, title, status }
    }

    /// Return the bead ID.
    #[must_use]
    pub fn id(&self) -> &BeadId {
        &self.id
    }

    /// Return the bead title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the current status.
    #[must_use]
    pub fn status(&self) -> BeadStatus {
        self.status
    }

    /// Update the status, returning the previous value.
    pub fn set_status(&mut self, status: BeadStatus) -> BeadStatus {
        std::mem::replace(&mut self.status, status)
    }
}

impl fmt::Display for BeadRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.id, self.status, self.title)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- BeadId --

    #[test]
    fn test_bead_id_parse_valid() {
        let id = BeadId::new("cs-4tu").unwrap();
        assert_eq!(id.prefix(), "cs");
        assert_eq!(id.suffix(), "4tu");
        assert_eq!(id.as_str(), "cs-4tu");
        assert_eq!(id.to_string(), "cs-4tu");
    }

    #[test]
    fn test_bead_id_various_valid_formats() {
        // Short prefix and suffix
        assert!(BeadId::new("gt-abc").is_ok());
        // Numeric suffix
        assert!(BeadId::new("cs-99m").is_ok());
        // Longer prefix
        assert!(BeadId::new("cosmon-pvx").is_ok());
        // Single-char parts
        assert!(BeadId::new("a-b").is_ok());
    }

    #[test]
    fn test_bead_id_rejects_invalid() {
        // Empty
        assert!(BeadId::new("").is_err());
        // No hyphen
        assert!(BeadId::new("nohyphen").is_err());
        // Empty prefix
        assert!(BeadId::new("-suffix").is_err());
        // Empty suffix
        assert!(BeadId::new("prefix-").is_err());
        // Non-alphanumeric prefix
        assert!(BeadId::new("c!s-4tu").is_err());
        // Non-alphanumeric suffix
        assert!(BeadId::new("cs-4t!u").is_err());
    }

    #[test]
    fn test_bead_id_display_roundtrip() {
        let id = BeadId::new("gt-pvx").unwrap();
        let displayed = id.to_string();
        let parsed: BeadId = displayed.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_bead_id_serde_roundtrip() {
        let id = BeadId::new("cs-4tu").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"cs-4tu\"");
        let back: BeadId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // -- BeadStatus --

    #[test]
    fn test_bead_status_as_str() {
        assert_eq!(BeadStatus::Open.as_str(), "open");
        assert_eq!(BeadStatus::InProgress.as_str(), "in_progress");
        assert_eq!(BeadStatus::Closed.as_str(), "closed");
        assert_eq!(BeadStatus::Blocked.as_str(), "blocked");
    }

    #[test]
    fn test_bead_status_is_active() {
        assert!(BeadStatus::Open.is_active());
        assert!(BeadStatus::InProgress.is_active());
        assert!(!BeadStatus::Closed.is_active());
        assert!(!BeadStatus::Blocked.is_active());
    }

    #[test]
    fn test_bead_status_parse_roundtrip() {
        for status in [
            BeadStatus::Open,
            BeadStatus::InProgress,
            BeadStatus::Closed,
            BeadStatus::Blocked,
        ] {
            let s = status.to_string();
            let parsed: BeadStatus = s.parse().unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn test_bead_status_parse_invalid() {
        assert!("unknown".parse::<BeadStatus>().is_err());
        assert!("OPEN".parse::<BeadStatus>().is_err());
    }

    #[test]
    fn test_bead_status_serde_roundtrip() {
        let status = BeadStatus::InProgress;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"in_progress\"");
        let back: BeadStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, back);
    }

    // -- BeadRef --

    #[test]
    fn test_bead_ref_new_defaults_to_open() {
        let id = BeadId::new("cs-4tu").unwrap();
        let bead = BeadRef::new(id, "implement bead types".to_owned());
        assert_eq!(bead.status(), BeadStatus::Open);
        assert_eq!(bead.title(), "implement bead types");
    }

    #[test]
    fn test_bead_ref_with_status() {
        let id = BeadId::new("gt-pvx").unwrap();
        let bead =
            BeadRef::with_status(id.clone(), "fix the bug".to_owned(), BeadStatus::InProgress);
        assert_eq!(bead.id(), &id);
        assert_eq!(bead.status(), BeadStatus::InProgress);
    }

    #[test]
    fn test_bead_ref_set_status_returns_previous() {
        let id = BeadId::new("cs-99m").unwrap();
        let mut bead = BeadRef::new(id, "test bead".to_owned());
        let prev = bead.set_status(BeadStatus::InProgress);
        assert_eq!(prev, BeadStatus::Open);
        assert_eq!(bead.status(), BeadStatus::InProgress);
    }

    #[test]
    fn test_bead_ref_display() {
        let id = BeadId::new("cs-4tu").unwrap();
        let bead = BeadRef::new(id, "implement bead types".to_owned());
        assert_eq!(bead.to_string(), "cs-4tu (open): implement bead types");
    }

    #[test]
    fn test_bead_ref_serde_roundtrip() {
        let id = BeadId::new("gt-abc").unwrap();
        let bead = BeadRef::with_status(id, "serde test".to_owned(), BeadStatus::Blocked);
        let json = serde_json::to_string(&bead).unwrap();
        let back: BeadRef = serde_json::from_str(&json).unwrap();
        assert_eq!(bead, back);
    }
}
