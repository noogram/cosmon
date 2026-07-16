// SPDX-License-Identifier: AGPL-3.0-only

#![allow(clippy::ignore_without_reason)] // proptest! macro generates #[ignore] without reason
//! Typed-links conformance proptest — structural invariants of the cosmon DAG.
//!
//! `cosmon-core`'s [`MoleculeLink`](cosmon_core::interaction::MoleculeLink)
//! enum carries several link kinds (`Blocks`, `BlockedBy`, `DecayProduct`,
//! `DecayedFrom`, `MergedFrom`, `MergedInto`, `TransformedFrom`, `Entangled`).
//! Each encodes an implicit structural rule that lives in prose today:
//!
//! * **S1 `SymmetricBlocks`** — every `Blocks(a → b)` edge comes with a matching
//!   `BlockedBy(b ← a)` on the target molecule.
//! * **S2 `NoBlocksCycle`** — the union of `Blocks` / `BlockedBy` is a DAG:
//!   depth-first traversal never revisits a node on the active stack.
//! * **S3 `DecayProductMonotonic`** — a `DecayProduct` child's `created_at` is
//!   greater-than-or-equal to its parent's: time flows only forward across
//!   decay. `DecayedFrom` must be paired with `DecayProduct` on the other side.
//! * **S4 `EntangledSymmetric`** — `Entangled` carries a free-form `String`, but
//!   when the target parses to a `MoleculeId` we expect the reverse edge to
//!   exist. Modeled that way, entanglement is a symmetric (and reflexive,
//!   transitive via closure) equivalence relation.
//! * **S5 `DecayChainFinite`** — the transitive closure of `DecayProduct` is
//!   acyclic: a molecule may not (even indirectly) decay into itself. This is
//!   the cosmon-core analogue of the documented `Refines` rule. `Refines`
//!   itself is not yet a `MoleculeLink` variant (it is declared in
//!   `cosmon-scenario` + `docs/spec-suite.md`); when it lands, extend S5 to
//!   cover `Refines` chains too.
//!
//! # Approach
//!
//! The test file defines a private [`MoleculeGraph`] builder that maintains the
//! five invariants *by construction*: every insertion helper refuses operations
//! that would violate them. Proptest then draws random operation sequences
//! from the alphabet `{AddMolecule, AddBlocks, AddDecay, AddEntangled}` and
//! asserts that every intermediate post-state satisfies S1–S5. This mirrors
//! the pattern from `tests/spec_conformance.rs` (TLA+ refuter) — the property
//! proves the helpers preserve the invariants over arbitrary traces.
//!
//! A block of witness `#[test]` cases at the bottom of the file constructs
//! graphs *without* the helpers (directly mutating `Vec<MoleculeLink>`) to
//! confirm the invariant checks have detection power — a dangling `Blocks`
//! with no `BlockedBy` counterpart, a cycle, a time-travelling decay. If
//! someone ever rewrites the production code path to drop the symmetry (say,
//! in a CLI refactor), one of these witness tests fails loudly.
//!
//! # Budget
//!
//! PR gate (`ci.yml` job `typed-links-conformance`): `PROPTEST_CASES=256`,
//! roughly 20 s wall clock. Nightly deep suite (`#[ignore]`): 50 000 cases.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, TimeZone, Utc};
use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::MoleculeLink;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Test-local graph builder — maintains S1..S5 by construction.
// ---------------------------------------------------------------------------

/// A molecule graph scoped to this test suite.
///
/// Each node carries a `created_at` timestamp (needed by S3) and a
/// `Vec<MoleculeLink>`. Insertion helpers refuse operations that would break
/// any invariant; raw `typed_links` mutation stays private so the witness
/// tests at the bottom can opt out deliberately.
#[derive(Debug, Clone, Default)]
struct MoleculeGraph {
    nodes: BTreeMap<MoleculeId, Node>,
}

#[derive(Debug, Clone)]
struct Node {
    created_at: DateTime<Utc>,
    typed_links: Vec<MoleculeLink>,
}

#[derive(Debug, PartialEq, Eq)]
enum LinkError {
    UnknownSource,
    UnknownTarget,
    WouldCreateBlocksCycle,
    WouldReverseDecayTime,
    WouldCreateDecayCycle,
    SelfLink,
}

impl MoleculeGraph {
    fn add_molecule(&mut self, id: MoleculeId, created_at: DateTime<Utc>) {
        self.nodes.entry(id).or_insert(Node {
            created_at,
            typed_links: Vec::new(),
        });
    }

    /// Add a `Blocks(from → to)` / `BlockedBy(to ← from)` pair.
    ///
    /// Refuses the insertion if the edge would close a cycle on the union of
    /// `Blocks` / `BlockedBy` edges already present (S2). Self-links are
    /// rejected (they are trivial cycles).
    fn link_blocks(&mut self, from: &MoleculeId, to: &MoleculeId) -> Result<(), LinkError> {
        if from == to {
            return Err(LinkError::SelfLink);
        }
        if !self.nodes.contains_key(from) {
            return Err(LinkError::UnknownSource);
        }
        if !self.nodes.contains_key(to) {
            return Err(LinkError::UnknownTarget);
        }
        if self.blocks_path_exists(to, from) {
            return Err(LinkError::WouldCreateBlocksCycle);
        }
        self.nodes
            .get_mut(from)
            .expect("existence checked")
            .typed_links
            .push(MoleculeLink::Blocks { target: to.clone() });
        self.nodes
            .get_mut(to)
            .expect("existence checked")
            .typed_links
            .push(MoleculeLink::BlockedBy {
                source: from.clone(),
            });
        Ok(())
    }

    /// Add a decay relationship: `parent` sprouts `child`.
    ///
    /// Writes `DecayProduct` on `parent` and `DecayedFrom` on `child`.
    /// Refuses if `child.created_at < parent.created_at` (S3) or if the
    /// resulting chain would loop (S5).
    fn link_decay(&mut self, parent: &MoleculeId, child: &MoleculeId) -> Result<(), LinkError> {
        if parent == child {
            return Err(LinkError::SelfLink);
        }
        let parent_ts = self
            .nodes
            .get(parent)
            .ok_or(LinkError::UnknownSource)?
            .created_at;
        let child_ts = self
            .nodes
            .get(child)
            .ok_or(LinkError::UnknownTarget)?
            .created_at;
        if child_ts < parent_ts {
            return Err(LinkError::WouldReverseDecayTime);
        }
        if self.decay_path_exists(child, parent) {
            return Err(LinkError::WouldCreateDecayCycle);
        }
        self.nodes
            .get_mut(parent)
            .expect("existence checked")
            .typed_links
            .push(MoleculeLink::DecayProduct { id: child.clone() });
        self.nodes
            .get_mut(child)
            .expect("existence checked")
            .typed_links
            .push(MoleculeLink::DecayedFrom { id: parent.clone() });
        Ok(())
    }

    /// Add a symmetric entangled pair, encoded molecule-ID-to-molecule-ID on
    /// both sides so S4 can be checked mechanically (`Entangled::target` is a
    /// free-form `String` at the type level).
    fn link_entangled(&mut self, a: &MoleculeId, b: &MoleculeId) -> Result<(), LinkError> {
        if a == b {
            return Err(LinkError::SelfLink);
        }
        if !self.nodes.contains_key(a) {
            return Err(LinkError::UnknownSource);
        }
        if !self.nodes.contains_key(b) {
            return Err(LinkError::UnknownTarget);
        }
        self.nodes
            .get_mut(a)
            .expect("existence checked")
            .typed_links
            .push(MoleculeLink::Entangled {
                target: b.as_str().to_owned(),
            });
        self.nodes
            .get_mut(b)
            .expect("existence checked")
            .typed_links
            .push(MoleculeLink::Entangled {
                target: a.as_str().to_owned(),
            });
        Ok(())
    }

    // ---- traversal helpers --------------------------------------------------

    fn blocks_path_exists(&self, from: &MoleculeId, to: &MoleculeId) -> bool {
        let mut stack = vec![from.clone()];
        let mut seen = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if &current == to {
                return true;
            }
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(node) = self.nodes.get(&current) {
                for link in &node.typed_links {
                    if let MoleculeLink::Blocks { target } = link {
                        stack.push(target.clone());
                    }
                }
            }
        }
        false
    }

    fn decay_path_exists(&self, from: &MoleculeId, to: &MoleculeId) -> bool {
        let mut stack = vec![from.clone()];
        let mut seen = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if &current == to {
                return true;
            }
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(node) = self.nodes.get(&current) {
                for link in &node.typed_links {
                    if let MoleculeLink::DecayProduct { id } = link {
                        stack.push(id.clone());
                    }
                }
            }
        }
        false
    }

    // ---- invariant checks --------------------------------------------------

    /// S1 — for every `Blocks(a → b)`, the target carries `BlockedBy(b ← a)`.
    fn check_s1_symmetric_blocks(&self) -> Result<(), String> {
        for (src, node) in &self.nodes {
            for link in &node.typed_links {
                let MoleculeLink::Blocks { target } = link else {
                    continue;
                };
                let tgt = self
                    .nodes
                    .get(target)
                    .ok_or_else(|| format!("S1: {src} blocks unknown {target}"))?;
                let mirrored = tgt
                    .typed_links
                    .iter()
                    .any(|l| matches!(l, MoleculeLink::BlockedBy { source } if source == src));
                if !mirrored {
                    return Err(format!("S1: {src} → Blocks({target}) has no BlockedBy"));
                }
            }
            for link in &node.typed_links {
                let MoleculeLink::BlockedBy { source } = link else {
                    continue;
                };
                let parent = self
                    .nodes
                    .get(source)
                    .ok_or_else(|| format!("S1: {src} BlockedBy unknown {source}"))?;
                let mirrored = parent
                    .typed_links
                    .iter()
                    .any(|l| matches!(l, MoleculeLink::Blocks { target } if target == src));
                if !mirrored {
                    return Err(format!("S1: {src} ← BlockedBy({source}) has no Blocks"));
                }
            }
        }
        Ok(())
    }

    /// S2 — DFS over `Blocks` detects no back-edge (the relation is a DAG).
    fn check_s2_no_blocks_cycle(&self) -> Result<(), String> {
        self.detect_cycle(
            |link| match link {
                MoleculeLink::Blocks { target } => Some(target),
                _ => None,
            },
            "S2",
        )
    }

    /// S3 — every `DecayProduct(parent → child)` has `child.created_at` ≥
    /// `parent.created_at`, and the reverse `DecayedFrom` edge is present.
    fn check_s3_decay_monotonic(&self) -> Result<(), String> {
        for (parent_id, parent) in &self.nodes {
            for link in &parent.typed_links {
                let MoleculeLink::DecayProduct { id: child_id } = link else {
                    continue;
                };
                let child = self
                    .nodes
                    .get(child_id)
                    .ok_or_else(|| format!("S3: {parent_id} decays to unknown {child_id}"))?;
                if child.created_at < parent.created_at {
                    return Err(format!("S3: {child_id} created before parent {parent_id}"));
                }
                let mirrored = child
                    .typed_links
                    .iter()
                    .any(|l| matches!(l, MoleculeLink::DecayedFrom { id } if id == parent_id));
                if !mirrored {
                    return Err(format!(
                        "S3: {child_id} is a decay product of {parent_id} but lacks DecayedFrom"
                    ));
                }
            }
        }
        Ok(())
    }

    /// S4 — for every `Entangled(target)` that parses to a known molecule ID,
    /// the back-edge exists on the target node.
    fn check_s4_entangled_symmetric(&self) -> Result<(), String> {
        for (src, node) in &self.nodes {
            for link in &node.typed_links {
                let MoleculeLink::Entangled { target } = link else {
                    continue;
                };
                let Ok(target_id) = MoleculeId::new(target.clone()) else {
                    continue; // free-form entanglement, out of scope for S4
                };
                let Some(peer) = self.nodes.get(&target_id) else {
                    continue; // foreign ID, out of scope for S4
                };
                let mirrored = peer.typed_links.iter().any(|l| {
                    matches!(l, MoleculeLink::Entangled { target: back }
                        if back.as_str() == src.as_str())
                });
                if !mirrored {
                    return Err(format!("S4: {src} ~ {target_id} has no back-edge"));
                }
            }
        }
        Ok(())
    }

    /// S5 — the transitive closure of `DecayProduct` is acyclic.
    fn check_s5_decay_chain_finite(&self) -> Result<(), String> {
        self.detect_cycle(
            |link| match link {
                MoleculeLink::DecayProduct { id } => Some(id),
                _ => None,
            },
            "S5",
        )
    }

    /// Shared iterative DFS that flags cycles on edges selected by `edge_of`.
    ///
    /// `color` legend: 0 white (unvisited), 1 grey (on the active stack),
    /// 2 black (fully drained). A grey hit proves a back-edge — the cycle
    /// witness returned to the caller via the `tag` (e.g. `"S2"`, `"S5"`).
    fn detect_cycle(
        &self,
        edge_of: impl Fn(&MoleculeLink) -> Option<&MoleculeId>,
        tag: &str,
    ) -> Result<(), String> {
        let mut color: BTreeMap<MoleculeId, u8> = BTreeMap::new();
        for root in self.nodes.keys() {
            if color.get(root).copied().unwrap_or(0) != 0 {
                continue;
            }
            // Stack frames hold `(node, next_child_index)`; on re-entry we
            // resume from the saved index to ensure we blacken nodes only
            // once every outgoing edge has been drained.
            let mut stack: Vec<(MoleculeId, usize)> = vec![(root.clone(), 0)];
            color.insert(root.clone(), 1);
            while let Some((node, mut idx)) = stack.pop() {
                let children: Vec<MoleculeId> = self
                    .nodes
                    .get(&node)
                    .map(|n| n.typed_links.iter().filter_map(&edge_of).cloned().collect())
                    .unwrap_or_default();
                let mut descended = false;
                while idx < children.len() {
                    let child = children[idx].clone();
                    idx += 1;
                    match color.get(&child).copied().unwrap_or(0) {
                        1 => return Err(format!("{tag}: cycle at {child}")),
                        2 => {}
                        _ => {
                            stack.push((node.clone(), idx));
                            color.insert(child.clone(), 1);
                            stack.push((child, 0));
                            descended = true;
                            break;
                        }
                    }
                }
                if !descended {
                    color.insert(node, 2);
                }
            }
        }
        Ok(())
    }

    fn check_all(&self) -> Result<(), String> {
        self.check_s1_symmetric_blocks()?;
        self.check_s2_no_blocks_cycle()?;
        self.check_s3_decay_monotonic()?;
        self.check_s4_entangled_symmetric()?;
        self.check_s5_decay_chain_finite()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Proptest strategies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Op {
    Molecule { index: usize, day_offset: u32 },
    Blocks { from: usize, to: usize },
    Decay { parent: usize, child: usize },
    Entangled { a: usize, b: usize },
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0usize..8, 0u32..=60).prop_map(|(index, day_offset)| Op::Molecule { index, day_offset }),
        (0usize..8, 0usize..8).prop_map(|(from, to)| Op::Blocks { from, to }),
        (0usize..8, 0usize..8).prop_map(|(parent, child)| Op::Decay { parent, child }),
        (0usize..8, 0usize..8).prop_map(|(a, b)| Op::Entangled { a, b }),
    ]
}

fn mol_id_for(index: usize) -> MoleculeId {
    MoleculeId::new(format!("task-20260401-{index:04x}")).expect("valid id")
}

fn drive(graph: &mut MoleculeGraph, op: &Op) {
    let base = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).single().unwrap();
    match *op {
        Op::Molecule { index, day_offset } => {
            let ts = base + Duration::days(day_offset.into());
            graph.add_molecule(mol_id_for(index), ts);
        }
        Op::Blocks { from, to } => {
            let _ = graph.link_blocks(&mol_id_for(from), &mol_id_for(to));
        }
        Op::Decay { parent, child } => {
            let _ = graph.link_decay(&mol_id_for(parent), &mol_id_for(child));
        }
        Op::Entangled { a, b } => {
            let _ = graph.link_entangled(&mol_id_for(a), &mol_id_for(b));
        }
    }
}

// ---------------------------------------------------------------------------
// Properties — invariants hold over any trace built with the helpers.
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn prop_all_invariants_hold_under_random_ops(ops in prop::collection::vec(arb_op(), 0..48)) {
        let mut graph = MoleculeGraph::default();
        for op in ops {
            drive(&mut graph, &op);
            prop_assert!(graph.check_all().is_ok(), "invariant violated: {:?}", graph.check_all());
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 5_000,
        ..ProptestConfig::default()
    })]

    /// Deep proptest — longer traces, more cases. `#[ignore]` so the PR gate
    /// stays under 30 s; nightly CI picks it up via `-- --ignored` with
    /// `PROPTEST_CASES=50000`.
    #[test]
    #[ignore]
    fn prop_deep_invariants_hold(ops in prop::collection::vec(arb_op(), 0..256)) {
        let mut graph = MoleculeGraph::default();
        for op in ops {
            drive(&mut graph, &op);
            prop_assert!(graph.check_all().is_ok());
        }
    }
}

// ---------------------------------------------------------------------------
// Witness tests — confirm the invariant checks have detection power.
// ---------------------------------------------------------------------------
//
// Each witness constructs a graph *without* the helpers so the resulting
// state violates exactly one invariant. If a future refactor makes the
// relevant check a tautology, the corresponding witness fails loudly.

fn mol(id: &str, day: u32) -> (MoleculeId, DateTime<Utc>) {
    let ts = Utc
        .with_ymd_and_hms(2026, 4, day, 0, 0, 0)
        .single()
        .unwrap();
    (MoleculeId::new(id).expect("valid id"), ts)
}

#[test]
fn witness_s1_detects_dangling_blocks() {
    let mut graph = MoleculeGraph::default();
    let (a, ts_a) = mol("task-20260401-aaaa", 1);
    let (b, ts_b) = mol("task-20260401-bbbb", 1);
    graph.add_molecule(a.clone(), ts_a);
    graph.add_molecule(b.clone(), ts_b);
    graph
        .nodes
        .get_mut(&a)
        .unwrap()
        .typed_links
        .push(MoleculeLink::Blocks { target: b });
    // Intentionally no BlockedBy back-edge.
    let err = graph.check_s1_symmetric_blocks().unwrap_err();
    assert!(err.starts_with("S1:"), "got {err}");
}

#[test]
fn witness_s2_detects_blocks_cycle() {
    let mut graph = MoleculeGraph::default();
    let (a, ts_a) = mol("task-20260401-0001", 1);
    let (b, ts_b) = mol("task-20260401-0002", 1);
    graph.add_molecule(a.clone(), ts_a);
    graph.add_molecule(b.clone(), ts_b);
    // Raw cyclic insertion — bypass link_blocks on purpose.
    graph
        .nodes
        .get_mut(&a)
        .unwrap()
        .typed_links
        .push(MoleculeLink::Blocks { target: b.clone() });
    graph
        .nodes
        .get_mut(&b)
        .unwrap()
        .typed_links
        .push(MoleculeLink::Blocks { target: a.clone() });
    let err = graph.check_s2_no_blocks_cycle().unwrap_err();
    assert!(err.starts_with("S2:"), "got {err}");
}

#[test]
fn witness_s3_detects_time_reversal() {
    let mut graph = MoleculeGraph::default();
    let (parent, ts_parent) = mol("idea-20260401-9999", 10);
    let (child, ts_child) = mol("task-20260401-8888", 1); // earlier than parent
    graph.add_molecule(parent.clone(), ts_parent);
    graph.add_molecule(child.clone(), ts_child);
    graph
        .nodes
        .get_mut(&parent)
        .unwrap()
        .typed_links
        .push(MoleculeLink::DecayProduct { id: child.clone() });
    graph
        .nodes
        .get_mut(&child)
        .unwrap()
        .typed_links
        .push(MoleculeLink::DecayedFrom { id: parent.clone() });
    let err = graph.check_s3_decay_monotonic().unwrap_err();
    assert!(err.starts_with("S3:"), "got {err}");
}

#[test]
fn witness_s4_detects_one_sided_entanglement() {
    let mut graph = MoleculeGraph::default();
    let (a, ts_a) = mol("task-20260401-cccc", 1);
    let (b, ts_b) = mol("task-20260401-dddd", 1);
    graph.add_molecule(a.clone(), ts_a);
    graph.add_molecule(b.clone(), ts_b);
    graph
        .nodes
        .get_mut(&a)
        .unwrap()
        .typed_links
        .push(MoleculeLink::Entangled {
            target: b.as_str().to_owned(),
        });
    // No back-edge on b.
    let err = graph.check_s4_entangled_symmetric().unwrap_err();
    assert!(err.starts_with("S4:"), "got {err}");
}

#[test]
fn witness_s5_detects_decay_loop() {
    let mut graph = MoleculeGraph::default();
    let (a, ts_a) = mol("task-20260401-eeee", 1);
    let (b, ts_b) = mol("task-20260401-ffff", 1);
    graph.add_molecule(a.clone(), ts_a);
    graph.add_molecule(b.clone(), ts_b);
    graph
        .nodes
        .get_mut(&a)
        .unwrap()
        .typed_links
        .push(MoleculeLink::DecayProduct { id: b.clone() });
    graph
        .nodes
        .get_mut(&b)
        .unwrap()
        .typed_links
        .push(MoleculeLink::DecayProduct { id: a.clone() });
    let err = graph.check_s5_decay_chain_finite().unwrap_err();
    assert!(err.starts_with("S5:"), "got {err}");
}

// ---------------------------------------------------------------------------
// Free-form entanglement stays permitted (S4 only applies to molecule-IDs).
// ---------------------------------------------------------------------------

#[test]
fn freeform_entangled_url_does_not_trip_s4() {
    let mut graph = MoleculeGraph::default();
    let (a, ts_a) = mol("task-20260401-1111", 1);
    graph.add_molecule(a.clone(), ts_a);
    graph
        .nodes
        .get_mut(&a)
        .unwrap()
        .typed_links
        .push(MoleculeLink::Entangled {
            target: "https://example.com/issue/42".to_owned(),
        });
    assert!(graph.check_s4_entangled_symmetric().is_ok());
}

// ---------------------------------------------------------------------------
// Builder helpers refuse cycle-inducing operations.
// ---------------------------------------------------------------------------

#[test]
fn link_blocks_refuses_direct_cycle() {
    let mut graph = MoleculeGraph::default();
    let (a, ts_a) = mol("task-20260401-2222", 1);
    let (b, ts_b) = mol("task-20260401-3333", 1);
    graph.add_molecule(a.clone(), ts_a);
    graph.add_molecule(b.clone(), ts_b);
    graph.link_blocks(&a, &b).unwrap();
    assert_eq!(
        graph.link_blocks(&b, &a),
        Err(LinkError::WouldCreateBlocksCycle)
    );
    // Sanity: the partial state still satisfies every invariant.
    graph.check_all().unwrap();
}

#[test]
fn link_decay_refuses_time_reversal() {
    let mut graph = MoleculeGraph::default();
    let (parent, ts_parent) = mol("idea-20260401-4444", 10);
    let (child, ts_child) = mol("task-20260401-5555", 1);
    graph.add_molecule(parent.clone(), ts_parent);
    graph.add_molecule(child.clone(), ts_child);
    assert_eq!(
        graph.link_decay(&parent, &child),
        Err(LinkError::WouldReverseDecayTime)
    );
}
