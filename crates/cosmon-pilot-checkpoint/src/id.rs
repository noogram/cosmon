// SPDX-License-Identifier: AGPL-3.0-only

//! The four identifiers a checkpoint is keyed by.
//!
//! Each is a newtype over a validated string rather than a bare `String`,
//! because three of them become path segments in the store and the fourth is
//! quoted verbatim in a finding. A `MissionId` and a `SessionId` are both
//! `task-20260731-67f2`-shaped and would swap silently at a call site if they
//! were the same type.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CheckpointError;

/// Accept `[A-Za-z0-9._-]+`, reject everything else.
///
/// The alphabet is the intersection of "what cosmon ids already look like"
/// and "what is safe as a single path segment on every filesystem we target".
/// `.` is allowed but a segment of only dots is not, which is what keeps `..`
/// out.
fn validate(kind: &'static str, raw: String) -> Result<String, CheckpointError> {
    if raw.is_empty() {
        return Err(CheckpointError::InvalidIdentifier(format!(
            "{kind} is empty"
        )));
    }
    if !raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        return Err(CheckpointError::InvalidIdentifier(format!(
            "{kind} {raw:?} is not [A-Za-z0-9._-]+"
        )));
    }
    if raw.bytes().all(|b| b == b'.') {
        return Err(CheckpointError::InvalidIdentifier(format!(
            "{kind} {raw:?} is a path traversal segment"
        )));
    }
    Ok(raw)
}

/// Declare one validated-string newtype with its constructor and accessors.
macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Build a ", $kind, " from `raw`.")]
            ///
            /// # Errors
            ///
            /// [`CheckpointError::InvalidIdentifier`] if `raw` is empty, is a
            /// run of dots, or contains a byte outside `[A-Za-z0-9._-]`.
            pub fn new(raw: impl Into<String>) -> Result<Self, CheckpointError> {
                validate($kind, raw.into()).map(Self)
            }

            /// The identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = CheckpointError;
            fn try_from(raw: String) -> Result<Self, Self::Error> {
                Self::new(raw)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

id_newtype!(
    /// Identifies one published checkpoint within a mission.
    CheckpointId,
    "checkpoint id"
);

id_newtype!(
    /// Identifies the mission both pilots are flying — a molecule id in
    /// practice, since ADR-111 makes a mission an ordinary root molecule.
    MissionId,
    "mission id"
);

id_newtype!(
    /// Identifies the cosmon session that published a checkpoint.
    ///
    /// Not the *provider* session key — that is
    /// `cosmon_session_probe::SessionSelector`, and a pilot's presence record
    /// is what maps one to the other.
    SessionId,
    "session id"
);

id_newtype!(
    /// Identifies one assertion inside a checkpoint, so a finding can cite it
    /// by reference rather than by quoting its text and hoping it is unique.
    ClaimId,
    "claim id"
);

id_newtype!(
    /// Identifies one drift finding.
    ///
    /// Content-addressed rather than random: the same two checkpoints compared
    /// twice must produce the same finding ids, or an operator who re-runs the
    /// comparison cannot tell a re-report from a new finding.
    FindingId,
    "finding id"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_cosmon_id_is_accepted() {
        assert_eq!(
            MissionId::new("task-20260731-67f2").unwrap().as_str(),
            "task-20260731-67f2"
        );
    }

    #[test]
    fn a_path_separator_is_refused_before_it_becomes_a_directory() {
        for hostile in ["..", "../../etc", "a/b", "", "a\0b"] {
            assert!(
                CheckpointId::new(hostile).is_err(),
                "{hostile:?} should not be a checkpoint id"
            );
        }
    }

    #[test]
    fn deserialisation_validates_too() {
        // A hand-edited state file is the realistic attack surface here: the
        // constructor is bypassed, `serde` is not.
        let hostile = serde_json::from_str::<CheckpointId>("\"../escape\"");
        assert!(hostile.is_err());
    }
}
