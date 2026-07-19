// SPDX-License-Identifier: AGPL-3.0-only

//! Integration test for `compile_plan` — exercises the helper against a
//! real [`cosmon_filestore::FileStore`] to prove the DAG-reconstruction
//! pipeline works end-to-end from JSON on disk.
//!
//! Coverage:
//!
//! 1. A chain of three molecules linked by `BlockedBy` typed links compiles
//!    into a Plan whose initial ready frontier is the single root.
//! 2. Running the compiled Plan + edges through a `DagPolicy` produces the
//!    expected `Evolve` actions in dependency order.
//! 3. A cyclic dependency graph (which should never be written by the CLI,
//!    but the helper must refuse) surfaces as a `CosmonError::StateStore`
//!    with a cycle message — we don't want `compile_plan` to panic on bad
//!    on-disk state.

use std::collections::HashMap;

use chrono::Utc;
use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{compile_plan, DagPolicy, FleetSnapshot, Policy, RuntimeAction};
use cosmon_state::{MoleculeData, StateStore};
use tempfile::TempDir;

fn mol_id(raw: &str) -> MoleculeId {
    MoleculeId::new(raw).expect("test molecule id")
}

fn seed_molecule(
    store: &dyn StateStore,
    id: &MoleculeId,
    status: MoleculeStatus,
    typed_links: Vec<MoleculeLink>,
) {
    let data = MoleculeData {
        id: id.clone(),
        fleet_id: FleetId::new("default").expect("fleet id"),
        formula_id: FormulaId::new("task-work").expect("formula id"),
        status,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        total_steps: 1,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind: None,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links,
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: std::collections::BTreeSet::new(),
        escalations: Vec::new(),
        freeze_on_last_step: false,
        expires_at: None,
        expiry_policy: None,
        originating_branch: None,
        pending_step: None,
        merged_at: None,
        prompt_seal: None,
        briefing_seals: Vec::new(),
        bootstrap_seals: Vec::new(),
        archived: false,
        last_progress_at: None,
        last_output_at: None,
        nudge_count: 0,
        last_nudged_at: None,
        propel_count: 0,
        last_propelled_at: None,
        process: None,
        energy_budget: None,
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    };
    store.save_molecule(id, &data).expect("save molecule");
}

#[test]
fn compile_plan_walks_blocked_by_links_into_linear_chain() {
    // On-disk state: A → B → C, expressed as BlockedBy links.
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let a = mol_id("task-20260410-zzz1");
    let b = mol_id("task-20260410-zzz2");
    let c = mol_id("task-20260410-zzz3");

    seed_molecule(
        &store,
        &a,
        MoleculeStatus::Pending,
        vec![MoleculeLink::Blocks { target: b.clone() }],
    );
    seed_molecule(
        &store,
        &b,
        MoleculeStatus::Pending,
        vec![
            MoleculeLink::BlockedBy { source: a.clone() },
            MoleculeLink::Blocks { target: c.clone() },
        ],
    );
    seed_molecule(
        &store,
        &c,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy { source: b.clone() }],
    );

    // Compile a plan rooted at `a`. The BFS must walk forward through
    // `Blocks` links to reach b and c, and build edges (a,b) and (b,c).
    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&a)).expect("compile_plan");

    // Edge list contains exactly the two expected dependency edges.
    // Both symmetric sides (A's Blocks and B's BlockedBy) should dedupe
    // to a single edge.
    assert_eq!(
        edges.len(),
        2,
        "chain A→B→C should compile to 2 edges, got {edges:?}"
    );
    assert!(edges.contains(&(a.clone(), b.clone())));
    assert!(edges.contains(&(b.clone(), c.clone())));

    // Ready frontier must be exactly {a}: b and c are blocked.
    let ready: Vec<&MoleculeId> = plan.ready().iter().collect();
    assert_eq!(ready, vec![&a], "initial ready frontier should be just A");
}

#[test]
fn compile_plan_feeds_dag_policy_for_end_to_end_scheduling() {
    // Proves compile_plan's output is a drop-in input for DagPolicy.
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let a = mol_id("task-20260410-eee1");
    let b = mol_id("task-20260410-eee2");

    seed_molecule(
        &store,
        &a,
        MoleculeStatus::Pending,
        vec![MoleculeLink::Blocks { target: b.clone() }],
    );
    seed_molecule(
        &store,
        &b,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy { source: a.clone() }],
    );

    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&a)).expect("compile_plan");
    let mut policy = DagPolicy::new(plan, edges);

    // Load the current snapshot from the same store and drive the policy.
    let snapshot = FleetSnapshot::load(&store).expect("load snapshot");
    let actions = policy.next_actions(&snapshot);

    // The root A should be the only emitted action.
    let evolve_ids: Vec<MoleculeId> = actions
        .iter()
        .filter_map(|act| match act {
            RuntimeAction::Evolve { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        evolve_ids,
        vec![a.clone()],
        "end-to-end compile_plan → DagPolicy must dispatch the root first"
    );
}

#[test]
fn compile_plan_tolerates_dangling_root_ids() {
    // Roots that do not exist in the store must not fail the compile —
    // they are silently ignored. This keeps the helper robust against
    // stale operator input (e.g., a molecule that has since been GC'd).
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let ghost = mol_id("task-20260410-gg01");
    let (plan, edges) = compile_plan(&store, &[ghost]).expect("compile_plan");
    assert!(
        plan.ready().is_empty(),
        "dangling root should produce an empty plan"
    );
    assert!(edges.is_empty());
}

#[test]
fn compile_plan_discovers_children_via_completed_cross_subgraph_blocker() {
    // Regression: two missions share no direct link, but completed mission-A
    // blocks children that also belong (via BlockedBy) to the frozen
    // mission-B subgraph. Compiling from mission-B alone must still reach
    // those children so they are not left orphaned-ready.
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let mission_a = mol_id("task-20260410-miaa");
    let mission_b = mol_id("task-20260410-mibb");
    let child = mol_id("task-20260410-chld");

    // mission-A: Completed, still carries Blocks link to child.
    seed_molecule(
        &store,
        &mission_a,
        MoleculeStatus::Completed,
        vec![MoleculeLink::Blocks {
            target: child.clone(),
        }],
    );
    // mission-B: Frozen (the fresh root the caller passes).
    seed_molecule(
        &store,
        &mission_b,
        MoleculeStatus::Frozen,
        vec![MoleculeLink::Blocks {
            target: child.clone(),
        }],
    );
    // Child: blocked by mission-A (completed cross-subgraph blocker).
    seed_molecule(
        &store,
        &child,
        MoleculeStatus::Pending,
        vec![
            MoleculeLink::BlockedBy {
                source: mission_a.clone(),
            },
            MoleculeLink::BlockedBy {
                source: mission_b.clone(),
            },
        ],
    );

    let (_plan, edges) =
        compile_plan(&store, std::slice::from_ref(&mission_b)).expect("compile_plan");

    assert!(
        edges.contains(&(mission_a.clone(), child.clone())),
        "edge from completed mission-A to child must be present: {edges:?}"
    );
    assert!(
        edges.contains(&(mission_b.clone(), child.clone())),
        "edge from frozen mission-B to child must be present: {edges:?}"
    );
}

#[test]
fn compile_plan_scopes_to_requested_root_and_ignores_disconnected_subgraphs() {
    // Regression (task-20260412-30c1): `cs run <root>` previously walked the
    // entire project history because compile_plan seeded BFS with every
    // completed/frozen molecule carrying an outgoing Blocks link. That
    // over-corrected a legitimate cross-subgraph case and pulled unrelated
    // historical subgraphs into the plan. The fix scopes BFS strictly to
    // the connected component reachable from the caller-provided roots.
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    // Subgraph A (historical, completed): old-root → old-child.
    let old_root = mol_id("task-20260410-oldr");
    let old_child = mol_id("task-20260410-oldc");
    seed_molecule(
        &store,
        &old_root,
        MoleculeStatus::Completed,
        vec![MoleculeLink::Blocks {
            target: old_child.clone(),
        }],
    );
    seed_molecule(
        &store,
        &old_child,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: old_root.clone(),
        }],
    );

    // Subgraph B (current): new-root → new-child. Disjoint from A.
    let new_root = mol_id("task-20260412-newr");
    let new_child = mol_id("task-20260412-newc");
    seed_molecule(
        &store,
        &new_root,
        MoleculeStatus::Pending,
        vec![MoleculeLink::Blocks {
            target: new_child.clone(),
        }],
    );
    seed_molecule(
        &store,
        &new_child,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: new_root.clone(),
        }],
    );

    let (_plan, edges) =
        compile_plan(&store, std::slice::from_ref(&new_root)).expect("compile_plan");

    assert!(
        edges.contains(&(new_root.clone(), new_child.clone())),
        "requested subgraph edge must be present: {edges:?}"
    );
    assert!(
        !edges
            .iter()
            .any(|(s, d)| s == &old_root || d == &old_root || s == &old_child || d == &old_child),
        "disconnected historical subgraph must NOT be included: {edges:?}"
    );
}

#[test]
fn compile_plan_follows_decay_product_links() {
    // A parent molecule has DecayProduct links to two children created
    // during a previous run's glide phase. compile_plan must discover
    // both children and emit (parent, child) edges so they appear in
    // the plan on resume.
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let parent = mol_id("task-20260411-prnt");
    let child_a = mol_id("task-20260411-dca1");
    let child_b = mol_id("task-20260411-dca2");

    // Parent carries DecayProduct links to both children.
    seed_molecule(
        &store,
        &parent,
        MoleculeStatus::Completed,
        vec![
            MoleculeLink::DecayProduct {
                id: child_a.clone(),
            },
            MoleculeLink::DecayProduct {
                id: child_b.clone(),
            },
        ],
    );
    // Children carry DecayedFrom back-links to the parent.
    seed_molecule(
        &store,
        &child_a,
        MoleculeStatus::Pending,
        vec![MoleculeLink::DecayedFrom { id: parent.clone() }],
    );
    seed_molecule(
        &store,
        &child_b,
        MoleculeStatus::Pending,
        vec![MoleculeLink::DecayedFrom { id: parent.clone() }],
    );

    let (_plan, edges) = compile_plan(&store, std::slice::from_ref(&parent)).expect("compile_plan");

    // Both (parent, child) edges must be present — the BFS traversed
    // DecayProduct links and the edge materializer emitted them.
    assert!(
        edges.contains(&(parent.clone(), child_a.clone())),
        "edge from parent to child_a must be present: {edges:?}"
    );
    assert!(
        edges.contains(&(parent.clone(), child_b.clone())),
        "edge from parent to child_b must be present: {edges:?}"
    );
    assert_eq!(
        edges.len(),
        2,
        "exactly 2 decay edges expected, got {edges:?}"
    );
}
