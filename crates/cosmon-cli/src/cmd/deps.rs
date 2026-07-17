// SPDX-License-Identifier: AGPL-3.0-only

//! `cs deps` — show molecule blocking dependencies.
//!
//! Read-only dependency walker for blocking links (`Blocks` and `BlockedBy`)
//! and for citation links (`Refines` and `RefinedBy`, used mainly by
//! [`Constellation`](cosmon_core::kind::MoleculeKind::Constellation)
//! molecules — the fil-rouge artifacts). Shows upstream (what blocks or
//! refines this molecule) and downstream (what this molecule blocks or
//! refines) relationships. Supports transitive traversal via BFS for
//! walking the full closure.
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

use std::collections::{HashSet, VecDeque};

use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::{CrossGalaxyRef, MoleculeLink};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};

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
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let target_id = resolve_molecule_id(store.as_ref(), &args.molecule)?;
    let target = store.load_molecule(&target_id)?;

    let (upstream, downstream) = if args.transitive {
        collect_transitive(store.as_ref(), &target_id)?
    } else {
        (
            direct_neighbors(&target, Direction::Upstream),
            direct_neighbors(&target, Direction::Downstream),
        )
    };

    // Cross-galaxy edges are listed but not traversed transitively
    // (Phase 1 of ADR-035 — transitive cross-galaxy walking requires
    // remote state access on every hop, which is reserved for the
    // resident-runtime DagPolicy work). We surface only the direct
    // refs declared on the target molecule itself.
    let cross_upstream = direct_cross_galaxy_neighbors(&target, Direction::Upstream);
    let cross_downstream = direct_cross_galaxy_neighbors(&target, Direction::Downstream);

    if ctx.json {
        emit_json(
            &target,
            &upstream,
            &downstream,
            &cross_upstream,
            &cross_downstream,
            args.transitive,
        );
    } else {
        emit_human(
            store.as_ref(),
            &target,
            &upstream,
            &downstream,
            &cross_upstream,
            &cross_downstream,
            args.transitive,
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Molecule resolution (exact, prefix)
// ---------------------------------------------------------------------------

/// Resolve a molecule ID from an exact match or a prefix match.
///
/// Exact match wins. Prefix match is accepted only if unambiguous.
fn resolve_molecule_id(store: &dyn StateStore, query: &str) -> anyhow::Result<MoleculeId> {
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
        0 => Err(anyhow::anyhow!("no molecule matching `{query}`")),
        1 => Ok(matches[0].id.clone()),
        n => Err(anyhow::anyhow!(
            "ambiguous prefix `{query}` matches {n} molecules"
        )),
    }
}

// ---------------------------------------------------------------------------
// Transitive walk
// ---------------------------------------------------------------------------

/// BFS walk of both directions from `start_id`, returning the full
/// upstream and downstream closures (excluding `start_id` itself).
///
/// Cycle-safe: visited set prevents infinite loops. Returns molecule IDs
/// in discovery order for a stable output (though the caller may sort).
fn collect_transitive(
    store: &dyn StateStore,
    start_id: &MoleculeId,
) -> anyhow::Result<(Vec<MoleculeId>, Vec<MoleculeId>)> {
    let upstream = walk_edges(store, start_id, Direction::Upstream)?;
    let downstream = walk_edges(store, start_id, Direction::Downstream)?;
    Ok((upstream, downstream))
}

/// Which edge type to follow during the BFS walk.
#[derive(Clone, Copy)]
enum Direction {
    /// Follow `BlockedBy` + `RefinedBy` links — walk towards the roots
    /// (both the progression ancestors and the citers).
    Upstream,
    /// Follow `Blocks` + `Refines` links — walk towards the leaves (both
    /// the progression descendants and the citations).
    Downstream,
}

/// Return the direct neighbors of a molecule in the requested direction,
/// merging progression edges (`Blocks`/`BlockedBy`) and citation edges
/// (`Refines`/`RefinedBy`). Duplicates (a molecule that is both blocked
/// and cited) are de-duplicated while preserving first-seen order.
/// Pull every direct cross-galaxy reference of the requested direction
/// off the molecule. Order matches `typed_links` insertion (which is
/// the order the operator declared them on the CLI), giving a stable,
/// human-readable view.
fn direct_cross_galaxy_neighbors(mol: &MoleculeData, dir: Direction) -> Vec<CrossGalaxyRef> {
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

fn direct_neighbors(mol: &MoleculeData, dir: Direction) -> Vec<MoleculeId> {
    let mut seen: HashSet<MoleculeId> = HashSet::new();
    let mut out: Vec<MoleculeId> = Vec::new();
    let pairs: [Vec<&MoleculeId>; 2] = match dir {
        Direction::Upstream => [mol.blocked_by(), mol.refined_by()],
        Direction::Downstream => [mol.blocks(), mol.refines()],
    };
    for group in pairs {
        for id in group {
            if seen.insert(id.clone()) {
                out.push(id.clone());
            }
        }
    }
    out
}

/// BFS walk in one direction.
fn walk_edges(
    store: &dyn StateStore,
    start_id: &MoleculeId,
    dir: Direction,
) -> anyhow::Result<Vec<MoleculeId>> {
    let mut seen: HashSet<MoleculeId> = HashSet::new();
    // Include start_id in the seen set so cycles do not put the starting
    // molecule back into its own upstream/downstream list. The user does
    // not want to see "A blocks A" reflected in its deps view.
    seen.insert(start_id.clone());
    let mut queue: VecDeque<MoleculeId> = VecDeque::new();
    let mut out: Vec<MoleculeId> = Vec::new();

    // Prime the queue with the direct neighbors of the starting molecule.
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

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn emit_json(
    target: &MoleculeData,
    upstream: &[MoleculeId],
    downstream: &[MoleculeId],
    cross_upstream: &[CrossGalaxyRef],
    cross_downstream: &[CrossGalaxyRef],
    transitive: bool,
) {
    let out = serde_json::json!({
        "molecule": target.id.as_str(),
        "status": target.status.to_string(),
        "transitive": transitive,
        "upstream": upstream.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
        "downstream": downstream.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
        "cross_galaxy_upstream": cross_upstream.iter().map(cross_galaxy_json).collect::<Vec<_>>(),
        "cross_galaxy_downstream": cross_downstream.iter().map(cross_galaxy_json).collect::<Vec<_>>(),
    });
    println!("{out}");
}

/// Render a cross-galaxy reference as JSON, including its current
/// resolution status so consumers can mark unreachable galaxies in
/// the UI without a second round-trip.
fn cross_galaxy_json(cgr: &CrossGalaxyRef) -> serde_json::Value {
    let resolution = resolve_cross_galaxy_ref(cgr);
    serde_json::json!({
        "galaxy": cgr.galaxy,
        "mol_id": cgr.mol_id.as_str(),
        "ref": cgr.to_canonical_string(),
        "resolution": resolution,
    })
}

fn emit_human(
    store: &dyn StateStore,
    target: &MoleculeData,
    upstream: &[MoleculeId],
    downstream: &[MoleculeId],
    cross_upstream: &[CrossGalaxyRef],
    cross_downstream: &[CrossGalaxyRef],
    transitive: bool,
) {
    let label = if transitive { "transitive" } else { "direct" };
    println!("Deps for {} ({}) — {label}", target.id, target.status);
    println!();
    println!("  ⏳ Blocked by ({}):", upstream.len());
    if upstream.is_empty() && cross_upstream.is_empty() {
        println!("    (none — this molecule has no upstream blockers)");
    } else {
        for id in upstream {
            let status = status_hint(store, id);
            println!("    - {id}{status}");
        }
        for cgr in cross_upstream {
            println!("    - {cgr}{}", cross_galaxy_status_hint(cgr));
        }
    }
    println!();
    println!("  ⛔ Blocks ({}):", downstream.len());
    if downstream.is_empty() && cross_downstream.is_empty() {
        println!("    (none — this molecule has no downstream blocked molecules)");
    } else {
        for id in downstream {
            let status = status_hint(store, id);
            println!("    - {id}{status}");
        }
        for cgr in cross_downstream {
            println!("    - {cgr}{}", cross_galaxy_status_hint(cgr));
        }
    }
}

/// Render a short status suffix for a cross-galaxy reference. Mirrors
/// [`status_hint`] but consults the cross-galaxy resolver instead of
/// the local store.
fn cross_galaxy_status_hint(cgr: &CrossGalaxyRef) -> String {
    match resolve_cross_galaxy_ref(cgr) {
        CrossGalaxyResolution::Resolved { status, .. } => format!(" [cross-galaxy/{status}]"),
        CrossGalaxyResolution::MoleculeMissing { .. } => " [cross-galaxy/MISSING]".to_owned(),
        CrossGalaxyResolution::GalaxyUnknown => " [cross-galaxy/UNKNOWN]".to_owned(),
    }
}

/// Render a short status suffix for a molecule ID in the dependency list.
/// If the molecule can be loaded, returns `" [status]"`; otherwise a
/// " `missing`" marker to flag dangling references the user should fix.
fn status_hint(store: &dyn StateStore, id: &MoleculeId) -> String {
    match store.load_molecule(id) {
        Ok(m) => {
            let label = match m.status {
                MoleculeStatus::Completed => "completed",
                MoleculeStatus::Collapsed => "collapsed",
                MoleculeStatus::Running => "running",
                MoleculeStatus::Pending => "pending",
                MoleculeStatus::Queued => "queued",
                MoleculeStatus::Frozen => "frozen",
                MoleculeStatus::Starved => "starved",
                _ => "unknown",
            };
            format!(" [{label}]")
        }
        Err(_) => " [MISSING]".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::interaction::MoleculeLink;
    use cosmon_filestore::FileStore;
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
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
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
    fn test_transitive_walk_from_root_downstream() {
        let (_tmp, store) = make_store();
        let (a_id, b_id, c_id) = build_chain(&store);

        let (upstream, downstream) = collect_transitive(&store, &a_id).unwrap();
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

        let (upstream, downstream) = collect_transitive(&store, &c_id).unwrap();
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

        let (upstream, downstream) = collect_transitive(&store, &a_id).unwrap();
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
