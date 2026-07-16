// SPDX-License-Identifier: AGPL-3.0-only

//! Interaction mode — does this molecule require the operator's attention?
//!
//! `InteractionMode` is a *static discriminant* posed at nucleation by the
//! author of the molecule (often an agent — the author has Type 2 cognition
//! available at write time even when the operator does not at dispatch
//! time). It is read at dispatch and survives the operator's present
//! state. It is **not** a dynamic signal about the operator.
//!
//! Two values, both explicit. There is no default — a molecule either
//! carries the tag or it does not. Consumers (the graceful degradation
//! controller, in particular) decide what to do when the tag is absent;
//! the tag itself is purely a labeling primitive.
//!
//! # Wire format
//!
//! `InteractionMode` projects to a [`Tag`] with key `interaction-mode`
//! and value `operator-required` or `background`. Persistence is the
//! existing [`MoleculeData::tags`](../../cosmon_state/struct.MoleculeData.html#structfield.tags)
//! `BTreeSet<Tag>`; no new state field is introduced.
//!
//! ```text
//! interaction-mode:operator-required
//! interaction-mode:background
//! ```
//!
//! # Convention
//!
//! - **operator-required** — tackling consumes operator exergy: the
//!   work cannot proceed without the operator's attention (verdict-door,
//!   review of a draft, decision that only the operator can make).
//! - **background** — the work proceeds without operator presence
//!   (compute-bound, agent-only, machine-counterparty).
//!
//! ```
//! use cosmon_core::interaction_mode::{InteractionMode, INTERACTION_MODE_TAG_KEY};
//! use cosmon_core::tag::Tag;
//! use std::collections::BTreeSet;
//!
//! let tag: Tag = InteractionMode::OperatorRequired.to_tag();
//! assert_eq!(tag.key(), INTERACTION_MODE_TAG_KEY);
//! assert_eq!(tag.value(), Some("operator-required"));
//!
//! let mut tags: BTreeSet<Tag> = BTreeSet::new();
//! tags.insert(tag);
//! assert_eq!(
//!     InteractionMode::from_tag_set(&tags),
//!     Some(InteractionMode::OperatorRequired)
//! );
//! ```

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::tag::Tag;

/// The canonical tag key under which [`InteractionMode`] is stored.
///
/// Held as a `&'static str` so callers can match against a tag's key
/// without allocating.
pub const INTERACTION_MODE_TAG_KEY: &str = "interaction-mode";

/// Static discriminant on a molecule: does tackling it require the
/// operator's attention, or can it proceed in the background?
///
/// Posed by the molecule's author at nucleation, never derived from the
/// operator's present state. *Invariant statique sur la molécule.*
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InteractionMode {
    /// Tackling this molecule requires the operator's attention. Examples:
    /// verdict-door decisions, drafts requiring review, choices only the
    /// operator can make. Consumes operator exergy.
    OperatorRequired,
    /// Tackling this molecule does not require the operator's attention.
    /// Examples: compute-bound research, agent-only refactors, machine
    /// counterparty interactions. Background-safe under graceful
    /// degradation.
    Background,
}

/// Errors returned when parsing an [`InteractionMode`] from a string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InteractionModeError {
    /// The input did not match `operator-required` or `background`.
    #[error(
        "interaction mode `{0}` is not recognised (expected `operator-required` or `background`)"
    )]
    Unknown(String),
}

impl InteractionMode {
    /// The canonical kebab-case spelling — also the value used in the tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OperatorRequired => "operator-required",
            Self::Background => "background",
        }
    }

    /// Project this mode into a [`Tag`] of the form
    /// `interaction-mode:<mode>`.
    ///
    /// # Panics
    /// Never — the constructed string is always a valid tag.
    #[must_use]
    pub fn to_tag(self) -> Tag {
        let raw = format!("{INTERACTION_MODE_TAG_KEY}:{}", self.as_str());
        Tag::new(raw).expect("InteractionMode::to_tag produces a valid tag")
    }

    /// Read a mode from a single tag, returning `None` for tags that do
    /// not carry the `interaction-mode` key or for unknown values.
    ///
    /// Unknown values produce `None` rather than an error so callers can
    /// scan a heterogeneous tag set without short-circuiting on a tag
    /// that happens to share the key with a future variant.
    #[must_use]
    pub fn from_tag(tag: &Tag) -> Option<Self> {
        if tag.key() != INTERACTION_MODE_TAG_KEY {
            return None;
        }
        tag.value().and_then(|v| v.parse().ok())
    }

    /// Read the first recognised mode from a tag iterable.
    ///
    /// Consumes from a `&BTreeSet<Tag>` (the canonical persistence form)
    /// or any other iterator over tag references. If multiple
    /// `interaction-mode:*` tags are present, the lexicographically
    /// first one wins — `BTreeSet` ordering already pins this. In
    /// practice, declarations only carry one (CLI rejects more than
    /// one).
    pub fn from_tag_set<'a, I>(tags: I) -> Option<Self>
    where
        I: IntoIterator<Item = &'a Tag>,
    {
        tags.into_iter().find_map(Self::from_tag)
    }
}

impl fmt::Display for InteractionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for InteractionMode {
    type Err = InteractionModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "operator-required" => Ok(Self::OperatorRequired),
            "background" => Ok(Self::Background),
            other => Err(InteractionModeError::Unknown(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn round_trip_operator_required() {
        let tag = InteractionMode::OperatorRequired.to_tag();
        assert_eq!(tag.key(), INTERACTION_MODE_TAG_KEY);
        assert_eq!(tag.value(), Some("operator-required"));
        assert_eq!(
            InteractionMode::from_tag(&tag),
            Some(InteractionMode::OperatorRequired)
        );
    }

    #[test]
    fn round_trip_background() {
        let tag = InteractionMode::Background.to_tag();
        assert_eq!(tag.value(), Some("background"));
        assert_eq!(
            InteractionMode::from_tag(&tag),
            Some(InteractionMode::Background)
        );
    }

    #[test]
    fn from_tag_returns_none_for_unrelated_key() {
        let tag = Tag::new("priority:high").unwrap();
        assert_eq!(InteractionMode::from_tag(&tag), None);
    }

    #[test]
    fn from_tag_returns_none_for_unknown_value() {
        let tag = Tag::new("interaction-mode:operator-saturated").unwrap();
        assert_eq!(InteractionMode::from_tag(&tag), None);
    }

    #[test]
    fn from_tag_returns_none_for_bare_key() {
        let tag = Tag::new("interaction-mode").unwrap();
        assert_eq!(InteractionMode::from_tag(&tag), None);
    }

    #[test]
    fn from_tag_set_picks_canonical_tag() {
        let mut tags: BTreeSet<Tag> = BTreeSet::new();
        tags.insert(Tag::new("priority:high").unwrap());
        tags.insert(InteractionMode::Background.to_tag());
        tags.insert(Tag::new("area:cli").unwrap());
        assert_eq!(
            InteractionMode::from_tag_set(&tags),
            Some(InteractionMode::Background)
        );
    }

    #[test]
    fn from_tag_set_returns_none_when_absent() {
        let mut tags: BTreeSet<Tag> = BTreeSet::new();
        tags.insert(Tag::new("priority:high").unwrap());
        assert_eq!(InteractionMode::from_tag_set(&tags), None);
    }

    #[test]
    fn from_str_round_trip() {
        for mode in [
            InteractionMode::OperatorRequired,
            InteractionMode::Background,
        ] {
            let s = mode.to_string();
            let parsed: InteractionMode = s.parse().unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn from_str_rejects_unknown() {
        let err: Result<InteractionMode, _> = "operator-maybe".parse();
        assert!(matches!(err, Err(InteractionModeError::Unknown(ref s)) if s == "operator-maybe"));
    }

    #[test]
    fn serde_round_trip_kebab_case() {
        // Wire form is kebab-case to match the tag value spelling. This
        // is what JSON callers (the cosmon-cockpit-http API and friends)
        // observe when an `InteractionMode` appears in a payload.
        let json = serde_json::to_string(&InteractionMode::OperatorRequired).unwrap();
        assert_eq!(json, "\"operator-required\"");
        let back: InteractionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, InteractionMode::OperatorRequired);
    }
}
