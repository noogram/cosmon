// SPDX-License-Identifier: AGPL-3.0-only

//! Molecule interactions — inter-molecule operations.
//!
//! An interaction is a domain event where molecules relate to each other:
//! one decays into many, many merge into one, or one transforms its kind.
//! These are the "reactions" of the cosmon universe.
//!
//! Interactions are **explicit** — triggered by agent or operator decision,
//! never automatic. This follows the Anti-Psychosis Principle (THESIS Part XIV):
//! the observer controls the amplification.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::MoleculeId;
use crate::kind::MoleculeKind;

/// A reference to a molecule in another galaxy, identified by a
/// human-readable alias (Phase 1 of [ADR-035](../../docs/adr/035-cross-galaxy-edges.md)).
///
/// The alias is what the operator types on the CLI (`mailroom`,
/// `tenant-demo`, …); resolution to a filesystem path is performed at the
/// CLI layer via a registry lookup with a convention fallback. The
/// content-addressed `GalaxyHash` form described by ADR-035 §1 is
/// deferred to Phase 2 — Phase 1 keeps the alias as authoritative so
/// the typed edge can land before the genesis-hash machinery exists.
///
/// String form on the CLI:
/// - `<alias>:<molecule-id>` (canonical, e.g. `mailroom:idea-9f3c2a01`)
/// - `<alias>@<molecule-id>` (alternate, ergonomic for shells)
///
/// JSON form (in `state.json`):
/// `{ "galaxy": "<alias>", "mol_id": "<molecule-id>" }`
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CrossGalaxyRef {
    /// Galaxy alias (e.g. `mailroom`). Resolved to a filesystem
    /// path by the CLI's resolver (see
    /// `cosmon_cli::cmd::nucleate::cross_galaxy::resolve_galaxy_path`).
    pub galaxy: String,
    /// Local molecule ID *within* the target galaxy. Same shape as a
    /// regular [`MoleculeId`].
    pub mol_id: MoleculeId,
}

/// Sentinel separator characters accepted between `<alias>` and
/// `<molecule-id>` in the CLI parser. Both `:` and `@` are allowed —
/// the colon is canonical and matches Linux-style scheme separators;
/// the at-sign mirrors ADR-035 §2 (`mol@galaxy` syntax) flipped to
/// the operator-friendly `galaxy@mol` reading order.
const CROSS_GALAXY_SEPARATORS: [char; 2] = [':', '@'];

impl CrossGalaxyRef {
    /// Build a new cross-galaxy reference. The caller is expected to
    /// have validated that the alias is non-empty and that the molecule
    /// id parses; this constructor is a thin wrapper that does not
    /// re-validate (use [`FromStr`] for parsing-and-validating in one
    /// step).
    #[must_use]
    pub fn new(galaxy: impl Into<String>, mol_id: MoleculeId) -> Self {
        Self {
            galaxy: galaxy.into(),
            mol_id,
        }
    }

    /// Display the reference in canonical CLI form (`alias:mol_id`).
    #[must_use]
    pub fn to_canonical_string(&self) -> String {
        format!("{}:{}", self.galaxy, self.mol_id)
    }

    /// Detect whether a free-form string contains a cross-galaxy
    /// separator (`:` or `@`). Used by `--blocked-by` to dispatch
    /// between the `MoleculeId` parser and the `CrossGalaxyRef`
    /// parser without a regex.
    #[must_use]
    pub fn looks_like_cross_galaxy(s: &str) -> bool {
        s.contains(CROSS_GALAXY_SEPARATORS)
    }
}

impl fmt::Display for CrossGalaxyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.galaxy, self.mol_id)
    }
}

/// Errors surfaced when parsing a cross-galaxy reference string.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CrossGalaxyRefError {
    /// No `:` or `@` separator was found between alias and molecule id.
    #[error("missing alias separator (`:` or `@`) in `{0}`")]
    MissingSeparator(String),
    /// The alias side of the reference was empty (e.g. `:mol-id`).
    #[error("galaxy alias is empty in `{0}`")]
    EmptyAlias(String),
    /// The molecule id side of the reference was empty (e.g. `alias:`).
    #[error("molecule id is empty in `{0}`")]
    EmptyMoleculeId(String),
    /// The alias contains characters that are not allowed for a galaxy
    /// alias. We intentionally keep the alphabet small (alphanumerics,
    /// `-`, `_`) so a misspelled `mailroom ` (trailing space) or
    /// shell-glob accident is caught at parse time.
    #[error("invalid galaxy alias `{0}` — must be alphanumeric, `-`, or `_`")]
    InvalidAlias(String),
    /// The molecule id portion failed [`MoleculeId`] validation.
    #[error("invalid molecule id `{id}`: {reason}")]
    InvalidMoleculeId {
        /// The offending molecule-id substring.
        id: String,
        /// The error returned by [`MoleculeId::new`].
        reason: String,
    },
}

impl FromStr for CrossGalaxyRef {
    type Err = CrossGalaxyRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let pos = s
            .find(CROSS_GALAXY_SEPARATORS)
            .ok_or_else(|| CrossGalaxyRefError::MissingSeparator(s.to_owned()))?;
        let alias = &s[..pos];
        let mol = &s[pos + 1..];

        if alias.is_empty() {
            return Err(CrossGalaxyRefError::EmptyAlias(s.to_owned()));
        }
        if mol.is_empty() {
            return Err(CrossGalaxyRefError::EmptyMoleculeId(s.to_owned()));
        }
        if !alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(CrossGalaxyRefError::InvalidAlias(alias.to_owned()));
        }
        let mol_id = MoleculeId::new(mol).map_err(|e| CrossGalaxyRefError::InvalidMoleculeId {
            id: mol.to_owned(),
            reason: e.to_string(),
        })?;
        Ok(Self {
            galaxy: alias.to_owned(),
            mol_id,
        })
    }
}

/// A recorded interaction between molecules.
///
/// Stored in the interaction log (`.cosmon/interactions.jsonl`) and
/// referenced via [`MoleculeLink`] on each participating molecule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Interaction {
    /// 1 molecule → N molecules. Source completes; products are nucleated.
    ///
    /// Example: an idea decays into 3 tasks.
    Decay {
        /// The molecule that decayed.
        source: MoleculeId,
        /// The new molecules produced.
        products: Vec<MoleculeId>,
        /// Why the decay happened.
        reason: String,
        /// When the interaction occurred.
        timestamp: DateTime<Utc>,
    },

    /// N molecules → 1 molecule. Sources complete; product is nucleated.
    ///
    /// Example: 3 research tasks merge into 1 decision.
    Merge {
        /// The molecules that were consumed.
        sources: Vec<MoleculeId>,
        /// The new molecule produced.
        product: MoleculeId,
        /// Why the merge happened.
        reason: String,
        /// When the interaction occurred.
        timestamp: DateTime<Utc>,
    },

    /// 1 molecule changes kind without changing identity.
    ///
    /// Example: an idea is promoted to a task.
    Transform {
        /// The molecule that changed kind.
        molecule: MoleculeId,
        /// The original kind.
        from: MoleculeKind,
        /// The new kind.
        to: MoleculeKind,
        /// Why the transform happened.
        reason: String,
        /// When the interaction occurred.
        timestamp: DateTime<Utc>,
    },
}

/// A typed link between molecules, replacing untyped `Vec<String>`.
///
/// Links are bidirectional — each participant in an interaction carries
/// a link to the other participants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "rel")]
#[non_exhaustive]
pub enum MoleculeLink {
    /// This molecule decayed from the referenced source.
    DecayedFrom {
        /// The source molecule that decayed.
        id: MoleculeId,
    },
    /// This molecule is a decay product of the referenced source.
    DecayProduct {
        /// The product molecule.
        id: MoleculeId,
    },
    /// This molecule was produced by merging the referenced sources.
    MergedFrom {
        /// The source molecules that were merged.
        ids: Vec<MoleculeId>,
    },
    /// This molecule contributed to the referenced merge product.
    MergedInto {
        /// The product molecule.
        id: MoleculeId,
    },
    /// This molecule was transformed from a different kind.
    TransformedFrom {
        /// The previous kind.
        kind: MoleculeKind,
    },
    /// This molecule blocks the referenced target — the target cannot
    /// progress until this one completes. Symmetric counterpart of
    /// [`BlockedBy`]: if A has `Blocks { target: B }`, then B must have
    /// `BlockedBy { source: A }`. Symmetry is maintained at the CLI and
    /// MCP layers when the link is created; consumers can trust either
    /// side of the pair.
    ///
    /// This is the first-class DAG edge that the resident runtime's
    /// `DagPolicy` consumes to compute execution order (see ADR-016).
    ///
    /// [`BlockedBy`]: Self::BlockedBy
    Blocks {
        /// The target molecule that this one blocks.
        target: MoleculeId,
    },
    /// This molecule is blocked by the referenced source — cannot progress
    /// until the source completes. Symmetric counterpart of [`Blocks`].
    ///
    /// [`Blocks`]: Self::Blocks
    BlockedBy {
        /// The source molecule that blocks this one.
        source: MoleculeId,
    },
    /// This molecule refines (cites, points at, elaborates) the referenced
    /// target — a semantic citation edge that does **not** carry progression
    /// semantics (no blocking, no decay chain).
    ///
    /// Intended primarily for [`Constellation`](crate::kind::MoleculeKind::Constellation)
    /// molecules that name a pattern across N existing molecules: each cited
    /// molecule becomes a `Refines` target on the constellation, and the
    /// target gains a symmetric [`RefinedBy`] edge. `Refines` is the minimal
    /// typed pointer — no cycle guarantees, no monotonic-time guarantees —
    /// so that different use cases (constellation, ADR "refines ADR-N",
    /// issue "supersedes task T") can reuse it without expanding its
    /// semantics.
    ///
    /// [`RefinedBy`]: Self::RefinedBy
    Refines {
        /// The molecule being refined / cited / pointed at.
        target: MoleculeId,
    },
    /// Symmetric counterpart of [`Refines`] — carried on the target side.
    ///
    /// [`Refines`]: Self::Refines
    RefinedBy {
        /// The molecule that refines / cites / points at this one.
        source: MoleculeId,
    },
    /// This molecule **refutes** the diagnosis carried by the referenced
    /// target — a semantic citation edge recording that a verification
    /// contradicted a relayed causal claim.
    ///
    /// This is the DAG-native form of the [ADR-143](../../docs/adr/143-cmb-diagnosis-verify-gate.md)
    /// diagnosis-verify gate's step 4 ("when verification contradicts the
    /// CMB, follow the code, not the note — and record the divergence").
    /// A `cmb-verify` molecule that reproduces the symptom but finds the
    /// stated *mechanism* describes a code path that does not exist points
    /// at the diagnosis molecule with a `Refutes` edge; the target gains a
    /// symmetric [`RefutedBy`] edge so a later reader (or the CMB sender,
    /// closing the calibration loop) can query which relayed diagnoses held.
    ///
    /// Like [`Refines`], it carries **no progression semantics** — it does
    /// not block, does not decay, and cycles are permitted (an epistemic
    /// annotation, not a scheduling constraint). It is deliberately *not* a
    /// `Blocks` edge: a refutation records a fact about a claim, it does not
    /// gate any molecule's execution.
    ///
    /// [`Refines`]: Self::Refines
    /// [`RefutedBy`]: Self::RefutedBy
    Refutes {
        /// The molecule whose diagnosis is being refuted.
        target: MoleculeId,
    },
    /// Symmetric counterpart of [`Refutes`] — carried on the target
    /// (refuted) side.
    ///
    /// [`Refutes`]: Self::Refutes
    RefutedBy {
        /// The molecule that refuted this one's diagnosis.
        source: MoleculeId,
    },
    /// Free-form entanglement (backward compat with `links: Vec<String>`).
    Entangled {
        /// The linked entity (molecule ID, URL, or free text).
        target: String,
    },
    /// Cross-galaxy blocking dependency — this molecule blocks a
    /// molecule that lives in another cosmon galaxy. Phase 1 of
    /// [ADR-035](../../docs/adr/035-cross-galaxy-edges.md): the target
    /// is identified by a human-readable galaxy alias, not yet by a
    /// content-addressed `GalaxyHash`.
    ///
    /// **Symmetry is best-effort across galaxies.** Per the
    /// one-writer-per-galaxy discipline (ADR-052), the source galaxy
    /// records the edge locally; the target galaxy is **not** mutated
    /// (we cannot acquire its fleet lock from the outside). The
    /// reciprocal `CrossGalaxyBlockedBy` is filed only when the target
    /// galaxy is reachable on the same filesystem; otherwise the edge
    /// stays asymmetric and the runtime treats the cross-galaxy side
    /// as informational.
    CrossGalaxyBlocks {
        /// The qualified target reference.
        target: CrossGalaxyRef,
    },
    /// Cross-galaxy upstream blocker — this molecule cannot progress
    /// until a molecule in another galaxy completes. Symmetric
    /// counterpart of [`CrossGalaxyBlocks`].
    ///
    /// [`CrossGalaxyBlocks`]: Self::CrossGalaxyBlocks
    CrossGalaxyBlockedBy {
        /// The qualified source reference.
        source: CrossGalaxyRef,
    },
}

impl MoleculeLink {
    /// Returns the target molecule ID of a `Blocks` link, or `None` for
    /// other variants. Used by helpers that walk the `blocks` edge.
    #[must_use]
    pub fn blocks_target(&self) -> Option<&MoleculeId> {
        match self {
            Self::Blocks { target } => Some(target),
            _ => None,
        }
    }

    /// Returns the source molecule ID of a `BlockedBy` link, or `None`
    /// for other variants. Used by helpers that walk the `blocked_by` edge.
    #[must_use]
    pub fn blocked_by_source(&self) -> Option<&MoleculeId> {
        match self {
            Self::BlockedBy { source } => Some(source),
            _ => None,
        }
    }

    /// Returns the product molecule ID of a `DecayProduct` link, or `None`
    /// for other variants. Used by helpers that walk decay children.
    #[must_use]
    pub fn decay_product_id(&self) -> Option<&MoleculeId> {
        match self {
            Self::DecayProduct { id } => Some(id),
            _ => None,
        }
    }

    /// Returns the target molecule ID of a `Refines` link, or `None` for
    /// other variants. Used by helpers (e.g. `cs deps`, constellation
    /// rendering) that walk the citation graph.
    #[must_use]
    pub fn refines_target(&self) -> Option<&MoleculeId> {
        match self {
            Self::Refines { target } => Some(target),
            _ => None,
        }
    }

    /// Returns the source molecule ID of a `RefinedBy` link, or `None` for
    /// other variants.
    #[must_use]
    pub fn refined_by_source(&self) -> Option<&MoleculeId> {
        match self {
            Self::RefinedBy { source } => Some(source),
            _ => None,
        }
    }

    /// Returns the target molecule ID of a `Refutes` link, or `None` for
    /// other variants. Used by helpers that walk the refutation graph
    /// (`cs deps`, the diagnosis-verify calibration surface).
    #[must_use]
    pub fn refutes_target(&self) -> Option<&MoleculeId> {
        match self {
            Self::Refutes { target } => Some(target),
            _ => None,
        }
    }

    /// Returns the source molecule ID of a `RefutedBy` link, or `None` for
    /// other variants.
    #[must_use]
    pub fn refuted_by_source(&self) -> Option<&MoleculeId> {
        match self {
            Self::RefutedBy { source } => Some(source),
            _ => None,
        }
    }

    /// Returns the qualified target of a `CrossGalaxyBlocks` link, or
    /// `None` for other variants. Used by `cs deps` to surface
    /// cross-galaxy edges in the downstream column without mistakenly
    /// treating them as local molecule IDs.
    #[must_use]
    pub fn cross_galaxy_blocks_target(&self) -> Option<&CrossGalaxyRef> {
        match self {
            Self::CrossGalaxyBlocks { target } => Some(target),
            _ => None,
        }
    }

    /// Returns the qualified source of a `CrossGalaxyBlockedBy` link,
    /// or `None` for other variants.
    #[must_use]
    pub fn cross_galaxy_blocked_by_source(&self) -> Option<&CrossGalaxyRef> {
        match self {
            Self::CrossGalaxyBlockedBy { source } => Some(source),
            _ => None,
        }
    }

    /// Project this link onto its canonical (forward) edges, expressed as
    /// `(RelationKind, source_mol, target_mol)` triples in the
    /// **dependency-before-dependent** orientation.
    ///
    /// Symmetric link pairs (`Blocks`/`BlockedBy`, `Refines`/`RefinedBy`,
    /// `Refutes`/`RefutedBy`, `DecayProduct`/`DecayedFrom`,
    /// `MergedFrom`/`MergedInto`) collapse to the same canonical edge
    /// regardless of which side carries the link, so a graph induced by
    /// all molecules' `typed_links` cannot double-count an edge.
    ///
    /// `self_id` is the molecule that owns this link record; it appears
    /// as either source or target depending on the variant. Variants that
    /// do not point at a local molecule (`TransformedFrom`, `Entangled`,
    /// `CrossGalaxyBlocks`, `CrossGalaxyBlockedBy`) yield an empty vec —
    /// the local DAG check ignores them.
    #[must_use]
    pub fn canonical_edges(
        &self,
        self_id: &MoleculeId,
    ) -> Vec<(RelationKind, MoleculeId, MoleculeId)> {
        match self {
            Self::Blocks { target } => {
                vec![(RelationKind::Blocks, self_id.clone(), target.clone())]
            }
            Self::BlockedBy { source } => {
                vec![(RelationKind::Blocks, source.clone(), self_id.clone())]
            }
            Self::DecayProduct { id } => {
                vec![(RelationKind::DecayProduct, self_id.clone(), id.clone())]
            }
            Self::DecayedFrom { id } => {
                vec![(RelationKind::DecayProduct, id.clone(), self_id.clone())]
            }
            Self::MergedFrom { ids } => ids
                .iter()
                .map(|src| (RelationKind::MergedFrom, src.clone(), self_id.clone()))
                .collect(),
            Self::MergedInto { id } => {
                vec![(RelationKind::MergedFrom, self_id.clone(), id.clone())]
            }
            Self::Refines { target } => {
                vec![(RelationKind::Refines, self_id.clone(), target.clone())]
            }
            Self::RefinedBy { source } => {
                vec![(RelationKind::Refines, source.clone(), self_id.clone())]
            }
            Self::Refutes { target } => {
                vec![(RelationKind::Refutes, self_id.clone(), target.clone())]
            }
            Self::RefutedBy { source } => {
                vec![(RelationKind::Refutes, source.clone(), self_id.clone())]
            }
            // Local-DAG analysis ignores transforms, free-form entanglement,
            // and cross-galaxy edges (the latter target a `CrossGalaxyRef`,
            // not a `MoleculeId`).
            Self::TransformedFrom { .. }
            | Self::Entangled { .. }
            | Self::CrossGalaxyBlocks { .. }
            | Self::CrossGalaxyBlockedBy { .. } => Vec::new(),
        }
    }
}

/// The kind of typed relation between molecules — the discriminator the
/// `cs verify-graph --relation R` primitive checks for cycle freeness.
///
/// One [`RelationKind`] groups every [`MoleculeLink`] variant that
/// expresses the **same** semantic edge from different vantage points
/// (e.g. `Blocks` and `BlockedBy` both denote `RelationKind::Blocks`).
/// This collapse is what lets cycle detection reason about a single
/// canonical orientation per relation.
///
/// [`is_dag_required`](Self::is_dag_required) declares which relations
/// MUST be acyclic for the cosmon runtime to remain sound. Relations
/// flagged `false` may legitimately carry cycles (e.g. a
/// `Constellation` molecule cites another constellation that cites back —
/// no progression semantics, no harm done).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RelationKind {
    /// `Blocks` / `BlockedBy` — progression edges that the DAG runtime
    /// consumes to compute execution order. Cycles are stuck molecules.
    Blocks,
    /// `DecayProduct` / `DecayedFrom` — decay chain. A molecule cannot
    /// be its own ancestor; cycles indicate replay corruption.
    DecayProduct,
    /// `MergedFrom` / `MergedInto` — merge chain. Same DAG invariant
    /// as decay: merge is irreversible and cycle-free.
    MergedFrom,
    /// `Refines` / `RefinedBy` — citation edges (constellation, ADR
    /// "refines ADR-N", etc.). **Cycles are permitted by design** —
    /// no progression semantics, two constellations may cite each
    /// other.
    Refines,
    /// `Refutes` / `RefutedBy` — refutation edges (a verify molecule
    /// contradicting a relayed diagnosis, ADR-143). **Cycles are
    /// permitted by design** — an epistemic annotation, not a
    /// progression edge; mutual refutation is a legitimate debate shape.
    Refutes,
}

impl RelationKind {
    /// Whether this relation MUST be a DAG for the runtime to be sound.
    ///
    /// `cs verify-graph` exits with status 1 when a `is_dag_required()`
    /// relation contains a non-trivial SCC. Relations that return
    /// `false` are still inspected (their SCCs are reported) but do
    /// not cause a non-zero exit.
    #[must_use]
    pub const fn is_dag_required(self) -> bool {
        match self {
            Self::Blocks | Self::DecayProduct | Self::MergedFrom => true,
            Self::Refines | Self::Refutes => false,
        }
    }

    /// Stable kebab-case identifier — what the operator types on the CLI
    /// (`--relation blocks`, `--relation decay-product`, …) and what
    /// `--json` output uses as a key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::DecayProduct => "decay-product",
            Self::MergedFrom => "merged-from",
            Self::Refines => "refines",
            Self::Refutes => "refutes",
        }
    }

    /// Every registered relation, in deterministic order. Used by
    /// `cs verify-graph --all` to iterate over the full surface.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Blocks,
            Self::DecayProduct,
            Self::MergedFrom,
            Self::Refines,
            Self::Refutes,
        ]
    }
}

impl fmt::Display for RelationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors returned when parsing a [`RelationKind`] from its kebab-case
/// CLI form.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown relation `{0}` (known: blocks, decay-product, merged-from, refines, refutes)")]
pub struct UnknownRelationKind(pub String);

impl FromStr for RelationKind {
    type Err = UnknownRelationKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "blocks" => Ok(Self::Blocks),
            "decay-product" | "decay_product" => Ok(Self::DecayProduct),
            "merged-from" | "merged_from" => Ok(Self::MergedFrom),
            "refines" => Ok(Self::Refines),
            "refutes" => Ok(Self::Refutes),
            other => Err(UnknownRelationKind(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interaction_serde_roundtrip() {
        let interaction = Interaction::Decay {
            source: MoleculeId::new("idea-20260407-abcd").unwrap(),
            products: vec![
                MoleculeId::new("task-20260407-ef01").unwrap(),
                MoleculeId::new("task-20260407-ef02").unwrap(),
            ],
            reason: "Idea decomposed into implementation tasks".to_string(),
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&interaction).unwrap();
        let parsed: Interaction = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, Interaction::Decay { .. }));
    }

    #[test]
    fn test_link_serde_roundtrip() {
        let link = MoleculeLink::DecayedFrom {
            id: MoleculeId::new("idea-20260407-abcd").unwrap(),
        };
        let json = serde_json::to_string(&link).unwrap();
        let parsed: MoleculeLink = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, MoleculeLink::DecayedFrom { .. }));
    }

    #[test]
    fn test_blocks_link_serde_roundtrip() {
        let link = MoleculeLink::Blocks {
            target: MoleculeId::new("task-20260409-0001").unwrap(),
        };
        let json = serde_json::to_string(&link).unwrap();
        let parsed: MoleculeLink = serde_json::from_str(&json).unwrap();
        match parsed {
            MoleculeLink::Blocks { target } => {
                assert_eq!(target.as_str(), "task-20260409-0001");
            }
            _ => panic!("expected Blocks variant, got {parsed:?}"),
        }
    }

    #[test]
    fn test_blocked_by_link_serde_roundtrip() {
        let link = MoleculeLink::BlockedBy {
            source: MoleculeId::new("task-20260409-0002").unwrap(),
        };
        let json = serde_json::to_string(&link).unwrap();
        let parsed: MoleculeLink = serde_json::from_str(&json).unwrap();
        match parsed {
            MoleculeLink::BlockedBy { source } => {
                assert_eq!(source.as_str(), "task-20260409-0002");
            }
            _ => panic!("expected BlockedBy variant, got {parsed:?}"),
        }
    }

    #[test]
    fn test_blocks_target_accessor() {
        let target = MoleculeId::new("task-20260409-aaaa").unwrap();
        let link = MoleculeLink::Blocks {
            target: target.clone(),
        };
        assert_eq!(link.blocks_target(), Some(&target));
        assert_eq!(link.blocked_by_source(), None);

        let other = MoleculeLink::Entangled {
            target: "foo".to_owned(),
        };
        assert_eq!(other.blocks_target(), None);
    }

    #[test]
    fn test_blocked_by_source_accessor() {
        let source = MoleculeId::new("task-20260409-bbbb").unwrap();
        let link = MoleculeLink::BlockedBy {
            source: source.clone(),
        };
        assert_eq!(link.blocked_by_source(), Some(&source));
        assert_eq!(link.blocks_target(), None);
    }

    #[test]
    fn test_blocks_serde_tag_format() {
        // Serde representation uses the snake_case rel tag — verify this
        // stays stable since MCP consumers and surface rendering key off it.
        let link = MoleculeLink::Blocks {
            target: MoleculeId::new("task-20260409-tagv").unwrap(),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "blocks");
        assert_eq!(json["target"], "task-20260409-tagv");
    }

    #[test]
    fn test_blocked_by_serde_tag_format() {
        let link = MoleculeLink::BlockedBy {
            source: MoleculeId::new("task-20260409-tagz").unwrap(),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "blocked_by");
        assert_eq!(json["source"], "task-20260409-tagz");
    }

    #[test]
    fn test_decay_product_id_accessor() {
        let child = MoleculeId::new("task-20260411-dcay").unwrap();
        let link = MoleculeLink::DecayProduct { id: child.clone() };
        assert_eq!(link.decay_product_id(), Some(&child));
        assert_eq!(link.blocks_target(), None);
        assert_eq!(link.blocked_by_source(), None);

        let other = MoleculeLink::Entangled {
            target: "bar".to_owned(),
        };
        assert_eq!(other.decay_product_id(), None);
    }

    #[test]
    fn test_refines_link_serde_roundtrip() {
        let link = MoleculeLink::Refines {
            target: MoleculeId::new("task-20260422-aaaa").unwrap(),
        };
        let json = serde_json::to_string(&link).unwrap();
        let parsed: MoleculeLink = serde_json::from_str(&json).unwrap();
        match parsed {
            MoleculeLink::Refines { target } => {
                assert_eq!(target.as_str(), "task-20260422-aaaa");
            }
            _ => panic!("expected Refines variant, got {parsed:?}"),
        }
    }

    #[test]
    fn test_refines_serde_tag_format() {
        let link = MoleculeLink::Refines {
            target: MoleculeId::new("task-20260422-bbbb").unwrap(),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "refines");
        assert_eq!(json["target"], "task-20260422-bbbb");
    }

    #[test]
    fn test_refined_by_serde_tag_format() {
        let link = MoleculeLink::RefinedBy {
            source: MoleculeId::new("task-20260422-cccc").unwrap(),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "refined_by");
        assert_eq!(json["source"], "task-20260422-cccc");
    }

    #[test]
    fn test_cross_galaxy_ref_parses_colon_form() {
        let cgr: CrossGalaxyRef = "mailroom:delib-20260425-39c1".parse().unwrap();
        assert_eq!(cgr.galaxy, "mailroom");
        assert_eq!(cgr.mol_id.as_str(), "delib-20260425-39c1");
        assert_eq!(cgr.to_string(), "mailroom:delib-20260425-39c1");
    }

    #[test]
    fn test_cross_galaxy_ref_parses_at_form() {
        let cgr: CrossGalaxyRef = "tenant-demo@delib-20260425-54aa".parse().unwrap();
        assert_eq!(cgr.galaxy, "tenant-demo");
        assert_eq!(cgr.mol_id.as_str(), "delib-20260425-54aa");
        // Canonical form always uses the colon — `@` is a parse-only
        // alias.
        assert_eq!(cgr.to_canonical_string(), "tenant-demo:delib-20260425-54aa");
    }

    #[test]
    fn test_cross_galaxy_ref_rejects_local_id() {
        // No separator at all → must be parsed as a local id, not as
        // a cross-galaxy ref.
        let err: CrossGalaxyRefError = "task-20260425-aaaa".parse::<CrossGalaxyRef>().unwrap_err();
        assert!(matches!(err, CrossGalaxyRefError::MissingSeparator(_)));
    }

    #[test]
    fn test_cross_galaxy_ref_rejects_empty_alias() {
        let err: CrossGalaxyRefError = ":task-20260425-aaaa".parse::<CrossGalaxyRef>().unwrap_err();
        assert!(matches!(err, CrossGalaxyRefError::EmptyAlias(_)));
    }

    #[test]
    fn test_cross_galaxy_ref_rejects_empty_mol() {
        let err: CrossGalaxyRefError = "mailroom:".parse::<CrossGalaxyRef>().unwrap_err();
        assert!(matches!(err, CrossGalaxyRefError::EmptyMoleculeId(_)));
    }

    #[test]
    fn test_cross_galaxy_ref_rejects_invalid_alias_chars() {
        let err: CrossGalaxyRefError = "sec retariat:task-20260425-aaaa"
            .parse::<CrossGalaxyRef>()
            .unwrap_err();
        assert!(matches!(err, CrossGalaxyRefError::InvalidAlias(_)));
    }

    #[test]
    fn test_cross_galaxy_ref_rejects_invalid_mol_id() {
        let err: CrossGalaxyRefError = "mailroom:not-a-real-id"
            .parse::<CrossGalaxyRef>()
            .unwrap_err();
        assert!(matches!(err, CrossGalaxyRefError::InvalidMoleculeId { .. }));
    }

    #[test]
    fn test_cross_galaxy_blocks_serde_roundtrip() {
        let link = MoleculeLink::CrossGalaxyBlocks {
            target: CrossGalaxyRef::new(
                "mailroom",
                MoleculeId::new("delib-20260425-39c1").unwrap(),
            ),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "cross_galaxy_blocks");
        assert_eq!(json["target"]["galaxy"], "mailroom");
        assert_eq!(json["target"]["mol_id"], "delib-20260425-39c1");
        let parsed: MoleculeLink = serde_json::from_value(json).unwrap();
        match parsed {
            MoleculeLink::CrossGalaxyBlocks { target } => {
                assert_eq!(target.galaxy, "mailroom");
                assert_eq!(target.mol_id.as_str(), "delib-20260425-39c1");
            }
            other => panic!("expected CrossGalaxyBlocks, got {other:?}"),
        }
    }

    #[test]
    fn test_cross_galaxy_blocked_by_serde_roundtrip() {
        let link = MoleculeLink::CrossGalaxyBlockedBy {
            source: CrossGalaxyRef::new(
                "tenant-demo",
                MoleculeId::new("delib-20260425-54aa").unwrap(),
            ),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "cross_galaxy_blocked_by");
        let parsed: MoleculeLink = serde_json::from_value(json).unwrap();
        assert!(matches!(parsed, MoleculeLink::CrossGalaxyBlockedBy { .. }));
    }

    #[test]
    fn test_cross_galaxy_accessors() {
        let target =
            CrossGalaxyRef::new("mailroom", MoleculeId::new("delib-20260425-39c1").unwrap());
        let link = MoleculeLink::CrossGalaxyBlocks {
            target: target.clone(),
        };
        assert_eq!(link.cross_galaxy_blocks_target(), Some(&target));
        assert_eq!(link.cross_galaxy_blocked_by_source(), None);
        assert_eq!(link.blocks_target(), None);

        let upstream = MoleculeLink::CrossGalaxyBlockedBy {
            source: target.clone(),
        };
        assert_eq!(upstream.cross_galaxy_blocked_by_source(), Some(&target));
        assert_eq!(upstream.cross_galaxy_blocks_target(), None);
    }

    #[test]
    fn test_looks_like_cross_galaxy_dispatches_correctly() {
        assert!(CrossGalaxyRef::looks_like_cross_galaxy(
            "mailroom:delib-20260425-39c1"
        ));
        assert!(CrossGalaxyRef::looks_like_cross_galaxy(
            "mailroom@delib-20260425-39c1"
        ));
        assert!(!CrossGalaxyRef::looks_like_cross_galaxy(
            "delib-20260425-39c1"
        ));
        assert!(!CrossGalaxyRef::looks_like_cross_galaxy(
            "task-20260409-aaaa"
        ));
    }

    #[test]
    fn test_refines_and_refined_by_accessors() {
        let target = MoleculeId::new("task-20260422-dddd").unwrap();
        let fwd = MoleculeLink::Refines {
            target: target.clone(),
        };
        assert_eq!(fwd.refines_target(), Some(&target));
        assert_eq!(fwd.refined_by_source(), None);

        let back = MoleculeLink::RefinedBy {
            source: target.clone(),
        };
        assert_eq!(back.refined_by_source(), Some(&target));
        assert_eq!(back.refines_target(), None);
    }

    // ─── Refutes / RefutedBy (ADR-143 diagnosis-verify edge) ────────────

    #[test]
    fn test_refutes_link_serde_roundtrip() {
        let link = MoleculeLink::Refutes {
            target: MoleculeId::new("task-20260705-e41d").unwrap(),
        };
        let json = serde_json::to_string(&link).unwrap();
        let parsed: MoleculeLink = serde_json::from_str(&json).unwrap();
        match parsed {
            MoleculeLink::Refutes { target } => {
                assert_eq!(target.as_str(), "task-20260705-e41d");
            }
            _ => panic!("expected Refutes variant, got {parsed:?}"),
        }
    }

    #[test]
    fn test_refutes_serde_tag_format() {
        // MCP consumers and surface rendering key off the snake_case rel
        // tag; pin it so a rename is a loud test failure.
        let link = MoleculeLink::Refutes {
            target: MoleculeId::new("task-20260705-aaaa").unwrap(),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "refutes");
        assert_eq!(json["target"], "task-20260705-aaaa");
    }

    #[test]
    fn test_refuted_by_serde_tag_format() {
        let link = MoleculeLink::RefutedBy {
            source: MoleculeId::new("task-20260705-bbbb").unwrap(),
        };
        let json = serde_json::to_value(&link).unwrap();
        assert_eq!(json["rel"], "refuted_by");
        assert_eq!(json["source"], "task-20260705-bbbb");
    }

    #[test]
    fn test_refutes_and_refuted_by_accessors() {
        let target = MoleculeId::new("task-20260705-cccc").unwrap();
        let fwd = MoleculeLink::Refutes {
            target: target.clone(),
        };
        assert_eq!(fwd.refutes_target(), Some(&target));
        assert_eq!(fwd.refuted_by_source(), None);
        // Refutes is not a Refines edge — the accessors must not cross-talk.
        assert_eq!(fwd.refines_target(), None);

        let back = MoleculeLink::RefutedBy {
            source: target.clone(),
        };
        assert_eq!(back.refuted_by_source(), Some(&target));
        assert_eq!(back.refutes_target(), None);
    }

    #[test]
    fn test_canonical_edges_refutes_pair_collapse() {
        // The verify molecule's `Refutes` and the diagnosis molecule's
        // `RefutedBy` collapse to the SAME canonical edge, so the
        // refutation graph is not double-counted.
        let verify = MoleculeId::new("task-20260705-e41d").unwrap();
        let diagnosis = MoleculeId::new("delib-20260705-036b").unwrap();
        let fwd = MoleculeLink::Refutes {
            target: diagnosis.clone(),
        };
        let back = MoleculeLink::RefutedBy {
            source: verify.clone(),
        };
        let edge = (RelationKind::Refutes, verify.clone(), diagnosis.clone());
        assert_eq!(fwd.canonical_edges(&verify), vec![edge.clone()]);
        assert_eq!(back.canonical_edges(&diagnosis), vec![edge]);
    }

    #[test]
    fn test_relation_kind_refutes_is_not_dag_required() {
        // A refutation is an epistemic annotation, not a scheduling
        // constraint: mutual refutation must not be a `verify-graph` error.
        assert!(!RelationKind::Refutes.is_dag_required());
        assert!(RelationKind::all().contains(&RelationKind::Refutes));
        assert_eq!(RelationKind::Refutes.as_str(), "refutes");
        assert_eq!(
            "refutes".parse::<RelationKind>().unwrap(),
            RelationKind::Refutes
        );
    }

    // ─── RelationKind + canonical_edges ─────────────────────────────────

    #[test]
    fn test_relation_kind_dag_required_partition() {
        // Hawking principle: the runtime must be able to declare which
        // relations carry progression semantics (no cycles allowed) and
        // which are purely citational (cycles legitimate).
        assert!(RelationKind::Blocks.is_dag_required());
        assert!(RelationKind::DecayProduct.is_dag_required());
        assert!(RelationKind::MergedFrom.is_dag_required());
        assert!(!RelationKind::Refines.is_dag_required());
    }

    #[test]
    fn test_relation_kind_str_roundtrip() {
        for kind in RelationKind::all() {
            let s = kind.as_str();
            let parsed: RelationKind = s.parse().expect("kebab-case roundtrip");
            assert_eq!(*kind, parsed);
            assert_eq!(kind.to_string(), s);
        }
    }

    #[test]
    fn test_relation_kind_accepts_underscore_aliases() {
        // Operators occasionally type the snake_case form; accept it as
        // an ergonomic alias rather than failing the verb.
        assert_eq!(
            "decay_product".parse::<RelationKind>().unwrap(),
            RelationKind::DecayProduct
        );
        assert_eq!(
            "merged_from".parse::<RelationKind>().unwrap(),
            RelationKind::MergedFrom
        );
    }

    #[test]
    fn test_relation_kind_unknown_yields_error() {
        let err = "oversee".parse::<RelationKind>().unwrap_err();
        assert!(err.to_string().contains("oversee"));
    }

    #[test]
    fn test_canonical_edges_blocks_pair_collapse() {
        // A `Blocks` and the matching `BlockedBy` must produce the
        // SAME canonical edge so the cycle check does not double-count.
        let a = MoleculeId::new("task-20260509-aaaa").unwrap();
        let b = MoleculeId::new("task-20260509-bbbb").unwrap();
        let fwd = MoleculeLink::Blocks { target: b.clone() };
        let back = MoleculeLink::BlockedBy { source: a.clone() };
        assert_eq!(
            fwd.canonical_edges(&a),
            vec![(RelationKind::Blocks, a.clone(), b.clone())]
        );
        assert_eq!(back.canonical_edges(&b), vec![(RelationKind::Blocks, a, b)]);
    }

    #[test]
    fn test_canonical_edges_decay_pair_collapse() {
        let parent = MoleculeId::new("idea-20260509-aaaa").unwrap();
        let child = MoleculeId::new("task-20260509-bbbb").unwrap();
        let on_parent = MoleculeLink::DecayProduct { id: child.clone() };
        let on_child = MoleculeLink::DecayedFrom { id: parent.clone() };
        let edge = (RelationKind::DecayProduct, parent.clone(), child.clone());
        assert_eq!(on_parent.canonical_edges(&parent), vec![edge.clone()]);
        assert_eq!(on_child.canonical_edges(&child), vec![edge]);
    }

    #[test]
    fn test_canonical_edges_merged_from_fans_out() {
        // `MergedFrom { ids }` carries N sources that all converge on
        // the merged product — N edges, all pointing into self_id.
        let merged = MoleculeId::new("task-20260509-mrgd").unwrap();
        let s1 = MoleculeId::new("task-20260509-src1").unwrap();
        let s2 = MoleculeId::new("task-20260509-src2").unwrap();
        let link = MoleculeLink::MergedFrom {
            ids: vec![s1.clone(), s2.clone()],
        };
        let edges = link.canonical_edges(&merged);
        assert_eq!(edges.len(), 2);
        assert!(edges.contains(&(RelationKind::MergedFrom, s1, merged.clone())));
        assert!(edges.contains(&(RelationKind::MergedFrom, s2, merged)));
    }

    #[test]
    fn test_canonical_edges_refines_pair_collapse() {
        let a = MoleculeId::new("task-20260509-cccc").unwrap();
        let b = MoleculeId::new("task-20260509-dddd").unwrap();
        let fwd = MoleculeLink::Refines { target: b.clone() };
        let back = MoleculeLink::RefinedBy { source: a.clone() };
        let edge = (RelationKind::Refines, a.clone(), b.clone());
        assert_eq!(fwd.canonical_edges(&a), vec![edge.clone()]);
        assert_eq!(back.canonical_edges(&b), vec![edge]);
    }

    proptest::proptest! {
        // Property: a `Blocks` link on the source and a `BlockedBy`
        // link on the target collapse to the SAME canonical edge for
        // any pair of distinct molecules. This is what makes
        // `verify-graph` immune to double-counting when both sides of
        // the symmetry are recorded (the common case).
        #[test]
        fn prop_blocks_pair_collapses_to_one_edge(
            i in 0u32..1024,
            j in 0u32..1024,
        ) {
            proptest::prop_assume!(i != j);
            let a = MoleculeId::new(format!("task-20260509-{i:04x}")).unwrap();
            let b = MoleculeId::new(format!("task-20260509-{j:04x}")).unwrap();
            let fwd = MoleculeLink::Blocks { target: b.clone() };
            let back = MoleculeLink::BlockedBy { source: a.clone() };
            proptest::prop_assert_eq!(fwd.canonical_edges(&a), back.canonical_edges(&b));
        }

        // Property: `canonical_edges` always returns edges of the
        // declared `RelationKind` — no cross-talk between relations.
        #[test]
        fn prop_canonical_edge_kind_matches_link(
            i in 0u32..1024,
            j in 0u32..1024,
        ) {
            proptest::prop_assume!(i != j);
            let a = MoleculeId::new(format!("task-20260509-{i:04x}")).unwrap();
            let b = MoleculeId::new(format!("task-20260509-{j:04x}")).unwrap();
            for (link, expected) in [
                (MoleculeLink::Blocks { target: b.clone() }, RelationKind::Blocks),
                (MoleculeLink::BlockedBy { source: a.clone() }, RelationKind::Blocks),
                (MoleculeLink::DecayProduct { id: b.clone() }, RelationKind::DecayProduct),
                (MoleculeLink::DecayedFrom { id: a.clone() }, RelationKind::DecayProduct),
                (MoleculeLink::Refines { target: b.clone() }, RelationKind::Refines),
                (MoleculeLink::RefinedBy { source: a.clone() }, RelationKind::Refines),
                (MoleculeLink::Refutes { target: b.clone() }, RelationKind::Refutes),
                (MoleculeLink::RefutedBy { source: a.clone() }, RelationKind::Refutes),
                (MoleculeLink::MergedFrom { ids: vec![a.clone()] }, RelationKind::MergedFrom),
                (MoleculeLink::MergedInto { id: b.clone() }, RelationKind::MergedFrom),
            ] {
                let edges = link.canonical_edges(&a);
                for (kind, _, _) in &edges {
                    proptest::prop_assert_eq!(*kind, expected);
                }
            }
        }
    }

    #[test]
    fn test_canonical_edges_skips_non_local_links() {
        // Cross-galaxy targets and free-form entanglement do not point
        // at a local `MoleculeId`, so `canonical_edges` returns empty.
        let me = MoleculeId::new("task-20260509-eeee").unwrap();
        let other = MoleculeId::new("task-20260509-ffff").unwrap();

        let cgb = MoleculeLink::CrossGalaxyBlocks {
            target: CrossGalaxyRef::new("mailroom", other.clone()),
        };
        assert!(cgb.canonical_edges(&me).is_empty());

        let cgbb = MoleculeLink::CrossGalaxyBlockedBy {
            source: CrossGalaxyRef::new("tenant-demo", other),
        };
        assert!(cgbb.canonical_edges(&me).is_empty());

        let ent = MoleculeLink::Entangled {
            target: "https://example.com/spec".to_owned(),
        };
        assert!(ent.canonical_edges(&me).is_empty());

        let xform = MoleculeLink::TransformedFrom {
            kind: crate::kind::MoleculeKind::Idea,
        };
        assert!(xform.canonical_edges(&me).is_empty());
    }
}
