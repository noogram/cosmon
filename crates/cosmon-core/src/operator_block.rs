// SPDX-License-Identifier: AGPL-3.0-only

//! Typed operator-block capability and its irreversibility boundary
//! (ADR-123 — the operator-block doctrine).
//!
//! # Why this module exists
//!
//! The motivating incident: a worker faced two briefing imperatives that
//! read as a contradiction —
//!
//! - generic: *"DO NOT wait for user input between steps"*;
//! - task-discipline: *"don't edit the signable act without an operator
//!   decision."*
//!
//! It resolved the conflict by guessing, blocked through a Claude Code
//! `AskUserQuestion` modal (a surface **external** to cosmon's state
//! machine), and the DAG drainage stalled all night, invisible.
//!
//! ADR-123 dissolves the contradiction by making the task-discipline a
//! **typed guard on the molecule** rather than a paragraph competing with
//! another paragraph. A worker reads exactly one instruction:
//!
//! - capability **absent** ⇒ surface-and-continue (the safe default); the
//!   generic *"DO NOT wait"* wins by construction because there is nothing
//!   to arbitrate;
//! - capability **present** ⇒ the worker MAY block, but ONLY at the
//!   declared [`IrreversibleBoundary`], and ONLY after emitting the typed
//!   block signal ([`crate::event_v2::EventV2::WorkerBlockedOnOperator`]).
//!   *Blocking without emitting is a protocol violation* (kahneman).
//!
//! # Encoding (ADR-123 Q4 = option (b))
//!
//! Both the capability **grant** and the block **signal** are encoded as
//! typed tags + an append-only event — *not* as a new `MoleculeStatus`
//! variant (rejected: adds weight to a `#[doc(hidden)]` legacy type) and
//! *not* as a `RunState` witness (rejected: premature, the storage
//! migration is incomplete). The molecule **stays `Running`**; "waiting on
//! a human" is a transient annotation, peer to `temp:frozen`.
//!
//! - The capability grant is the tag [`OperatorBlockCapability::to_tag`]
//!   (`op-block:<boundary>`), stamped at `cs nucleate`.
//! - The block signal is the event
//!   [`crate::event_v2::EventV2::WorkerBlockedOnOperator`] **plus** the tag
//!   [`AWAITING_OP_TAG`] (`temp:awaiting-op`), written by `cs await-operator`.
//!
//! # Belt-and-suspenders with the external backstop (C1)
//!
//! This is the *worker-emitted* half. The *un-emitting* case (a worker
//! parked at a modal that emits nothing) is caught by the external
//! event-age patrol (`cs patrol --event-age`). The two agree on names:
//! `cs await-operator`
//! stamps [`IrreversibleBoundary::alert_tag`] so the patrol's
//! irreversible-class router (`signature` / `push` / `publish` /
//! `irreversible`) fires `cs notify --level alert`.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::agent::ParseEnumError;
use crate::tag::Tag;

/// The tag key used to encode the operator-block capability grant.
///
/// Full tag form: `op-block:<boundary>`, e.g. `op-block:signature`.
pub const CAPABILITY_TAG_KEY: &str = "op-block";

/// The derived surface marker projected while a worker is blocked on an
/// operator decision (ADR-123 Q4). Read by `cs peek`,
/// `cs ensemble --tag`, and `STATUS.md`; the molecule stays `Running`.
pub const AWAITING_OP_TAG: &str = "temp:awaiting-op";

/// The irreversibility boundary at which a worker is authorised to pause
/// for an operator decision (ADR-123 Q1).
///
/// The line is **not** "this artifact is valuable" — almost everything a
/// worker does on an unmerged worktree is reversible by `git` + `cs`.
/// The line is crossed only by an effect cosmon **cannot revert** with
/// `git` + `cs`: a signature transmitted, a push to a shared remote, an
/// email/publish sent, or an authoritative value downstream consumers act
/// on before a human reviews it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IrreversibleBoundary {
    /// A cryptographic / legal signature is about to be transmitted.
    Signature,
    /// An effect leaves the worktree for a shared destination — a `git
    /// push` to a shared remote, or any outbound send the recipient acts
    /// on (email, message, webhook).
    ExternalSend,
    /// Content is about to be published to a durable, externally-visible
    /// surface (a release, a public document, a package registry).
    Publish,
    /// An authoritative value is about to be written that downstream
    /// consumers act on before a human reviews it.
    AuthoritativeValue,
}

impl IrreversibleBoundary {
    /// The canonical kebab-case wire form (matches the serde
    /// representation and the `op-block:<value>` tag value).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::ExternalSend => "external-send",
            Self::Publish => "publish",
            Self::AuthoritativeValue => "authoritative-value",
        }
    }

    /// The bare tag the external event-age patrol (C1) recognises as
    /// irreversible-class, so a worker-emitted block routes to
    /// `cs notify --level alert` even via the un-emitting backstop.
    ///
    /// Every boundary maps onto the C1 recognised set
    /// (`signature` / `push` / `publish` / `irreversible`); the agreement
    /// on these names is the belt-and-suspenders contract with the external
    /// event-age patrol.
    #[must_use]
    pub const fn alert_tag(self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::ExternalSend => "push",
            Self::Publish => "publish",
            Self::AuthoritativeValue => "irreversible",
        }
    }
}

impl fmt::Display for IrreversibleBoundary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for IrreversibleBoundary {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept both kebab-case (`external-send`, wire form) and
        // snake_case (`external_send`) for ergonomic CLI input.
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "signature" | "sign" => Ok(Self::Signature),
            "external-send" | "send" | "push" | "email" => Ok(Self::ExternalSend),
            "publish" | "release" => Ok(Self::Publish),
            "authoritative-value" | "authoritative" | "value" => Ok(Self::AuthoritativeValue),
            _ => Err(ParseEnumError {
                type_name: "IrreversibleBoundary",
                value: s.to_owned(),
            }),
        }
    }
}

/// May this molecule pause for an operator decision, and at which
/// boundary? (ADR-123 Q5 — the typed guard.)
///
/// Absent ⇒ the worker MUST surface-and-continue (the safe default).
/// There is no contradiction to arbitrate: the generic *"DO NOT wait"*
/// wins by construction because the capability simply is not present.
///
/// Present ⇒ the worker MAY block, but ONLY at [`Self::boundary`], and
/// ONLY after emitting the typed block signal. **Blocking without
/// emitting is a protocol violation, not a judgment call.**
///
/// # Persistence
///
/// Granted at `cs nucleate` and encoded as the tag [`Self::to_tag`]
/// (`op-block:<boundary>`), so it is visible in `state.json`'s `tags`
/// array without adding a field to every `MoleculeData` construction
/// site. Reconstructed with [`Self::from_tags`].
///
/// > **Deliberate non-field (ADR-123 §Consequences).** No
/// > `timeout_to_default` that auto-*applies* a value: AUTO-DEFAULT on an
/// > irreversible act is forbidden (Q2/CV-4) — its worst case (a wrong
/// > value signed/pushed) is the only unrecoverable outcome. A bound, if
/// > any, escalates the ALERT or parks the molecule; it never applies a
/// > value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OperatorBlockCapability {
    /// The irreversibility boundary that authorises a pause.
    boundary: IrreversibleBoundary,
}

impl OperatorBlockCapability {
    /// Construct a capability authorising a pause at `boundary`.
    #[must_use]
    pub const fn new(boundary: IrreversibleBoundary) -> Self {
        Self { boundary }
    }

    /// The boundary at which this capability authorises a pause.
    #[must_use]
    pub const fn boundary(self) -> IrreversibleBoundary {
        self.boundary
    }

    /// The canonical tag encoding the grant — `op-block:<boundary>`.
    ///
    /// # Panics
    ///
    /// Never in practice: the constructed string is always a valid
    /// [`Tag`] (kebab-case key, kebab-case value). The `expect` documents
    /// the invariant for a future reader.
    #[must_use]
    pub fn to_tag(self) -> Tag {
        Tag::new(format!("{CAPABILITY_TAG_KEY}:{}", self.boundary.as_str()))
            .expect("op-block:<boundary> is always a valid tag")
    }

    /// Parse a capability from a single tag, if it is an `op-block:*`
    /// grant with a recognised boundary value. Returns `None` for any
    /// other tag.
    #[must_use]
    pub fn from_tag(tag: &Tag) -> Option<Self> {
        if tag.key() != CAPABILITY_TAG_KEY {
            return None;
        }
        let boundary = tag.value()?.parse().ok()?;
        Some(Self { boundary })
    }

    /// Reconstruct the capability from a molecule's tag set, if any tag
    /// grants it. When more than one `op-block:*` tag is present (a
    /// malformed grant), the lexically-first boundary wins for
    /// determinism.
    #[must_use]
    pub fn from_tags(tags: &BTreeSet<Tag>) -> Option<Self> {
        tags.iter().find_map(Self::from_tag)
    }
}

impl fmt::Display for OperatorBlockCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{CAPABILITY_TAG_KEY}:{}", self.boundary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_roundtrips_through_str() {
        for b in [
            IrreversibleBoundary::Signature,
            IrreversibleBoundary::ExternalSend,
            IrreversibleBoundary::Publish,
            IrreversibleBoundary::AuthoritativeValue,
        ] {
            assert_eq!(b.as_str().parse::<IrreversibleBoundary>().unwrap(), b);
        }
    }

    #[test]
    fn boundary_from_str_accepts_synonyms_and_snake_case() {
        assert_eq!(
            "sign".parse::<IrreversibleBoundary>().unwrap(),
            IrreversibleBoundary::Signature
        );
        assert_eq!(
            "external_send".parse::<IrreversibleBoundary>().unwrap(),
            IrreversibleBoundary::ExternalSend
        );
        assert_eq!(
            "release".parse::<IrreversibleBoundary>().unwrap(),
            IrreversibleBoundary::Publish
        );
        assert!("nonsense".parse::<IrreversibleBoundary>().is_err());
    }

    #[test]
    fn every_boundary_alert_tag_is_c1_recognised() {
        // The belt-and-suspenders contract with task-20260608-014f: each
        // boundary's alert tag must be in C1's irreversible-class set.
        let c1_recognised = [
            "signature",
            "sign",
            "push",
            "publish",
            "release",
            "irreversible",
        ];
        for b in [
            IrreversibleBoundary::Signature,
            IrreversibleBoundary::ExternalSend,
            IrreversibleBoundary::Publish,
            IrreversibleBoundary::AuthoritativeValue,
        ] {
            assert!(
                c1_recognised.contains(&b.alert_tag()),
                "{b} alert_tag `{}` not in C1 recognised set",
                b.alert_tag()
            );
            // And the alert tag must be a valid bare tag.
            assert!(Tag::new(b.alert_tag()).is_ok());
        }
    }

    #[test]
    fn capability_roundtrips_through_tag() {
        for b in [
            IrreversibleBoundary::Signature,
            IrreversibleBoundary::ExternalSend,
            IrreversibleBoundary::Publish,
            IrreversibleBoundary::AuthoritativeValue,
        ] {
            let cap = OperatorBlockCapability::new(b);
            let tag = cap.to_tag();
            assert_eq!(OperatorBlockCapability::from_tag(&tag), Some(cap));
        }
    }

    #[test]
    fn from_tags_finds_grant_among_noise() {
        let mut tags = BTreeSet::new();
        tags.insert(Tag::new("temp:hot").unwrap());
        tags.insert(Tag::new("area:cli").unwrap());
        assert_eq!(OperatorBlockCapability::from_tags(&tags), None);

        tags.insert(OperatorBlockCapability::new(IrreversibleBoundary::Signature).to_tag());
        assert_eq!(
            OperatorBlockCapability::from_tags(&tags),
            Some(OperatorBlockCapability::new(
                IrreversibleBoundary::Signature
            ))
        );
    }

    #[test]
    fn unrelated_tag_is_not_a_capability() {
        assert_eq!(
            OperatorBlockCapability::from_tag(&Tag::new("temp:warm").unwrap()),
            None
        );
        // `op-block` with no/invalid value is not a grant.
        assert_eq!(
            OperatorBlockCapability::from_tag(&Tag::new("op-block").unwrap()),
            None
        );
        assert_eq!(
            OperatorBlockCapability::from_tag(&Tag::new("op-block:bogus").unwrap()),
            None
        );
    }
}
