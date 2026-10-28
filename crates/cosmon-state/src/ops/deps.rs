// SPDX-License-Identifier: AGPL-3.0-only

//! `deps` — the molecule relationship walker (blocking, lineage, citation).
//!
//! Read-only. Shared by `cs deps` and `GET /v1/molecules/:id/deps`: both
//! call [`deps`] and render its [`DepsView`], so the two surfaces cannot
//! disagree about what a molecule's neighbours are.
//!
//! Three edge families are walked, in both directions:
//!
//! - **blocking** — `Blocks` / `BlockedBy`;
//! - **lineage** — `DecayProduct` (downstream) / `DecayedFrom` (upstream);
//! - **citation** — `Refines` (downstream) / `RefinedBy` (upstream). These
//!   carry no progression semantics, they are purely cognitive.
//!
//! Cross-galaxy edges are listed from the target molecule only. They are
//! never traversed, and never resolved here: resolving one reads another
//! galaxy's state, which is a local-operator capability the CLI adds on
//! top of this view and a tenant-facing surface must not have.

use std::collections::{HashSet, VecDeque};

use cosmon_core::error::CosmonError;
use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::{CrossGalaxyRef, MoleculeLink};
use cosmon_core::molecule::MoleculeStatus;
use serde::Serialize;

use crate::ops::error::OpsError;
use crate::{MoleculeData, MoleculeFilter, StateStore};

/// Errors returned by [`deps`].
#[derive(Debug, thiserror::Error)]
pub enum DepsError {
    /// No molecule matches the given id or prefix.
    #[error("no molecule matching `{0}`")]
    MoleculeNotFound(String),
    /// A prefix matched more than one molecule.
    #[error("ambiguous prefix `{0}` matches {1} molecules")]
    AmbiguousPrefix(String, usize),
    /// The state store could not be read.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for DepsError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::AmbiguousPrefix(_, _) => "ambiguous-prefix",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::AmbiguousPrefix(_, _) => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

impl From<CosmonError> for DepsError {
    fn from(err: CosmonError) -> Self {
        Self::StoreUnavailable(err.to_string())
    }
}

/// Inputs for [`deps`].
#[derive(Debug, Clone, Default)]
pub struct DepsRequest {
    /// Walk the full transitive closure instead of only direct edges.
    pub transitive: bool,
}

/// Which edge family direction a walk follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Follow `BlockedBy` + `DecayedFrom` + `RefinedBy` — towards the roots.
    Upstream,
    /// Follow `Blocks` + `DecayProduct` + `Refines` — towards the leaves.
    Downstream,
}

/// Result of a successful [`deps`] call.
#[derive(Debug, Clone)]
pub struct DepsView {
    /// The molecule the walk started from.
    pub target: MoleculeData,
    /// Whether the walk was transitive.
    pub transitive: bool,
    /// Upstream molecule ids, in discovery order.
    pub upstream: Vec<MoleculeId>,
    /// Downstream molecule ids, in discovery order.
    pub downstream: Vec<MoleculeId>,
    /// Direct cross-galaxy upstream references of the target (unresolved).
    pub cross_upstream: Vec<CrossGalaxyRef>,
    /// Direct cross-galaxy downstream references of the target (unresolved).
    pub cross_downstream: Vec<CrossGalaxyRef>,
}

/// Wire shape of one cross-galaxy reference.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CrossGalaxyJson {
    /// Galaxy name the edge points into.
    pub galaxy: String,
    /// Molecule id inside that galaxy.
    pub mol_id: String,
    /// Canonical `galaxy:mol_id` spelling.
    #[serde(rename = "ref")]
    pub reference: String,
}

impl CrossGalaxyJson {
    /// Project a cross-galaxy reference onto its wire shape.
    #[must_use]
    pub fn from_ref(cgr: &CrossGalaxyRef) -> Self {
        Self {
            galaxy: cgr.galaxy.clone(),
            mol_id: cgr.mol_id.as_str().to_owned(),
            reference: cgr.to_canonical_string(),
        }
    }
}

/// Wire shape of a [`DepsView`] — the body `cs --json deps` prints and the
/// route returns. The CLI adds a `resolution` key to each cross-galaxy
/// entry; the route does not (see the module docs).
#[derive(Debug, Clone, Serialize)]
pub struct DepsJson {
    /// Target molecule id.
    pub molecule: String,
    /// Target molecule status.
    pub status: String,
    /// Whether the walk was transitive.
    pub transitive: bool,
    /// Upstream molecule ids.
    pub upstream: Vec<String>,
    /// Downstream molecule ids.
    pub downstream: Vec<String>,
    /// Direct cross-galaxy upstream references.
    pub cross_galaxy_upstream: Vec<CrossGalaxyJson>,
    /// Direct cross-galaxy downstream references.
    pub cross_galaxy_downstream: Vec<CrossGalaxyJson>,
}

impl DepsJson {
    /// Project a view onto its wire shape.
    #[must_use]
    pub fn from_view(view: &DepsView) -> Self {
        let ids = |v: &[MoleculeId]| v.iter().map(|i| i.as_str().to_owned()).collect();
        let cross = |v: &[CrossGalaxyRef]| v.iter().map(CrossGalaxyJson::from_ref).collect();
        Self {
            molecule: view.target.id.as_str().to_owned(),
            status: view.target.status.to_string(),
            transitive: view.transitive,
            upstream: ids(&view.upstream),
            downstream: ids(&view.downstream),
            cross_galaxy_upstream: cross(&view.cross_upstream),
            cross_galaxy_downstream: cross(&view.cross_downstream),
        }
    }
}

/// Walk a molecule's relationships.
///
/// `query` is an exact molecule id or an unambiguous prefix; an exact match
/// wins over a prefix match.
///
/// # Errors
///
/// [`DepsError::MoleculeNotFound`] when nothing matches,
/// [`DepsError::AmbiguousPrefix`] when a prefix matches several molecules,
/// [`DepsError::StoreUnavailable`] when the store cannot be read.
///
/// # `#[verb]` registration
///
/// Annotated as `GET /v1/molecules/:id/deps` (principal `tenant`).
#[cosmon_thin_macro::verb(method = "GET", path = "/v1/molecules/:id/deps", principal = "tenant")]
pub fn deps(
    store: &dyn StateStore,
    query: &str,
    request: &DepsRequest,
) -> Result<DepsView, DepsError> {
    let target_id = resolve_molecule_id(store, query)?;
    let target = store.load_molecule(&target_id)?;

    let (upstream, downstream) = if request.transitive {
        (
            walk_edges(store, &target_id, Direction::Upstream)?,
            walk_edges(store, &target_id, Direction::Downstream)?,
        )
    } else {
        (
            direct_neighbors(&target, Direction::Upstream),
            direct_neighbors(&target, Direction::Downstream),
        )
    };
    let cross_upstream = direct_cross_galaxy_neighbors(&target, Direction::Upstream);
    let cross_downstream = direct_cross_galaxy_neighbors(&target, Direction::Downstream);

    Ok(DepsView {
        target,
        transitive: request.transitive,
        upstream,
        downstream,
        cross_upstream,
        cross_downstream,
    })
}

/// Resolve a molecule id from an exact match or an unambiguous prefix.
///
/// # Errors
///
/// See [`deps`].
pub fn resolve_molecule_id(store: &dyn StateStore, query: &str) -> Result<MoleculeId, DepsError> {
    if let Ok(exact) = MoleculeId::new(query) {
        if store.load_molecule(&exact).is_ok() {
            return Ok(exact);
        }
    }
    let all = store.list_molecules(&MoleculeFilter::default())?;
    let matches: Vec<_> = all
        .iter()
        .filter(|m| m.id.as_str().starts_with(query))
        .collect();
    match matches.len() {
        0 => Err(DepsError::MoleculeNotFound(query.to_owned())),
        1 => Ok(matches[0].id.clone()),
        n => Err(DepsError::AmbiguousPrefix(query.to_owned(), n)),
    }
}

/// BFS closure from `start_id` in one direction, excluding `start_id`.
///
/// Cycle-safe: the visited set keeps a cycle from putting the start back
/// into its own list. Ids come back in discovery order.
///
/// # Errors
///
/// [`DepsError::StoreUnavailable`] when the start molecule cannot be read.
/// A dangling neighbour is still listed, it is just not expanded.
pub fn walk_edges(
    store: &dyn StateStore,
    start_id: &MoleculeId,
    dir: Direction,
) -> Result<Vec<MoleculeId>, DepsError> {
    let mut seen: HashSet<MoleculeId> = HashSet::new();
    seen.insert(start_id.clone());
    let mut queue: VecDeque<MoleculeId> = VecDeque::new();
    let mut out: Vec<MoleculeId> = Vec::new();

    let start = store.load_molecule(start_id)?;
    for id in direct_neighbors(&start, dir) {
        if seen.insert(id.clone()) {
            queue.push_back(id);
        }
    }
    while let Some(id) = queue.pop_front() {
        out.push(id.clone());
        if let Ok(mol) = store.load_molecule(&id) {
            for n in direct_neighbors(&mol, dir) {
                if seen.insert(n.clone()) {
                    queue.push_back(n);
                }
            }
        }
    }
    Ok(out)
}

/// Direct cross-galaxy references of one direction, in declaration order.
#[must_use]
pub fn direct_cross_galaxy_neighbors(mol: &MoleculeData, dir: Direction) -> Vec<CrossGalaxyRef> {
    mol.typed_links
        .iter()
        .filter_map(|link| match (dir, link) {
            (Direction::Upstream, MoleculeLink::CrossGalaxyBlockedBy { source }) => Some(source),
            (Direction::Downstream, MoleculeLink::CrossGalaxyBlocks { target }) => Some(target),
            _ => None,
        })
        .cloned()
        .collect()
}

/// Direct neighbours across progression, lineage and citation edges,
/// de-duplicated in first-seen order.
#[must_use]
pub fn direct_neighbors(mol: &MoleculeData, dir: Direction) -> Vec<MoleculeId> {
    let mut seen: HashSet<MoleculeId> = HashSet::new();
    let mut out: Vec<MoleculeId> = Vec::new();
    let lineage: Vec<&MoleculeId> = mol
        .typed_links
        .iter()
        .filter_map(|link| match (dir, link) {
            (Direction::Upstream, MoleculeLink::DecayedFrom { id })
            | (Direction::Downstream, MoleculeLink::DecayProduct { id }) => Some(id),
            _ => None,
        })
        .collect();
    let groups: [Vec<&MoleculeId>; 3] = match dir {
        Direction::Upstream => [mol.blocked_by(), lineage, mol.refined_by()],
        Direction::Downstream => [mol.blocks(), lineage, mol.refines()],
    };
    for group in groups {
        for id in group {
            if seen.insert(id.clone()) {
                out.push(id.clone());
            }
        }
    }
    out
}

/// Short status label for a molecule, or `None` when it is not in the store.
///
/// Used by the human rendering to flag dangling references.
#[must_use]
pub fn status_label(store: &dyn StateStore, id: &MoleculeId) -> Option<&'static str> {
    store.load_molecule(id).ok().map(|m| match m.status {
        MoleculeStatus::Completed => "completed",
        MoleculeStatus::Collapsed => "collapsed",
        MoleculeStatus::Running => "running",
        MoleculeStatus::Pending => "pending",
        MoleculeStatus::Queued => "queued",
        MoleculeStatus::Frozen => "frozen",
        MoleculeStatus::Starved => "starved",
        _ => "unknown",
    })
}
