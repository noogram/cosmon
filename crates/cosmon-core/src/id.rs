// SPDX-License-Identifier: AGPL-3.0-only

//! Identity newtypes for type-safe domain modeling.
//!
//! Each ID type wraps a `String` with validation on construction.
//! No type can be constructed from a raw string without going through `::new()` or `::parse()`.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::id::{AgentId, WorkerId, MoleculeId};
//!
//! // Simple IDs just need to be non-empty:
//! let agent = AgentId::new("witness").unwrap();
//! assert_eq!(agent.as_str(), "witness");
//!
//! // WorkerId supports optional "ep-" ensemble prefix:
//! let worker = WorkerId::new("ep-quartz").unwrap();
//! assert!(worker.has_ensemble_prefix());
//! assert_eq!(worker.name(), "quartz");
//!
//! // MoleculeId enforces PREFIX-YYYYMMDD-XXXX format:
//! let mol = MoleculeId::new("cs-20260401-hjdr").unwrap();
//! assert_eq!(mol.prefix(), "cs");
//! assert_eq!(mol.date(), "20260401");
//!
//! // Invalid IDs are rejected at construction time:
//! assert!(AgentId::new("").is_err());
//! assert!(MoleculeId::new("bad-format").is_err());
//! ```

use std::fmt;
use std::str::FromStr;

use chrono::Utc;
use rand::Rng;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Error returned when an ID string fails validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive] // error set will grow; external callers must keep a `_ =>` arm
pub enum IdError {
    /// The ID string was empty.
    #[error("{kind} cannot be empty")]
    Empty {
        /// Which ID type was being parsed.
        kind: &'static str,
    },
    /// The ID string failed validation.
    #[error("{kind}: {reason}")]
    Invalid {
        /// Which ID type was being parsed.
        kind: &'static str,
        /// Description of the validation failure.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Macro: simple validated newtype (non-empty string)
// ---------------------------------------------------------------------------

macro_rules! simple_id {
    (
        $(#[$meta:meta])*
        $name:ident, $kind:literal
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Create a new ID, validating that it is non-empty.
            ///
            /// # Errors
            /// Returns [`IdError::Empty`] if the string is empty.
            pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
                let s = s.into();
                if s.is_empty() {
                    return Err(IdError::Empty { kind: $kind });
                }
                Ok(Self(s))
            }

            /// Return the inner string slice.
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

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                Self::new(s)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Simple ID types
// ---------------------------------------------------------------------------

simple_id!(
    /// Identifies an agent definition (e.g. "witness", "refinery").
    AgentId, "AgentId"
);

simple_id!(
    /// Identifies a formula (workflow template).
    FormulaId, "FormulaId"
);

simple_id!(
    /// Identifies a running session.
    SessionId, "SessionId"
);

simple_id!(
    /// Identifies a step within a molecule.
    StepId, "StepId"
);

simple_id!(
    /// Identifies a fleet — a group of workers with a shared mission.
    FleetId, "FleetId"
);

simple_id!(
    /// Identifies a convoy — an ordered group of molecules.
    ConvoyId, "ConvoyId"
);

simple_id!(
    /// Identifies a rig (project workspace) in the Gas Town topology.
    RigId, "RigId"
);

simple_id!(
    /// Identifies a chamber (bounded-context workspace) in the Gas Town topology.
    ChamberId, "ChamberId"
);

simple_id!(
    /// Identifies a verifiable claim extracted from molecule output.
    ///
    /// Emitted by verifier pipelines alongside [`crate::event::Event::ClaimEmitted`]
    /// and later resolved by [`crate::event::Event::ClaimVerified`]. The ID is opaque
    /// to cosmon-core — callers (e.g. `cs verify`) choose the generation scheme
    /// (ULID, content-hash, monotonic counter). Only non-emptiness is enforced.
    ClaimId, "ClaimId"
);

simple_id!(
    /// Identifies a signal in the inter-agent signal bus.
    ///
    /// Typically an auto-incremented integer stringified by the `SQLite` adapter,
    /// but the core treats it as an opaque non-empty string.
    SignalId, "SignalId"
);

simple_id!(
    /// Identifies a Nucléon — a stable causal source that nucleates molecules.
    ///
    /// A Nucléon is *anything that causes molecules*: a human operator
    /// (`"you"`), an LLM worker with a stable persona (`"witness"`),
    /// a hypothetical world-model, or Noogram-self. The ID is the
    /// admission boundary's stable handle for that source — it never
    /// rotates within a session and is the key under which session
    /// continuity (G1) is asserted.
    ///
    /// The core treats `NucleonId` as an opaque non-empty string;
    /// the upstream ADR (`docs/adr/061-pilot-session-and-causal-closure.md`)
    /// and the matrix-tick admission layer (`crates/cosmon-matrix-tick`)
    /// govern how concrete IDs are minted from external identities
    /// (MXIDs, JWT `sub`, operator config). Cosmon-core only enforces
    /// the newtype boundary and non-emptiness.
    NucleonId, "NucleonId"
);

// ---------------------------------------------------------------------------
// ProjectId — dirname-XXXX (4-char hex of SHA-256 of canonical root)
// ---------------------------------------------------------------------------

/// Identifies a cosmon project.
///
/// Format: `{dirname}-{hash4}` where `dirname` is the project directory name
/// (lowercased, restricted to alphanumeric + hyphens) and `hash4` is the first
/// 4 hex characters of the SHA-256 digest of the canonical (absolute) project
/// root path.
///
/// Generated once by `cs init` and stored in `.cosmon/config.toml` under
/// `[project]`. All commands resolve it via the walk-up discovery path and
/// error if it is missing — there is no silent fallback.
///
/// # Examples
///
/// ```
/// use cosmon_core::id::ProjectId;
///
/// let id = ProjectId::new("cosmon-a1b2").unwrap();
/// assert_eq!(id.as_str(), "cosmon-a1b2");
/// assert_eq!(id.to_string(), "cosmon-a1b2");
///
/// // Roundtrip through FromStr:
/// let parsed: ProjectId = "cosmon-a1b2".parse().unwrap();
/// assert_eq!(id, parsed);
///
/// // Generation from a project root path:
/// let generated = ProjectId::generate(std::path::Path::new("/tmp/my-project"));
/// assert!(generated.as_str().starts_with("my-project-"));
/// assert_eq!(generated.as_str().len(), "my-project-".len() + 4);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectId(String);

impl ProjectId {
    /// Create a `ProjectId` from a pre-formatted string.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Empty`] if the string is empty.
    /// Returns [`IdError::Invalid`] if the string does not contain at least
    /// one hyphen separating the dirname from the hash suffix.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if s.is_empty() {
            return Err(IdError::Empty { kind: "ProjectId" });
        }
        // Must contain at least one hyphen (dirname-hash).
        if !s.contains('-') {
            return Err(IdError::Invalid {
                kind: "ProjectId",
                reason: "expected format: <dirname>-<hash4>".to_string(),
            });
        }
        Ok(Self(s))
    }

    /// Generate a `ProjectId` from a project root path.
    ///
    /// The dirname is extracted from the path, lowercased, and sanitized to
    /// contain only `[a-z0-9-]`. The hash suffix is the first 4 hex characters
    /// of the SHA-256 digest of the full path string.
    #[must_use]
    pub fn generate(project_root: &std::path::Path) -> Self {
        use sha2::{Digest, Sha256};

        let dirname = project_root.file_name().map_or_else(
            || "project".to_string(),
            |n| n.to_string_lossy().to_string(),
        );

        // Sanitize: lowercase, keep only alphanumeric + hyphens, collapse runs.
        let sanitized: String = dirname
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let sanitized = sanitized.trim_matches('-').to_string();
        let sanitized = if sanitized.is_empty() {
            "project".to_string()
        } else {
            sanitized
        };

        // Hash the canonical path string.
        let path_str = project_root.to_string_lossy();
        let hash = Sha256::digest(path_str.as_bytes());
        let hash4 = format!("{:02x}{:02x}", hash[0], hash[1]);

        Self(format!("{sanitized}-{hash4}"))
    }

    /// Return the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ProjectId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for ProjectId {
    type Error = IdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<ProjectId> for String {
    fn from(id: ProjectId) -> Self {
        id.0
    }
}

// ---------------------------------------------------------------------------
// MoleculeId — PREFIX-YYYYMMDD-XXXX
// ---------------------------------------------------------------------------

/// Validate a molecule-ID prefix.
///
/// A prefix may contain ASCII alphanumerics and internal hyphens, but must be
/// non-empty and neither begin nor end with a hyphen. Internal hyphens are
/// unambiguous because [`MoleculeId::parse_inner`] recovers the prefix by
/// splitting from the right, and the date/suffix segments never contain one.
/// This admits multi-word formula prefixes such as `bug-closure` and the
/// `drift-*` family (task-20260705-6c3a). Shared by `generate` and
/// `parse_inner` so the write path and the read path agree on what a valid
/// prefix is.
fn is_valid_molecule_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && !prefix.starts_with('-')
        && !prefix.ends_with('-')
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Identifies a molecule instance.
///
/// Format: `PREFIX-YYYYMMDD-XXXX` where PREFIX is a non-empty string of ASCII
/// alphanumerics and internal hyphens (it may neither begin nor end with a
/// hyphen), YYYYMMDD is a valid date, and XXXX is a non-empty alphanumeric
/// suffix.
///
/// The prefix is allowed to contain hyphens so multi-word formula prefixes —
/// `bug-closure`, the `drift-*` family — can nucleate. Parsing anchors on the
/// two trailing segments (date and suffix never contain a hyphen), so the
/// prefix is recovered unambiguously by splitting from the right. See
/// task-20260705-6c3a: a hyphenated `id_prefix` previously failed at ID
/// generation and silently blocked nucleation of those formulas.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MoleculeId {
    raw: String,
    prefix: String,
    date: String,
    suffix: String,
}

impl MoleculeId {
    /// Parse and validate a molecule ID string.
    ///
    /// # Errors
    /// Returns [`IdError`] if the string does not match `PREFIX-YYYYMMDD-XXXX`.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        Self::parse_inner(&s)
    }

    /// Generate a new molecule ID with today's date and a random 4-hex suffix.
    ///
    /// The prefix is typically derived from the formula's `id_prefix` field.
    /// The RNG is injected by the caller so the *entropy* source enters at the
    /// boundary (INV-DOMAIN-PURE-NO-IO, ADR-082). Note this function still
    /// reads the wall clock ([`Utc::now`]) for the date segment, so it is not
    /// yet fully pure — that ambient-time read is waiver W1, pending the same
    /// `Clock`-injection refactor that will thread time alongside the RNG.
    /// Tests can pass a seeded `StdRng` for determinism on the suffix;
    /// production callers pass an OS-backed RNG from an adapter crate.
    ///
    /// # Errors
    /// Returns [`IdError`] if the prefix is empty, contains characters other
    /// than ASCII alphanumerics and hyphens, or begins/ends with a hyphen.
    pub fn generate<R: Rng + ?Sized>(prefix: &str, rng: &mut R) -> Result<Self, IdError> {
        if !is_valid_molecule_prefix(prefix) {
            return Err(IdError::Invalid {
                kind: "MoleculeId",
                reason: format!(
                    "prefix must be non-empty ASCII alphanumeric with optional internal hyphens, got \"{prefix}\""
                ),
            });
        }
        let date = Utc::now().format("%Y%m%d");
        let suffix: u16 = rng.gen();
        let raw = format!("{prefix}-{date}-{suffix:04x}");
        Self::parse_inner(&raw)
    }

    fn parse_inner(s: &str) -> Result<Self, IdError> {
        if s.is_empty() {
            return Err(IdError::Empty { kind: "MoleculeId" });
        }

        // Split from the RIGHT into exactly 3 parts: PREFIX-YYYYMMDD-XXXX.
        // The prefix itself may contain hyphens (e.g. `bug-closure`), so we
        // anchor on the two trailing segments — the date (8 digits) and the
        // suffix (hex) never contain a hyphen — and treat everything before
        // them as the prefix. `rsplitn` yields the segments right-to-left.
        let mut it = s.rsplitn(3, '-');
        let (Some(suffix), Some(date), Some(prefix)) = (it.next(), it.next(), it.next()) else {
            return Err(IdError::Invalid {
                kind: "MoleculeId",
                reason: format!("expected PREFIX-YYYYMMDD-XXXX, got \"{s}\""),
            });
        };

        if !is_valid_molecule_prefix(prefix) {
            return Err(IdError::Invalid {
                kind: "MoleculeId",
                reason: format!(
                    "prefix must be non-empty ASCII alphanumeric with optional internal hyphens, got \"{prefix}\""
                ),
            });
        }

        if date.len() != 8 || !date.chars().all(|c| c.is_ascii_digit()) {
            return Err(IdError::Invalid {
                kind: "MoleculeId",
                reason: format!("date must be YYYYMMDD (8 digits), got \"{date}\""),
            });
        }

        // Basic date validation
        let year: u32 = date[..4].parse().unwrap_or(0);
        let month: u32 = date[4..6].parse().unwrap_or(0);
        let day: u32 = date[6..8].parse().unwrap_or(0);
        if year < 2000 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return Err(IdError::Invalid {
                kind: "MoleculeId",
                reason: format!("invalid date: {date}"),
            });
        }

        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(IdError::Invalid {
                kind: "MoleculeId",
                reason: format!("suffix must be non-empty alphanumeric, got \"{suffix}\""),
            });
        }

        Ok(Self {
            raw: s.to_owned(),
            prefix: prefix.to_owned(),
            date: date.to_owned(),
            suffix: suffix.to_owned(),
        })
    }

    /// Return the full ID string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Return the prefix part.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Return the date part (YYYYMMDD).
    #[must_use]
    pub fn date(&self) -> &str {
        &self.date
    }

    /// Return the suffix part.
    #[must_use]
    pub fn suffix(&self) -> &str {
        &self.suffix
    }
}

impl fmt::Display for MoleculeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for MoleculeId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_inner(s)
    }
}

impl TryFrom<String> for MoleculeId {
    type Error = IdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<MoleculeId> for String {
    fn from(id: MoleculeId) -> Self {
        id.raw
    }
}

// ---------------------------------------------------------------------------
// WorkerId — ep-{name} or bare {name}
// ---------------------------------------------------------------------------

/// Identifies a worker instance.
///
/// Valid formats: `ep-{name}` (ensemble-prefixed) or bare `{name}`.
/// Names must be non-empty and contain only ASCII alphanumeric characters and hyphens.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkerId(String);

impl WorkerId {
    /// Create a new worker ID, validating format.
    ///
    /// # Errors
    /// Returns [`IdError`] if the string is empty or contains invalid characters.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if s.is_empty() {
            return Err(IdError::Empty { kind: "WorkerId" });
        }

        // Strip optional "ep-" prefix for name validation
        let name = s.strip_prefix("ep-").unwrap_or(&s);

        if name.is_empty() {
            return Err(IdError::Invalid {
                kind: "WorkerId",
                reason: "name after 'ep-' prefix cannot be empty".to_owned(),
            });
        }

        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(IdError::Invalid {
                kind: "WorkerId",
                reason: format!(
                    "name must contain only ASCII alphanumeric or hyphens, got \"{name}\""
                ),
            });
        }

        // Name must not start or end with hyphen
        if name.starts_with('-') || name.ends_with('-') {
            return Err(IdError::Invalid {
                kind: "WorkerId",
                reason: format!("name must not start or end with hyphen, got \"{name}\""),
            });
        }

        Ok(Self(s))
    }

    /// Return the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return whether this is an ensemble-prefixed worker ID.
    #[must_use]
    pub fn has_ensemble_prefix(&self) -> bool {
        self.0.starts_with("ep-")
    }

    /// Return the bare name (without `ep-` prefix if present).
    #[must_use]
    pub fn name(&self) -> &str {
        self.0.strip_prefix("ep-").unwrap_or(&self.0)
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for WorkerId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for WorkerId {
    type Error = IdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<WorkerId> for String {
    fn from(id: WorkerId) -> Self {
        id.0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    // -- AgentId --

    #[test]
    fn test_agent_id_display_roundtrip() {
        let id = AgentId::new("witness").unwrap();
        let displayed = id.to_string();
        let parsed: AgentId = displayed.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_agent_id_rejects_empty() {
        assert!(AgentId::new("").is_err());
    }

    #[test]
    fn test_agent_id_serde_roundtrip() {
        let id = AgentId::new("refinery").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"refinery\"");
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // -- MoleculeId --

    #[test]
    fn test_molecule_id_parse_valid() {
        let id = MoleculeId::new("cs-20260401-hjdr").unwrap();
        assert_eq!(id.prefix(), "cs");
        assert_eq!(id.date(), "20260401");
        assert_eq!(id.suffix(), "hjdr");
        assert_eq!(id.to_string(), "cs-20260401-hjdr");
    }

    #[test]
    fn test_molecule_id_parse_invalid_rejects() {
        // No hyphens
        assert!(MoleculeId::new("nohyphens").is_err());
        // Only one hyphen
        assert!(MoleculeId::new("cs-20260401").is_err());
        // Bad date
        assert!(MoleculeId::new("cs-99991301-abc").is_err());
        // Empty prefix
        assert!(MoleculeId::new("-20260401-abc").is_err());
        // Empty suffix
        assert!(MoleculeId::new("cs-20260401-").is_err());
        // Empty string
        assert!(MoleculeId::new("").is_err());
        // Non-alphanumeric prefix
        assert!(MoleculeId::new("c!s-20260401-abc").is_err());
        // Date too short
        assert!(MoleculeId::new("cs-2026040-abc").is_err());
        // Trailing hyphen on the prefix would leave an empty prefix word
        assert!(MoleculeId::new("bug--20260401-abc").is_err());
        // Leading hyphen on the prefix
        assert!(MoleculeId::new("-bug-20260401-abc").is_err());
    }

    /// A hyphenated `id_prefix` (`bug-closure`, the `drift-*` family) must
    /// parse: the prefix is everything left of the date/suffix, recovered by
    /// splitting from the right. Regression for task-20260705-6c3a, where a
    /// hyphenated prefix silently blocked nucleation.
    #[test]
    fn test_molecule_id_parse_hyphenated_prefix() {
        let id = MoleculeId::new("bug-closure-20260705-a1b2").unwrap();
        assert_eq!(id.prefix(), "bug-closure");
        assert_eq!(id.date(), "20260705");
        assert_eq!(id.suffix(), "a1b2");
        assert_eq!(id.to_string(), "bug-closure-20260705-a1b2");

        // Multi-hyphen prefix (the deepest drift-* name).
        let drift = MoleculeId::new("drift-project-frozen-20260705-00ff").unwrap();
        assert_eq!(drift.prefix(), "drift-project-frozen");
        assert_eq!(drift.date(), "20260705");
        assert_eq!(drift.suffix(), "00ff");
    }

    /// `generate` must accept a hyphenated prefix and produce an ID that
    /// round-trips back to the same prefix.
    #[test]
    fn test_molecule_id_generate_hyphenated_prefix() {
        let mut rng = StdRng::seed_from_u64(7);
        let id = MoleculeId::generate("bug-closure", &mut rng).unwrap();
        assert_eq!(id.prefix(), "bug-closure");
        let reparsed = MoleculeId::new(id.as_str()).unwrap();
        assert_eq!(reparsed, id);
        // A leading/trailing hyphen is still rejected at generation.
        assert!(MoleculeId::generate("-bad", &mut rng).is_err());
        assert!(MoleculeId::generate("bad-", &mut rng).is_err());
        assert!(MoleculeId::generate("", &mut rng).is_err());
    }

    #[test]
    fn test_molecule_id_display_roundtrip() {
        let id = MoleculeId::new("wisp-20260401-abcd").unwrap();
        let displayed = id.to_string();
        let parsed: MoleculeId = displayed.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_molecule_id_serde_roundtrip() {
        let id = MoleculeId::new("cs-20260401-hjdr").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"cs-20260401-hjdr\"");
        let back: MoleculeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // -- WorkerId --

    #[test]
    fn test_worker_id_bare_name() {
        let id = WorkerId::new("quartz").unwrap();
        assert_eq!(id.as_str(), "quartz");
        assert!(!id.has_ensemble_prefix());
        assert_eq!(id.name(), "quartz");
    }

    #[test]
    fn test_worker_id_ensemble_prefixed() {
        let id = WorkerId::new("ep-quartz").unwrap();
        assert_eq!(id.as_str(), "ep-quartz");
        assert!(id.has_ensemble_prefix());
        assert_eq!(id.name(), "quartz");
    }

    #[test]
    fn test_worker_id_rejects_empty() {
        assert!(WorkerId::new("").is_err());
    }

    #[test]
    fn test_worker_id_rejects_empty_after_prefix() {
        assert!(WorkerId::new("ep-").is_err());
    }

    #[test]
    fn test_worker_id_rejects_invalid_chars() {
        assert!(WorkerId::new("qu@rtz").is_err());
    }

    #[test]
    fn test_worker_id_display_roundtrip() {
        let id = WorkerId::new("ep-quartz").unwrap();
        let displayed = id.to_string();
        let parsed: WorkerId = displayed.parse().unwrap();
        assert_eq!(id, parsed);
    }

    // -- ConvoyId --

    #[test]
    fn test_convoy_id_roundtrip() {
        let id = ConvoyId::new("convoy-alpha").unwrap();
        let parsed: ConvoyId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_convoy_id_rejects_empty() {
        assert!(ConvoyId::new("").is_err());
    }

    #[test]
    fn test_convoy_id_serde_roundtrip() {
        let id = ConvoyId::new("convoy-beta").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"convoy-beta\"");
        let back: ConvoyId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // -- ChamberId --

    #[test]
    fn test_chamber_id_roundtrip() {
        let id = ChamberId::new("ch-001").unwrap();
        let parsed: ChamberId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_chamber_id_rejects_empty() {
        assert!(ChamberId::new("").is_err());
    }

    #[test]
    fn test_chamber_id_serde_roundtrip() {
        let id = ChamberId::new("ch-sealed-42").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"ch-sealed-42\"");
        let back: ChamberId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // -- RigId --

    #[test]
    fn test_rig_id_roundtrip() {
        let id = RigId::new("cosmon").unwrap();
        let parsed: RigId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_rig_id_rejects_empty() {
        assert!(RigId::new("").is_err());
    }

    #[test]
    fn test_rig_id_serde_roundtrip() {
        let id = RigId::new("gastown").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"gastown\"");
        let back: RigId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // -- FormulaId, SessionId, StepId --

    #[test]
    fn test_formula_id_roundtrip() {
        let id = FormulaId::new("mol-polecat-work").unwrap();
        let parsed: FormulaId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_session_id_roundtrip() {
        let id = SessionId::new("c26d059e-415d-466f-97d4-ea84d4b9c027").unwrap();
        let parsed: SessionId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_step_id_roundtrip() {
        let id = StepId::new("step-1-load-context").unwrap();
        let parsed: StepId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    // -- ProjectId --

    #[test]
    fn test_project_id_roundtrip() {
        let id = ProjectId::new("cosmon-a1b2").unwrap();
        let parsed: ProjectId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_project_id_rejects_empty() {
        assert!(ProjectId::new("").is_err());
    }

    #[test]
    fn test_project_id_rejects_no_hyphen() {
        assert!(ProjectId::new("cosmon").is_err());
    }

    #[test]
    fn test_project_id_serde_roundtrip() {
        let id = ProjectId::new("cosmon-a1b2").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"cosmon-a1b2\"");
        let back: ProjectId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn test_project_id_generate() {
        let id = ProjectId::generate(std::path::Path::new("/tmp/my-project"));
        assert!(id.as_str().starts_with("my-project-"));
        // dirname (10) + hyphen (1) + hash4 (4) = 15
        assert_eq!(id.as_str().len(), 15);
    }

    #[test]
    fn test_project_id_generate_deterministic() {
        let p = std::path::Path::new("/home/user/dev/cosmon");
        let a = ProjectId::generate(p);
        let b = ProjectId::generate(p);
        assert_eq!(a, b);
    }

    #[test]
    fn test_project_id_generate_sanitizes_dirname() {
        let id = ProjectId::generate(std::path::Path::new("/tmp/My Project!"));
        // "My Project!" → "my-project-" → trimmed → "my-project"
        assert!(id.as_str().starts_with("my-project-"));
    }

    // -- Type safety: compile-time test --
    // This test proves that AgentId and WorkerId are distinct types.
    // If this compiles, the type system prevents mixing them up.
    #[test]
    fn test_worker_id_is_not_a_string() {
        fn takes_agent_id(_: &AgentId) {}

        let agent = AgentId::new("witness").unwrap();
        takes_agent_id(&agent);

        // The following would NOT compile (proving type safety):
        // let worker = WorkerId::new("quartz").unwrap();
        // takes_agent_id(&worker); // ERROR: expected &AgentId, found &WorkerId
    }
}
