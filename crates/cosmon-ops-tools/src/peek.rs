// SPDX-License-Identifier: AGPL-3.0-only

//! `peek` operation tool — read-only fleet + molecule overview.
//!
//! The cosmon-domain analogue of glancing at `cs peek`: one call returns
//! the live worker roster and a histogram of molecule states, plus a slim
//! list of the alive molecules. It is the "can the pilot *see* the fleet"
//! capability that makes a read-only walking skeleton honest (delib
//! `2026-05-31` §8, Q3).
//!
//! Unlike [`crate::observe`] (one molecule) and [`crate::ensemble`]
//! (a filtered slice), `peek` is the *zoomed-out* view: it joins the
//! fleet snapshot ([`cosmon_state::StateStore::load_fleet`]) with a full
//! molecule scan ([`cosmon_state::StateStore::list_molecules`]) and
//! summarises both. There is no single `cosmon_state::ops` verb for this
//! aggregate, so the tool composes the two read-only store calls directly
//! — still no `cs` subprocess (delib §4).
//!
//! JSON-in / JSON-out per the claw-code tool contract borrowed as
//! bibliography under ADR-096 §2.6; `peek` is a cosmon glossary verb,
//! not a claw `Channel`.

use std::collections::BTreeMap;
use std::path::Path;

use cosmon_agent_harness::{ParametersSchema, Tool, ToolDeclaration, ToolError};
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};
use serde::{Deserialize, Serialize};

use crate::{io_err, parse_args, resolve_store};

/// Arguments for the `peek` tool — all optional.
#[derive(Debug, Default, Deserialize)]
pub struct PeekInput {
    /// Include terminal molecules (`completed` / `collapsed`) in the
    /// returned `molecules` list. Defaults to `false` — the overview
    /// shows the *live* fleet by default, mirroring `cs peek`'s focus on
    /// what is in motion. The `molecules_by_status` histogram always
    /// counts every molecule regardless of this flag.
    #[serde(default)]
    pub include_terminal: bool,
}

/// Slim per-worker record in the [`PeekJson`] roster.
#[derive(Debug, Serialize)]
pub struct WorkerSummary {
    /// Worker id (string form).
    pub id: String,
    /// Agent role (`pilot` / `worker` / `runtime` …), string form.
    pub role: String,
    /// Runtime-vs-cognition discriminator, string form.
    pub worker_role: String,
    /// Lifecycle status (`active` / `paused` / `stale` …), string form.
    pub status: String,
    /// Molecule the worker is currently processing, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_molecule: Option<String>,
    /// Worktree path (relative to project root), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Slim per-molecule record in the [`PeekJson`] list.
///
/// Same field set as `cs ensemble`'s entry shape so the two overview
/// tools speak one vocabulary; the full projection (coupling report,
/// ghost, energy) is one `observe :id` away.
#[derive(Debug, Serialize)]
pub struct MoleculeSummary {
    /// Molecule id.
    pub id: String,
    /// Lifecycle status string.
    pub status: String,
    /// Source formula id.
    pub formula: String,
    /// Assigned worker, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Current step index.
    pub current_step: usize,
    /// Total number of steps.
    pub total_steps: usize,
    /// Tags (sorted lexically).
    pub tags: Vec<String>,
}

impl MoleculeSummary {
    fn from_data(mol: &MoleculeData) -> Self {
        Self {
            id: mol.id.to_string(),
            status: mol.status.to_string(),
            formula: mol.formula_id.to_string(),
            worker: mol.assigned_worker.as_ref().map(ToString::to_string),
            current_step: mol.current_step,
            total_steps: mol.total_steps,
            tags: mol.tags.iter().map(ToString::to_string).collect(),
        }
    }
}

/// Wire shape returned by the `peek` tool.
#[derive(Debug, Serialize)]
pub struct PeekJson {
    /// Live worker roster, sorted by worker id for stable output.
    pub workers: Vec<WorkerSummary>,
    /// Number of workers in the roster (duplicates `workers.len()`).
    pub total_workers: usize,
    /// Count of molecules per lifecycle status, over **all** molecules.
    /// Keyed by status string; `BTreeMap` for deterministic key order.
    pub molecules_by_status: BTreeMap<String, usize>,
    /// Slim per-molecule records — alive molecules by default, or every
    /// molecule when `include_terminal` was set.
    pub molecules: Vec<MoleculeSummary>,
    /// Total number of molecules in the store (regardless of filter).
    pub total_molecules: usize,
    /// Number of molecules actually listed in `molecules` after the
    /// alive/terminal filter.
    pub shown_molecules: usize,
}

/// `peek` — read-only fleet + molecule overview.
#[derive(Debug, Default, Clone, Copy)]
pub struct PeekTool;

impl Tool for PeekTool {
    fn name(&self) -> &'static str {
        "peek"
    }

    fn declaration(&self) -> ToolDeclaration {
        ToolDeclaration::new(
            "peek",
            "Get a zoomed-out overview of the cosmon fleet: the live worker roster \
             (id, role, status, current molecule) plus a histogram of molecule \
             counts by status and a slim list of the alive molecules. Set \
             include_terminal=true to also list completed/collapsed molecules. \
             Read-only — the wide-angle complement to `observe` (one molecule) and \
             `ensemble` (a filtered slice).",
            ParametersSchema::from_json(serde_json::json!({
                "type": "object",
                "properties": {
                    "include_terminal": {
                        "type": "boolean",
                        "description": "List terminal (completed/collapsed) molecules too. \
                            Defaults to false. The status histogram always counts all molecules."
                    }
                },
            })),
        )
    }

    fn execute(&self, arguments_json: &str, work_dir: &Path) -> Result<String, ToolError> {
        let input: PeekInput = parse_args("peek", arguments_json)?;
        let (store, _state_dir) = resolve_store(work_dir);

        let fleet = store.load_fleet().map_err(io_err)?;
        let mut workers: Vec<WorkerSummary> = fleet
            .workers
            .values()
            .map(|w| WorkerSummary {
                id: w.id.to_string(),
                role: w.role.to_string(),
                worker_role: w.worker_role.to_string(),
                status: w.status.to_string(),
                current_molecule: w.current_molecule.as_ref().map(ToString::to_string),
                repo: w.repo.clone(),
            })
            .collect();
        workers.sort_by(|a, b| a.id.cmp(&b.id));
        let total_workers = workers.len();

        let all = store
            .list_molecules(&MoleculeFilter::default())
            .map_err(io_err)?;
        let total_molecules = all.len();

        let mut molecules_by_status: BTreeMap<String, usize> = BTreeMap::new();
        for mol in &all {
            *molecules_by_status
                .entry(mol.status.to_string())
                .or_insert(0) += 1;
        }

        let molecules: Vec<MoleculeSummary> = all
            .iter()
            .filter(|m| input.include_terminal || m.status.is_alive())
            .map(MoleculeSummary::from_data)
            .collect();
        let shown_molecules = molecules.len();

        let out = PeekJson {
            workers,
            total_workers,
            molecules_by_status,
            molecules,
            total_molecules,
            shown_molecules,
        };
        serde_json::to_string(&out).map_err(io_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixture::{seed_molecule, seed_project};

    #[test]
    fn declaration_names_the_tool() {
        assert_eq!(PeekTool.name(), "peek");
        assert_eq!(PeekTool.declaration().name, "peek");
    }

    #[test]
    fn invalid_json_is_invalid_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let err = PeekTool
            .execute("{ not json", dir.path())
            .expect_err("must reject");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[test]
    fn peek_overview_counts_and_filters_alive() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule(fixture.path(), "task-20260531-aaaa", "running");
        seed_molecule(fixture.path(), "task-20260531-bbbb", "pending");
        seed_molecule(fixture.path(), "task-20260531-cccc", "completed");

        // Default: alive only (running + pending), terminal excluded.
        let raw = PeekTool
            .execute("{}", fixture.path())
            .expect("peek must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");

        assert_eq!(parsed["total_molecules"], 3);
        assert_eq!(parsed["shown_molecules"], 2);
        // Histogram counts every molecule regardless of the alive filter.
        assert_eq!(parsed["molecules_by_status"]["running"], 1);
        assert_eq!(parsed["molecules_by_status"]["pending"], 1);
        assert_eq!(parsed["molecules_by_status"]["completed"], 1);
        // Empty fleet fixture → no workers.
        assert_eq!(parsed["total_workers"], 0);
    }

    #[test]
    fn peek_include_terminal_lists_everything() {
        let fixture = tempfile::tempdir().unwrap();
        seed_molecule(fixture.path(), "task-20260531-aaaa", "running");
        seed_molecule(fixture.path(), "task-20260531-cccc", "completed");

        let raw = PeekTool
            .execute(
                &serde_json::json!({ "include_terminal": true }).to_string(),
                fixture.path(),
            )
            .expect("peek must succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["shown_molecules"], 2);
    }

    #[test]
    fn peek_empty_project_is_empty_overview() {
        let fixture = tempfile::tempdir().unwrap();
        seed_project(fixture.path());

        let raw = PeekTool
            .execute("{}", fixture.path())
            .expect("peek over an empty project must still succeed");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["total_molecules"], 0);
        assert_eq!(parsed["total_workers"], 0);
    }
}
