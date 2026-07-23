// SPDX-License-Identifier: AGPL-3.0-only

//! `cs verify-graph` — Tarjan SCC check on the subgraph induced by a
//! typed [`MoleculeLink`] relation.
//!
//! Substrate primitive for layer-C of the *organization-twin* programme:
//! the `meta_level` cap that prevents overseer-of-overseer is a special
//! case of *no cycle in
//! the transitive closure of any typed relation between molecules*.
//! Before adding new relations (`oversee`, `attends`, `mission-controls`,
//! …) we need a generic primitive that decides that property.
//!
//! ## What it does
//!
//! For each requested [`RelationKind`] (or all of them with `--all`):
//!
//! 1. Walk every molecule in the store.
//! 2. Project each `typed_links` entry through
//!    [`MoleculeLink::canonical_edges`] to obtain forward edges in the
//!    relation's subgraph.
//! 3. Run Tarjan SCC on that subgraph.
//! 4. Emit a `RelationReport` listing the non-trivial SCCs (cycles).
//!
//! ## Exit code
//!
//! - `0` — every DAG-required relation passed (all SCCs trivial).
//! - `1` — at least one DAG-required relation contained a non-trivial
//!   SCC. Cycles in non-DAG-required relations (e.g. `Refines`) are
//!   reported but do not flip the exit code.
//!
//! ## Read-only
//!
//! This command does not mutate any state — it inspects `typed_links`
//! and prints a report. Safe to run from any worktree, in parallel with
//! other agents, in CI, etc.

use cosmon_core::interaction::{MoleculeLink, RelationKind};
use cosmon_state::MoleculeFilter;

use super::Context;

/// Arguments for `cs verify-graph`.
#[derive(clap::Args)]
pub struct Args {
    /// Single relation to check (e.g. `blocks`, `decay-product`,
    /// `merged-from`, `refines`, `refutes`). Mutually exclusive with `--all`.
    #[arg(long, value_name = "KIND", conflicts_with = "all")]
    pub relation: Option<String>,

    /// Check every registered relation kind in turn.
    #[arg(long)]
    pub all: bool,
}

/// One row of the verify-graph report — one relation across the entire
/// fleet's molecule store.
#[derive(Debug, Clone)]
struct RelationReport {
    kind: RelationKind,
    edges: usize,
    vertices: usize,
    cycles: Vec<Vec<String>>,
    /// `true` when the relation MUST be a DAG. A non-empty `cycles`
    /// list flips the global exit code only when this is `true`.
    dag_required: bool,
}

impl RelationReport {
    fn passed(&self) -> bool {
        self.cycles.is_empty()
    }

    fn fails_global(&self) -> bool {
        self.dag_required && !self.cycles.is_empty()
    }
}

/// Execute `cs verify-graph`.
///
/// # Errors
///
/// Returns an error if neither `--relation` nor `--all` is supplied,
/// if `--relation` is unknown, or if the state store is unreachable.
/// A successful invocation may still exit with status 1 — see module
/// docs.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let kinds: Vec<RelationKind> = match (args.all, args.relation.as_deref()) {
        (true, _) => RelationKind::all().to_vec(),
        (false, Some(s)) => vec![s
            .parse()
            .map_err(|e: cosmon_core::interaction::UnknownRelationKind| anyhow::anyhow!("{e}"))?],
        (false, None) => anyhow::bail!(
            "verify-graph: pass --relation <KIND> or --all (known kinds: {})",
            RelationKind::all()
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);
    let molecules = store.list_molecules(&MoleculeFilter::default())?;

    let reports: Vec<RelationReport> = kinds.iter().map(|k| build_report(*k, &molecules)).collect();

    if ctx.json {
        emit_json(&reports);
    } else {
        emit_human(&reports);
    }

    let any_fail = reports.iter().any(RelationReport::fails_global);
    if any_fail {
        std::process::exit(1);
    }
    Ok(())
}

/// Build the report for one relation by projecting every molecule's
/// `typed_links` onto canonical edges and running Tarjan SCC.
fn build_report(kind: RelationKind, molecules: &[cosmon_state::MoleculeData]) -> RelationReport {
    use std::collections::BTreeSet;

    let mut vertices: BTreeSet<String> = BTreeSet::new();
    let mut edge_set: BTreeSet<(String, String)> = BTreeSet::new();

    for mol in molecules {
        let self_id = mol.id.clone();
        for link in &mol.typed_links {
            for (rk, src, tgt) in MoleculeLink::canonical_edges(link, &self_id) {
                if rk != kind {
                    continue;
                }
                let s = src.as_str().to_owned();
                let t = tgt.as_str().to_owned();
                vertices.insert(s.clone());
                vertices.insert(t.clone());
                edge_set.insert((s, t));
            }
        }
    }

    let vertex_vec: Vec<String> = vertices.into_iter().collect();
    let edge_vec: Vec<(String, String)> = edge_set.iter().cloned().collect();
    let cycles = cosmon_graph::scc::non_trivial_sccs(&vertex_vec, &edge_vec);

    RelationReport {
        kind,
        edges: edge_vec.len(),
        vertices: vertex_vec.len(),
        cycles,
        dag_required: kind.is_dag_required(),
    }
}

fn emit_human(reports: &[RelationReport]) {
    for r in reports {
        let label = match (r.passed(), r.dag_required) {
            (true, _) => "PASS",
            (false, true) => "FAIL",
            (false, false) => "WARN",
        };
        println!(
            "[{label}] relation={:<14} vertices={} edges={} cycles={}",
            r.kind.as_str(),
            r.vertices,
            r.edges,
            r.cycles.len()
        );
        if !r.cycles.is_empty() {
            for (i, cycle) in r.cycles.iter().enumerate() {
                println!("       cycle {}: {}", i + 1, cycle.join(" → "));
            }
            if !r.dag_required {
                println!(
                    "       (relation `{}` is not DAG-required — cycles permitted)",
                    r.kind.as_str()
                );
            }
        }
    }

    let any_fail = reports.iter().any(RelationReport::fails_global);
    println!();
    if any_fail {
        println!("verify-graph: FAIL — DAG-required relation contains a cycle");
    } else {
        println!("verify-graph: PASS");
    }
}

fn emit_json(reports: &[RelationReport]) {
    for r in reports {
        let row = serde_json::json!({
            "relation": r.kind.as_str(),
            "vertices": r.vertices,
            "edges": r.edges,
            "cycles": r.cycles,
            "dag_required": r.dag_required,
            "status": if r.passed() {
                "pass"
            } else if r.dag_required {
                "fail"
            } else {
                "warn"
            },
        });
        println!("{row}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::interaction::MoleculeLink;
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::molecule_class::MoleculeClass;
    use cosmon_state::MoleculeData;
    use std::collections::{BTreeSet, HashMap};

    fn fixture_mol(id: &str, links: Vec<MoleculeLink>) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
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
            class: MoleculeClass::default(),
            typed_links: links,
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
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
            adapter: None,
        }
    }

    #[test]
    fn dag_blocks_passes_on_clean_chain() {
        // a → b → c (no cycle).
        let a = "task-20260509-aaaa";
        let b = "task-20260509-bbbb";
        let c = "task-20260509-cccc";
        let mols = vec![
            fixture_mol(
                a,
                vec![MoleculeLink::Blocks {
                    target: MoleculeId::new(b).unwrap(),
                }],
            ),
            fixture_mol(
                b,
                vec![
                    MoleculeLink::BlockedBy {
                        source: MoleculeId::new(a).unwrap(),
                    },
                    MoleculeLink::Blocks {
                        target: MoleculeId::new(c).unwrap(),
                    },
                ],
            ),
            fixture_mol(
                c,
                vec![MoleculeLink::BlockedBy {
                    source: MoleculeId::new(b).unwrap(),
                }],
            ),
        ];
        let report = build_report(RelationKind::Blocks, &mols);
        assert!(report.passed(), "expected PASS, got {report:?}");
        // 3 unique edges (Blocks/BlockedBy collapse to the same canonical edges).
        assert_eq!(report.edges, 2);
        assert_eq!(report.vertices, 3);
    }

    #[test]
    fn adversarial_three_node_cycle_is_caught() {
        // a → b → c → a — the "known cycle of size 3" fixture from the
        // briefing's adversarial test case.
        let a = "task-20260509-1111";
        let b = "task-20260509-2222";
        let c = "task-20260509-3333";
        let mols = vec![
            fixture_mol(
                a,
                vec![MoleculeLink::Blocks {
                    target: MoleculeId::new(b).unwrap(),
                }],
            ),
            fixture_mol(
                b,
                vec![MoleculeLink::Blocks {
                    target: MoleculeId::new(c).unwrap(),
                }],
            ),
            fixture_mol(
                c,
                vec![MoleculeLink::Blocks {
                    target: MoleculeId::new(a).unwrap(),
                }],
            ),
        ];
        let report = build_report(RelationKind::Blocks, &mols);
        assert!(!report.passed(), "expected cycle detection");
        assert!(report.fails_global(), "Blocks is DAG-required");
        assert_eq!(report.cycles.len(), 1);
        let cycle = &report.cycles[0];
        assert_eq!(cycle.len(), 3);
        // Each node is in the cycle.
        for id in [a, b, c] {
            assert!(cycle.iter().any(|v| v == id), "missing {id} in cycle");
        }
    }

    #[test]
    fn refines_cycle_reported_but_does_not_fail_globally() {
        // Constellation A refines B; constellation B refines A — a
        // legitimate citation cycle. Reported as WARN, not FAIL.
        let a = "task-20260509-aaaa";
        let b = "task-20260509-bbbb";
        let mols = vec![
            fixture_mol(
                a,
                vec![MoleculeLink::Refines {
                    target: MoleculeId::new(b).unwrap(),
                }],
            ),
            fixture_mol(
                b,
                vec![MoleculeLink::Refines {
                    target: MoleculeId::new(a).unwrap(),
                }],
            ),
        ];
        let report = build_report(RelationKind::Refines, &mols);
        assert!(!report.passed(), "cycle is detected");
        assert!(
            !report.fails_global(),
            "Refines is not DAG-required — should not flip exit code"
        );
        assert_eq!(report.cycles.len(), 1);
    }

    #[test]
    fn empty_store_passes_all_relations() {
        for kind in RelationKind::all() {
            let report = build_report(*kind, &[]);
            assert!(report.passed(), "empty store: {kind:?} expected PASS");
            assert_eq!(report.edges, 0);
            assert_eq!(report.vertices, 0);
        }
    }

    #[test]
    fn unrelated_relations_are_isolated() {
        // A `Refines` cycle does NOT show up as a `Blocks` failure —
        // the subgraph induced by relation R must include only edges
        // of kind R.
        let a = "task-20260509-aaaa";
        let b = "task-20260509-bbbb";
        let mols = vec![
            fixture_mol(
                a,
                vec![MoleculeLink::Refines {
                    target: MoleculeId::new(b).unwrap(),
                }],
            ),
            fixture_mol(
                b,
                vec![MoleculeLink::Refines {
                    target: MoleculeId::new(a).unwrap(),
                }],
            ),
        ];
        let blocks = build_report(RelationKind::Blocks, &mols);
        assert!(blocks.passed());
        assert_eq!(blocks.edges, 0);
    }

    #[test]
    fn self_loop_in_blocks_is_caught() {
        // A molecule that blocks itself is also a livelock SCC.
        let a = "task-20260509-self";
        let mol = fixture_mol(
            a,
            vec![MoleculeLink::Blocks {
                target: MoleculeId::new(a).unwrap(),
            }],
        );
        let report = build_report(RelationKind::Blocks, &[mol]);
        assert!(!report.passed());
        assert!(report.fails_global());
        assert_eq!(report.cycles.len(), 1);
        assert_eq!(report.cycles[0], vec![a.to_owned()]);
    }
}
