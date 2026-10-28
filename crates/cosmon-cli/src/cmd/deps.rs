// SPDX-License-Identifier: AGPL-3.0-only

//! `cs deps` — show molecule blocking, lineage, and citation links.
//!
//! Read-only relationship walker for blocking links (`Blocks` and `BlockedBy`),
//! lineage links (`DecayProduct` and `DecayedFrom`), and citation links
//! (`Refines` and `RefinedBy`, used mainly by
//! [`Constellation`](cosmon_core::kind::MoleculeKind::Constellation)
//! molecules — the fil-rouge artifacts). Shows upstream and downstream
//! relationships and supports transitive traversal via BFS.
//!
//! `Refines` edges are traversed along the same directions as `Blocks`:
//! a molecule's `Refines` targets appear under **downstream** (the things
//! this molecule points at, cognitively), and its `RefinedBy` sources
//! appear under **upstream** (the citers). They do not carry progression
//! semantics — they are purely cognitive citations.
//!
//! This is the human-facing counterpart of what the future resident runtime's
//! `DagPolicy` (ADR-016 Phase 3) consumes programmatically. It exists so
//! operators can inspect the DAG before the runtime ships.
//!
//! Read-only by construction: never mutates state, never spawns workers.

use cosmon_core::interaction::CrossGalaxyRef;
use cosmon_state::ops::deps::{deps, status_label, DepsJson, DepsRequest};
use cosmon_state::StateStore;

use super::cross_galaxy::{resolve_cross_galaxy_ref, CrossGalaxyResolution};
use super::Context;

/// Arguments for the `deps` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID (exact or prefix) whose dependencies should be shown.
    pub molecule: String,

    /// Walk the full transitive closure instead of only direct edges.
    #[arg(long)]
    pub transitive: bool,
}

/// Execute the `deps` command.
///
/// The walk is [`cosmon_state::ops::deps::deps`], the same operation
/// `GET /v1/molecules/:id/deps` calls. This module adds what only a local
/// operator can do: resolving cross-galaxy references against the other
/// galaxies on this machine.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let view = deps(
        store.as_ref(),
        &args.molecule,
        &DepsRequest {
            transitive: args.transitive,
        },
    )?;

    if ctx.json {
        emit_json(&DepsJson::from_view(&view), &view);
    } else {
        emit_human(store.as_ref(), &view);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn emit_json(wire: &DepsJson, view: &cosmon_state::ops::deps::DepsView) {
    let mut out = serde_json::to_value(wire).unwrap_or(serde_json::Value::Null);
    // The CLI alone enriches cross-galaxy edges with their current
    // resolution so consumers can mark unreachable galaxies without a
    // second round-trip.
    out["cross_galaxy_upstream"] = view.cross_upstream.iter().map(cross_galaxy_json).collect();
    out["cross_galaxy_downstream"] = view
        .cross_downstream
        .iter()
        .map(cross_galaxy_json)
        .collect();
    println!("{out}");
}

/// Render a cross-galaxy reference as JSON, including its current
/// resolution status.
fn cross_galaxy_json(cgr: &CrossGalaxyRef) -> serde_json::Value {
    let resolution = resolve_cross_galaxy_ref(cgr);
    serde_json::json!({
        "galaxy": cgr.galaxy,
        "mol_id": cgr.mol_id.as_str(),
        "ref": cgr.to_canonical_string(),
        "resolution": resolution,
    })
}

fn emit_human(store: &dyn StateStore, view: &cosmon_state::ops::deps::DepsView) {
    let label = if view.transitive {
        "transitive"
    } else {
        "direct"
    };
    println!(
        "Deps for {} ({}) — {label}",
        view.target.id, view.target.status
    );
    println!();
    println!("  ⬆ Upstream links ({}):", view.upstream.len());
    if view.upstream.is_empty() && view.cross_upstream.is_empty() {
        println!("    (none — this molecule has no upstream links)");
    } else {
        for id in &view.upstream {
            println!("    - {id}{}", status_hint(store, id));
        }
        for cgr in &view.cross_upstream {
            println!("    - {cgr}{}", cross_galaxy_status_hint(cgr));
        }
    }
    println!();
    println!("  ⬇ Downstream links ({}):", view.downstream.len());
    if view.downstream.is_empty() && view.cross_downstream.is_empty() {
        println!("    (none — this molecule has no downstream links)");
    } else {
        for id in &view.downstream {
            println!("    - {id}{}", status_hint(store, id));
        }
        for cgr in &view.cross_downstream {
            println!("    - {cgr}{}", cross_galaxy_status_hint(cgr));
        }
    }
}

/// Render a short status suffix for a cross-galaxy reference.
fn cross_galaxy_status_hint(cgr: &CrossGalaxyRef) -> String {
    match resolve_cross_galaxy_ref(cgr) {
        CrossGalaxyResolution::Resolved { status, .. } => format!(" [cross-galaxy/{status}]"),
        CrossGalaxyResolution::MoleculeMissing { .. } => " [cross-galaxy/MISSING]".to_owned(),
        CrossGalaxyResolution::GalaxyUnknown => " [cross-galaxy/UNKNOWN]".to_owned(),
    }
}

/// Short status suffix for a molecule id: `" [status]"`, or `" [MISSING]"`
/// for a dangling reference the user should fix.
fn status_hint(store: &dyn StateStore, id: &cosmon_core::id::MoleculeId) -> String {
    match status_label(store, id) {
        Some(label) => format!(" [{label}]"),
        None => " [MISSING]".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::MoleculeId;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::interaction::MoleculeLink;
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::ops::deps::{direct_neighbors, resolve_molecule_id, walk_edges, Direction};
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        (tmp, store)
    }

    fn sample(id: &str) -> MoleculeData {
        MoleculeData {
            harvest_reason: None,
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            base_branch: None,
            protected_paths: Vec::new(),
            pending_step: None,
            merged_at: None,
            non_integration: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            collapse_receipt: None,
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
            adapter: None,
        }
    }

    /// Build a 3-molecule chain A -blocks-> B -blocks-> C and persist it.
    /// Symmetry is maintained manually for test determinism.
    fn build_chain(store: &FileStore) -> (MoleculeId, MoleculeId, MoleculeId) {
        let a_id = MoleculeId::new("task-20260409-aaaa").unwrap();
        let b_id = MoleculeId::new("task-20260409-bbbb").unwrap();
        let c_id = MoleculeId::new("task-20260409-cccc").unwrap();

        let mut a = sample(a_id.as_str());
        a.typed_links.push(MoleculeLink::Blocks {
            target: b_id.clone(),
        });

        let mut b = sample(b_id.as_str());
        b.typed_links.push(MoleculeLink::BlockedBy {
            source: a_id.clone(),
        });
        b.typed_links.push(MoleculeLink::Blocks {
            target: c_id.clone(),
        });

        let mut c = sample(c_id.as_str());
        c.typed_links.push(MoleculeLink::BlockedBy {
            source: b_id.clone(),
        });

        store.save_molecule(&a.id, &a).unwrap();
        store.save_molecule(&b.id, &b).unwrap();
        store.save_molecule(&c.id, &c).unwrap();

        (a_id, b_id, c_id)
    }

    #[test]
    fn test_direct_deps_show_only_immediate_neighbors() {
        let (_tmp, store) = make_store();
        let (a_id, b_id, c_id) = build_chain(&store);

        // Middle molecule B — direct deps should be {A up, C down}.
        let b = store.load_molecule(&b_id).unwrap();
        let upstream: Vec<MoleculeId> = b.blocked_by().into_iter().cloned().collect();
        let downstream: Vec<MoleculeId> = b.blocks().into_iter().cloned().collect();
        assert_eq!(upstream, vec![a_id]);
        assert_eq!(downstream, vec![c_id]);
    }

    #[test]
    fn frozen_planner_lineage_is_visible_through_pipeline_closure() {
        let (_tmp, store) = make_store();
        let mission_id = MoleculeId::new("mission-20261001-aaaa").unwrap();
        let first_id = MoleculeId::new("task-20261001-bbbb").unwrap();
        let second_id = MoleculeId::new("task-20261001-cccc").unwrap();
        let mut mission = sample(mission_id.as_str());
        mission.status = MoleculeStatus::Frozen;
        mission.typed_links.push(MoleculeLink::DecayProduct {
            id: first_id.clone(),
        });
        mission.typed_links.push(MoleculeLink::DecayProduct {
            id: second_id.clone(),
        });
        let mut first = sample(first_id.as_str());
        first.typed_links.push(MoleculeLink::DecayedFrom {
            id: mission_id.clone(),
        });
        first.typed_links.push(MoleculeLink::Blocks {
            target: second_id.clone(),
        });
        let mut second = sample(second_id.as_str());
        second.typed_links.push(MoleculeLink::DecayedFrom {
            id: mission_id.clone(),
        });
        second.typed_links.push(MoleculeLink::BlockedBy {
            source: first_id.clone(),
        });
        for molecule in [&mission, &first, &second] {
            store.save_molecule(&molecule.id, molecule).unwrap();
        }

        assert_eq!(
            (
                walk_edges(&store, &mission_id, Direction::Upstream).unwrap(),
                walk_edges(&store, &mission_id, Direction::Downstream).unwrap()
            )
                .1,
            vec![first_id.clone(), second_id]
        );
        assert_eq!(
            direct_neighbors(&first, Direction::Upstream),
            vec![mission_id]
        );
        assert!(
            first.blocked_by().is_empty(),
            "lineage must not become a BlockedBy edge"
        );
    }

    #[test]
    fn test_transitive_walk_from_root_downstream() {
        let (_tmp, store) = make_store();
        let (a_id, b_id, c_id) = build_chain(&store);

        let (upstream, downstream) = (
            walk_edges(&store, &a_id, Direction::Upstream).unwrap(),
            walk_edges(&store, &a_id, Direction::Downstream).unwrap(),
        );
        assert!(upstream.is_empty(), "A is root, no upstream expected");
        // A blocks B which blocks C → full downstream closure = [B, C].
        assert_eq!(downstream.len(), 2);
        assert!(downstream.contains(&b_id));
        assert!(downstream.contains(&c_id));
    }

    #[test]
    fn test_transitive_walk_from_leaf_upstream() {
        let (_tmp, store) = make_store();
        let (a_id, b_id, c_id) = build_chain(&store);

        let (upstream, downstream) = (
            walk_edges(&store, &c_id, Direction::Upstream).unwrap(),
            walk_edges(&store, &c_id, Direction::Downstream).unwrap(),
        );
        assert!(downstream.is_empty(), "C is leaf, no downstream expected");
        // C is blocked_by B which is blocked_by A → full upstream = [B, A].
        assert_eq!(upstream.len(), 2);
        assert!(upstream.contains(&a_id));
        assert!(upstream.contains(&b_id));
    }

    #[test]
    fn test_transitive_walk_cycle_safe() {
        let (_tmp, store) = make_store();
        // Construct an artificial cycle: A blocks B, B blocks A. Not a valid
        // DAG semantically but the walker must not infinite-loop on it.
        let a_id = MoleculeId::new("task-20260409-cyc1").unwrap();
        let b_id = MoleculeId::new("task-20260409-cyc2").unwrap();

        let mut a = sample(a_id.as_str());
        a.typed_links.push(MoleculeLink::Blocks {
            target: b_id.clone(),
        });
        a.typed_links.push(MoleculeLink::BlockedBy {
            source: b_id.clone(),
        });

        let mut b = sample(b_id.as_str());
        b.typed_links.push(MoleculeLink::BlockedBy {
            source: a_id.clone(),
        });
        b.typed_links.push(MoleculeLink::Blocks {
            target: a_id.clone(),
        });

        store.save_molecule(&a.id, &a).unwrap();
        store.save_molecule(&b.id, &b).unwrap();

        let (upstream, downstream) = (
            walk_edges(&store, &a_id, Direction::Upstream).unwrap(),
            walk_edges(&store, &a_id, Direction::Downstream).unwrap(),
        );
        // Walker terminates and each opposite molecule is reported exactly once.
        assert_eq!(upstream.len(), 1);
        assert_eq!(downstream.len(), 1);
        assert_eq!(upstream[0], b_id);
        assert_eq!(downstream[0], b_id);
    }

    #[test]
    fn test_resolve_exact_match() {
        let (_tmp, store) = make_store();
        let mol = sample("task-20260409-xact");
        store.save_molecule(&mol.id, &mol).unwrap();
        let resolved = resolve_molecule_id(&store, "task-20260409-xact").unwrap();
        assert_eq!(resolved.as_str(), "task-20260409-xact");
    }

    #[test]
    fn test_resolve_no_match_errors() {
        let (_tmp, store) = make_store();
        let err = resolve_molecule_id(&store, "task-20260409-none").unwrap_err();
        assert!(err.to_string().contains("no molecule"));
    }
}
