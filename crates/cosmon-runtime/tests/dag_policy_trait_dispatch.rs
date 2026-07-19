// SPDX-License-Identifier: AGPL-3.0-only

//! Regression test for the trait-dispatch of `needs_recompile` / `recompile`.
//!
//! Before the fix, both methods were defined only on `impl DagPolicy`, not on
//! `impl Policy for DagPolicy`. Code that held a `Box<dyn Policy>` (as the
//! real `Runtime` does) would therefore dispatch to the default trait
//! implementations — `needs_recompile` always returned `false`, `recompile`
//! was a no-op — and the runtime never reloaded the on-disk edge set after a
//! decay splice. Spliced siblings with inter-child `BlockedBy` links appeared
//! ready simultaneously instead of honoring their chain.
//!
//! This test exercises the exact path the runtime takes: it wraps a
//! `DagPolicy` in `Box<dyn Policy>`, runs one tick after seeding a parent
//! splice, then calls `needs_recompile` / `recompile` through the trait
//! object. If the override is not visible through trait dispatch, the second
//! tick will dispatch every child molecule at once and the test fails.

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

/// A parent molecule decays into a 4-molecule chain (writer → fact-checker →
/// reviewer → editor) wired by `BlockedBy` edges on disk. After the parent
/// splice, only the writer must dispatch.
///
/// The key line is `let policy: Box<dyn Policy> = Box::new(DagPolicy::new(…))`.
/// That is exactly what `Runtime` holds. If `needs_recompile` / `recompile`
/// are not in `impl Policy for DagPolicy`, the trait object sees only the
/// defaults (false / no-op), the on-disk `BlockedBy` edges never load, and all
/// four children are surfaced as ready simultaneously.
#[test]
fn policy_trait_object_sees_recompile_override_after_decay_splice() {
    let tmp = TempDir::new().expect("tempdir");
    let store = FileStore::new(tmp.path());

    let parent = mol_id("task-20260410-par0");
    let writer = mol_id("task-20260410-wri1");
    let factchk = mol_id("task-20260410-fct2");
    let reviewer = mol_id("task-20260410-rev3");
    let editor = mol_id("task-20260410-edt4");

    // Parent decays into [writer, factchk, reviewer, editor].
    seed_molecule(
        &store,
        &parent,
        MoleculeStatus::Completed,
        vec![
            MoleculeLink::DecayProduct { id: writer.clone() },
            MoleculeLink::DecayProduct {
                id: factchk.clone(),
            },
            MoleculeLink::DecayProduct {
                id: reviewer.clone(),
            },
            MoleculeLink::DecayProduct { id: editor.clone() },
        ],
    );

    // Inter-child chain: writer → factchk → reviewer → editor.
    seed_molecule(
        &store,
        &writer,
        MoleculeStatus::Pending,
        vec![MoleculeLink::Blocks {
            target: factchk.clone(),
        }],
    );
    seed_molecule(
        &store,
        &factchk,
        MoleculeStatus::Pending,
        vec![
            MoleculeLink::BlockedBy {
                source: writer.clone(),
            },
            MoleculeLink::Blocks {
                target: reviewer.clone(),
            },
        ],
    );
    seed_molecule(
        &store,
        &reviewer,
        MoleculeStatus::Pending,
        vec![
            MoleculeLink::BlockedBy {
                source: factchk.clone(),
            },
            MoleculeLink::Blocks {
                target: editor.clone(),
            },
        ],
    );
    seed_molecule(
        &store,
        &editor,
        MoleculeStatus::Pending,
        vec![MoleculeLink::BlockedBy {
            source: reviewer.clone(),
        }],
    );

    // Bootstrap the policy only from the parent. `compile_plan` walks
    // Blocks/BlockedBy edges, so it will NOT traverse DecayProduct links —
    // the policy starts ignorant of the inter-child chain, just like the
    // real runtime does at the moment the parent completes.
    let (plan, edges) =
        compile_plan(&store, std::slice::from_ref(&parent)).expect("compile parent plan");

    // Hold the policy through a `Box<dyn Policy>`. This is the path the
    // real `Runtime` takes — if the override is not wired through the
    // trait, every child will dispatch in parallel.
    let mut policy: Box<dyn Policy> = Box::new(DagPolicy::new(plan, edges));

    // Tick 1: absorb the parent completion, splice in (parent,child) edges,
    // request a recompile.
    let snapshot = FleetSnapshot::load(&store).expect("snapshot 1");
    let _ = policy.next_actions(&snapshot);

    // The splice must be visible through the trait object.
    assert!(
        policy.needs_recompile(),
        "trait-dispatched needs_recompile() must return true after a decay splice; \
         the inherent method is invisible through Box<dyn Policy>"
    );

    // Recompile through the trait object: this must load the on-disk
    // BlockedBy chain into the policy's edge list.
    policy
        .recompile(&store as &dyn StateStore)
        .expect("trait-dispatched recompile() must load BlockedBy edges from disk");

    assert!(
        !policy.needs_recompile(),
        "recompile() must clear the flag so the runtime does not loop"
    );

    // Tick 2: only the writer is ready. The other three siblings remain
    // blocked by the freshly-loaded inter-child edges.
    let snapshot = FleetSnapshot::load(&store).expect("snapshot 2");
    let actions = policy.next_actions(&snapshot);

    let evolved: Vec<MoleculeId> = actions
        .into_iter()
        .filter_map(|act| match act {
            RuntimeAction::Evolve { id, .. } => Some(id),
            _ => None,
        })
        .collect();

    assert_eq!(
        evolved,
        vec![writer.clone()],
        "after a decay splice and recompile, only the root of the inter-child \
         chain (the writer) must dispatch; other siblings remain blocked. \
         Got: {evolved:?}"
    );
}
