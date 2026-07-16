// SPDX-License-Identifier: AGPL-3.0-only

//! Typed destination for a notarized prompt (ADR-059).
//!
//! A [`PromptDestination`] is the triple an operator commits to at mint
//! time: `(residence, branch, genre)`. It replaces the free-form
//! `PathBuf` that previously identified where a prompt was headed. The
//! triple makes wheeler's detachment functor `U(M ⊕ σ) = M` hold
//! **structurally**: any rename/move that lands the prompt in a
//! different triple invalidates the Seal, surfacing exactly what the
//! notary was meant to detect.
//!
//! This module is a **shape declaration** — it defines the types, their
//! canonical encoding, and their validation rules, but does not wire
//! them into `cs notarize` or the `Commitment` schema. The notary-side
//! adoption and the canonical-form v2 bump are deferred follow-ups (see
//! ADR-059 §4).
//!
//! # Axes (ADR-059 §2.1)
//!
//! - [`Residence`] — a mint-time classifier snapshot of ADR-055's
//!   residence axis. Flattened on purpose: at mint time we record
//!   which variant was in force, not its full configuration (repo URL,
//!   age recipients, …).
//! - [`BranchName`] — a validated non-empty branch name. Git
//!   ref-format grammar is **not** enforced here; the git command
//!   remains ground truth if it ever matters.
//! - [`Genre`] — the six v0 genres from ADR-057 §2.2. `Addl` carries a
//!   [`PartnerName`] capture (the partner component of the addl path).
//!
//! # Canonical encoding
//!
//! [`PromptDestination`] derives `Serialize` / `Deserialize`. Fields
//! encode as `snake_case`; `Genre::Addl` encodes as
//! `{"addl": "<partner>"}`. No floats, no optional fields, no nested
//! objects — the whole struct must hash deterministically under the
//! same canonical-form v1 rules as ADR-056.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::id::IdError;

/// The triple that locates a notarized prompt at mint time.
///
/// See the module docs for the full story; the fields map 1:1 to the
/// three axes ADR-059 §2.1 names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PromptDestination {
    /// Which social contract governs the bytes (ADR-055).
    pub residence: Residence,
    /// Which git branch holds them at mint time.
    pub branch: BranchName,
    /// What kind of artifact this is, and by extension who its audience
    /// is (ADR-057).
    pub genre: Genre,
}

impl PromptDestination {
    /// Construct a destination from its three axes.
    ///
    /// No cross-field validation is performed at this stage: any
    /// combination of residence/branch/genre is representable, because
    /// the destination records *what the operator committed to*, not
    /// *what the operator should have committed to*. Higher-level
    /// coherence checks (e.g. a `github-surface` genre on a `remote`
    /// residence) belong to `cs notarize verify`, not to the
    /// constructor.
    #[must_use]
    pub fn new(residence: Residence, branch: BranchName, genre: Genre) -> Self {
        Self {
            residence,
            branch,
            genre,
        }
    }
}

/// Mint-time classifier snapshot of ADR-055's residence axis.
///
/// Flattened on purpose. The galaxy's full residence (including repo
/// URL, age recipients, or remote endpoint) lives in
/// `.cosmon/config.toml`; the notary records only which variant was
/// in force when the Seal was produced. A future unification pass
/// will collapse this enum with ADR-055's once the latter lands its
/// Rust form in `cosmon-core` — see ADR-059 §4 *Open*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Residence {
    /// Single operator, local filesystem only.
    Solo,
    /// Shared git remote; narration tracked, live-state gitignored.
    Team,
    /// Team + age-encrypted narration.
    Encrypted,
    /// Narration hosted by cosmon-saas over HTTP.
    Remote,
}

impl Residence {
    /// Operator-facing string form. Round-trips with [`Residence::from_str`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Residence::Solo => "solo",
            Residence::Team => "team",
            Residence::Encrypted => "encrypted",
            Residence::Remote => "remote",
        }
    }
}

impl fmt::Display for Residence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str((*self).as_str())
    }
}

impl FromStr for Residence {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "solo" => Ok(Residence::Solo),
            "team" => Ok(Residence::Team),
            "encrypted" => Ok(Residence::Encrypted),
            "remote" => Ok(Residence::Remote),
            other => Err(IdError::Invalid {
                kind: "Residence",
                reason: format!("unknown residence '{other}'"),
            }),
        }
    }
}

/// The six v0 genres declared by ADR-057 §2.2.
///
/// `Addl` carries the partner component captured from the glob; every
/// other variant is data-free because its audience does not vary with
/// the path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Genre {
    /// Operator narration (chronicles).
    Chronicle,
    /// Architecture Decision Records.
    Adr,
    /// Partner deliverables — the inner [`PartnerName`] is the `<name>`
    /// component captured from the path glob.
    Addl(PartnerName),
    /// Regenerable GitHub mirror (docs/surfaces/*, STATUS.md, ISSUES.md).
    #[serde(rename = "github-surface")]
    GithubSurface,
    /// Multi-persona panel synthesis.
    Deliberation,
    /// Everything else — the catch-all.
    Code,
}

impl Genre {
    /// Operator-facing string form without the partner capture
    /// (e.g. `Addl(PartnerName("operator-b"))` → `"addl"`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Genre::Chronicle => "chronicle",
            Genre::Adr => "adr",
            Genre::Addl(_) => "addl",
            Genre::GithubSurface => "github-surface",
            Genre::Deliberation => "deliberation",
            Genre::Code => "code",
        }
    }
}

impl fmt::Display for Genre {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Genre::Addl(name) => write!(f, "addl:{name}"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// A git branch name — non-empty, otherwise opaque.
///
/// Git's full `refs/heads/*` grammar is **not** enforced here: the
/// destination records what the operator committed to, and if that
/// name fails `git check-ref-format`, the commit itself is what
/// surfaces the error, not the notary. This decision may be revisited
/// once the notary learns to cross-check branches against the live git
/// index (ADR-059 §4 *Open*).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BranchName(String);

impl BranchName {
    /// Construct a validated branch name.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Empty`] when the string is empty after
    /// trimming.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(IdError::Empty { kind: "BranchName" });
        }
        Ok(Self(s))
    }

    /// Borrow the underlying branch name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BranchName {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for BranchName {
    type Error = IdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<BranchName> for String {
    fn from(b: BranchName) -> Self {
        b.0
    }
}

/// A partner identifier — the captured `<name>` component of an
/// `addl/<name>/**/*` glob.
///
/// Shape matches [`BranchName`]: non-empty, otherwise opaque. Partner
/// naming conventions (ASCII, kebab-case, …) are a policy concern the
/// `artifact-map.toml` glob resolves, not a type-level invariant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PartnerName(String);

impl PartnerName {
    /// Construct a validated partner name.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::Empty`] when the string is empty after
    /// trimming.
    pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(IdError::Empty {
                kind: "PartnerName",
            });
        }
        Ok(Self(s))
    }

    /// Borrow the underlying partner name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PartnerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PartnerName {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for PartnerName {
    type Error = IdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<PartnerName> for String {
    fn from(p: PartnerName) -> Self {
        p.0
    }
}
