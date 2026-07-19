// SPDX-License-Identifier: AGPL-3.0-only

//! Self-calibration oracle for the cockpit dashboard.
//!
//! Compares the dashboard's internal view (via [`DashboardView`]) against an
//! independent "oracle" that walks the filesystem directly, bypassing the
//! `FileStore` abstraction. Any divergence between the two signals drift —
//! a dashboard that does not verify itself is Ptolemaic.
//!
//! # Observables
//!
//! | Name | Dashboard source | Oracle source |
//! |------|-----------------|---------------|
//! | `molecule_count` | `molecules(None).len()` | walk `state.json` files |
//! | `status_counts` | group molecules by status | parse each `state.json` |
//! | `event_line_count` | (not tracked) | `wc -l events.jsonl` |
//! | `fleet_worker_count` | `fleet().worker_count` | parse `fleet.json` |

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::view::{CockpitError, DashboardView};

/// A single observable comparison between dashboard and oracle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservableCheck {
    /// Name of the observable (e.g. `molecule_count`).
    pub name: String,
    /// What the dashboard reports.
    pub dashboard: String,
    /// What the oracle (direct filesystem walk) reports.
    pub oracle: String,
    /// Whether the two agree.
    pub agrees: bool,
}

/// Result of one selfcheck cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfcheckResult {
    /// When this check was performed.
    pub checked_at: DateTime<Utc>,
    /// Whether all observables agree (the system is calibrated).
    pub calibrated: bool,
    /// Per-observable comparison results.
    pub observables: Vec<ObservableCheck>,
}

/// Oracle snapshot gathered by walking the filesystem directly.
#[derive(Debug)]
struct OracleSnapshot {
    /// Number of molecule directories containing a `state.json`.
    molecule_count: usize,
    /// Per-status molecule counts from parsing each `state.json`.
    status_counts: BTreeMap<String, usize>,
    /// Number of lines in `events.jsonl`.
    event_line_count: usize,
    /// Number of workers from `fleet.json`.
    fleet_worker_count: usize,
}

/// Run a single selfcheck cycle.
///
/// Compares the dashboard's view (via `DashboardView`) against a direct
/// filesystem walk of the same state directory. Returns the comparison
/// result with per-observable agree/disagree verdicts.
///
/// # Arguments
///
/// * `view` — the dashboard view to check (what the cockpit API would return)
/// * `state_dir` — the `.cosmon/state/` directory to walk as the oracle
/// * `workspace_root` — the repo root (parent of `.cosmon/`) for `events.jsonl`
pub fn run_selfcheck(
    view: &dyn DashboardView,
    state_dir: &Path,
    workspace_root: &Path,
) -> SelfcheckResult {
    let oracle = gather_oracle(state_dir, workspace_root);
    let mut observables = Vec::new();

    // 1. molecule_count
    let dashboard_mol_count = view.molecules(None).map_or(0, |m| m.len());
    observables.push(ObservableCheck {
        name: "molecule_count".to_owned(),
        dashboard: dashboard_mol_count.to_string(),
        oracle: oracle.molecule_count.to_string(),
        agrees: dashboard_mol_count == oracle.molecule_count,
    });

    // 2. status_counts — compare per-status
    let dashboard_status_counts = dashboard_status_counts(view);
    let all_statuses: Vec<String> = {
        let mut keys: std::collections::BTreeSet<String> =
            oracle.status_counts.keys().cloned().collect();
        keys.extend(dashboard_status_counts.keys().cloned());
        keys.into_iter().collect()
    };
    for status in &all_statuses {
        let d = dashboard_status_counts.get(status).copied().unwrap_or(0);
        let o = oracle.status_counts.get(status).copied().unwrap_or(0);
        observables.push(ObservableCheck {
            name: format!("status_{status}"),
            dashboard: d.to_string(),
            oracle: o.to_string(),
            agrees: d == o,
        });
    }

    // 3. event_line_count (oracle-only — dashboard doesn't track this)
    observables.push(ObservableCheck {
        name: "event_line_count".to_owned(),
        dashboard: "-".to_owned(),
        oracle: oracle.event_line_count.to_string(),
        agrees: true, // no dashboard counterpart, always "agrees"
    });

    // 4. fleet_worker_count
    let dashboard_worker_count = view.fleet().map_or(0, |f| f.worker_count);
    observables.push(ObservableCheck {
        name: "fleet_worker_count".to_owned(),
        dashboard: dashboard_worker_count.to_string(),
        oracle: oracle.fleet_worker_count.to_string(),
        agrees: dashboard_worker_count == oracle.fleet_worker_count,
    });

    let calibrated = observables.iter().all(|o| o.agrees);

    SelfcheckResult {
        checked_at: Utc::now(),
        calibrated,
        observables,
    }
}

/// Gather an oracle snapshot by walking the filesystem directly.
fn gather_oracle(state_dir: &Path, workspace_root: &Path) -> OracleSnapshot {
    let (molecule_count, status_counts) = walk_molecules(state_dir);
    let event_line_count = count_event_lines(workspace_root);
    let fleet_worker_count = read_fleet_worker_count(state_dir);

    OracleSnapshot {
        molecule_count,
        status_counts,
        event_line_count,
        fleet_worker_count,
    }
}

/// Walk molecule directories and count state.json files, parsing status from each.
fn walk_molecules(state_dir: &Path) -> (usize, BTreeMap<String, usize>) {
    let mut count = 0usize;
    let mut statuses = BTreeMap::new();

    // Fleet-scoped: fleets/{fleet}/molecules/{id}/state.json
    let fleets_dir = state_dir.join("fleets");
    if fleets_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
            for fleet_entry in entries.flatten() {
                let mols_dir = fleet_entry.path().join("molecules");
                scan_mol_dir(&mols_dir, &mut count, &mut statuses);
            }
        }
    }

    // Legacy: ops/molecules/{id}/state.json
    let legacy_dir = state_dir.join("ops/molecules");
    scan_mol_dir(&legacy_dir, &mut count, &mut statuses);

    (count, statuses)
}

/// Scan a single molecules directory, incrementing count and status tallies.
fn scan_mol_dir(dir: &Path, count: &mut usize, statuses: &mut BTreeMap<String, usize>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let state_path = entry.path().join("state.json");
        if !state_path.exists() {
            continue;
        }
        *count += 1;
        // Parse just the status field for lightweight comparison.
        if let Ok(content) = std::fs::read_to_string(&state_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(status) = json["status"].as_str() {
                    *statuses.entry(status.to_owned()).or_insert(0) += 1;
                }
            }
        }
    }
}

/// Count lines in `events.jsonl` (equivalent to `wc -l`).
fn count_event_lines(workspace_root: &Path) -> usize {
    let path = workspace_root.join(".cosmon/events.jsonl");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };
    content.lines().filter(|l| !l.is_empty()).count()
}

/// Read worker count directly from `fleet.json`.
fn read_fleet_worker_count(state_dir: &Path) -> usize {
    let path = state_dir.join("fleet.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return 0;
    };
    json["workers"].as_object().map_or(0, serde_json::Map::len)
}

/// Build per-status counts from the dashboard view.
fn dashboard_status_counts(view: &dyn DashboardView) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    if let Ok(mols) = view.molecules(None) {
        for m in &mols {
            let status_str = m.status.clone();
            *counts.entry(status_str).or_insert(0) += 1;
        }
    }
    counts
}

/// Check whether the dashboard and oracle agree for a golden test scenario.
///
/// This is the acceptance test entry point: creates a `SelfcheckResult` and
/// verifies the `calibrated` field matches the expected value.
///
/// # Errors
///
/// Returns [`CockpitError::Store`] if calibration status does not match
/// `expected_calibrated`.
pub fn assert_calibration(
    view: &dyn DashboardView,
    state_dir: &Path,
    workspace_root: &Path,
    expected_calibrated: bool,
) -> Result<SelfcheckResult, CockpitError> {
    let result = run_selfcheck(view, state_dir, workspace_root);
    if result.calibrated != expected_calibrated {
        return Err(CockpitError::Store(format!(
            "calibration mismatch: expected calibrated={expected_calibrated}, got calibrated={}. Observables: {:?}",
            result.calibrated,
            result.observables.iter()
                .filter(|o| !o.agrees)
                .map(|o| format!("{}: dashboard={}, oracle={}", o.name, o.dashboard, o.oracle))
                .collect::<Vec<_>>()
        )));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::FileCockpitView;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, StateStore};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn sample_mol(suffix: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(format!("task-20260410-{suffix}")).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: Some(WorkerId::new("w-test").unwrap()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: 1,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: Some(cosmon_core::kind::MoleculeKind::Task),
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
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    /// Golden fixture: dashboard and oracle agree on healthy state.
    #[test]
    fn test_selfcheck_agrees_on_clean_state() {
        let workspace = TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        let view = FileCockpitView::new(&state_dir);

        let m1 = sample_mol("sc01", MoleculeStatus::Running);
        let m2 = sample_mol("sc02", MoleculeStatus::Completed);
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();

        let result = run_selfcheck(&view, &state_dir, workspace.path());
        assert!(
            result.calibrated,
            "should be calibrated: {:?}",
            result.observables
        );
        assert_eq!(
            result
                .observables
                .iter()
                .find(|o| o.name == "molecule_count")
                .unwrap()
                .dashboard,
            "2"
        );
    }

    /// Acceptance test: agrees=false when state.json is manually tampered.
    #[test]
    fn test_selfcheck_detects_tampered_state() {
        let workspace = TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        let view = FileCockpitView::new(&state_dir);

        let m1 = sample_mol("tamp1", MoleculeStatus::Running);
        store.save_molecule(&m1.id, &m1).unwrap();

        // Verify calibrated first
        let result = run_selfcheck(&view, &state_dir, workspace.path());
        assert!(result.calibrated);

        // Now tamper: add a rogue state.json that the FileStore won't parse
        // via the DashboardView (e.g. a molecule in a non-standard location
        // that only the oracle's directory walk finds).
        let rogue_dir = state_dir.join("fleets/default/molecules/rogue-mol-xxxx");
        std::fs::create_dir_all(&rogue_dir).unwrap();
        std::fs::write(
            rogue_dir.join("state.json"),
            r#"{"id":"rogue-mol-xxxx","fleet_id":"default","formula_id":"task-work","status":"running","variables":{},"assigned_worker":null,"created_at":"2026-04-10T00:00:00Z","updated_at":"2026-04-10T00:00:00Z","total_steps":1,"current_step":0,"completed_steps":[],"collapse_reason":null,"collapsed_step":null,"links":[],"kind":"task","typed_links":[]}"#,
        ).unwrap();

        // The oracle will find 2 molecules, the dashboard will also find 2
        // (FileStore scans fleets/). But the rogue state has status "running"
        // while it's actually a manually injected file. The counts will match
        // since FileStore does pick it up. Let's instead tamper by directly
        // editing the state.json to change the status string to something
        // the dashboard's status parsing doesn't produce.
        //
        // Actually, the real drift detection is simpler: the oracle parses
        // raw JSON status strings, while the dashboard uses typed enums.
        // Let's tamper by adding a molecule dir with an invalid state.json
        // that the FileStore will skip (parse error) but the oracle line
        // counter still counts as a directory.
        let bad_dir = state_dir.join("fleets/default/molecules/bad-mol-yyyy");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(
            bad_dir.join("state.json"),
            r#"{"status":"running","this_is_not_valid_molecule_data":true}"#,
        )
        .unwrap();

        // Oracle counts 3 state.json files (m1, rogue, bad).
        // Dashboard lists only 2 (m1, rogue — bad fails MoleculeData parse).
        // But actually FileStore's scan_molecules_dir also skips parse errors...
        // Let me check. It returns an error actually. Let me make the state truly
        // unparseable by the MoleculeData deserializer but still having a "status" field.

        // The FileStore scan does `serde_json::from_str::<MoleculeData>` which will
        // fail on the bad JSON, causing the entire list_molecules to return Err.
        // That's not what we want. Let's just remove the rogue dir and instead
        // directly modify the molecule count in a way that creates divergence.

        // Simplest approach: remove a state.json AFTER the dashboard has cached it.
        // But FileCockpitView doesn't cache — it reads fresh every time.

        // The real acceptance scenario: tamper a state.json's status field to
        // a value the oracle reads as-is but the dashboard deserializes differently.
        // Actually both go through the same serde path.

        // Let me just test with a filesystem walk divergence: add a directory
        // that has a state.json the oracle counts but FileStore's Rust deserializer
        // rejects (because MoleculeData has required fields).
        // The Oracle only reads the "status" field, so it succeeds on partial JSON.
        // The Dashboard uses FileStore which does full MoleculeData deserialization.

        // Clean up the bad_dir and rogue_dir, start fresh
        std::fs::remove_dir_all(&bad_dir).unwrap();
        std::fs::remove_dir_all(&rogue_dir).unwrap();

        // Create a molecule that the oracle can count (has status field)
        // but FileStore cannot deserialize (missing required fields).
        let partial_dir = state_dir.join("fleets/default/molecules/partial-zzzz");
        std::fs::create_dir_all(&partial_dir).unwrap();
        std::fs::write(partial_dir.join("state.json"), r#"{"status":"pending"}"#).unwrap();

        // Oracle: 2 molecules (m1 + partial). Dashboard: depends on FileStore behavior.
        // FileStore::list_molecules returns Err on deserialization failure, so
        // the dashboard returns 0 molecules (error path). Oracle finds 2.
        let result = run_selfcheck(&view, &state_dir, workspace.path());

        // The dashboard should report 0 (error) or 1, oracle reports 2.
        // Either way, they disagree on molecule_count.
        let mol_check = result
            .observables
            .iter()
            .find(|o| o.name == "molecule_count")
            .unwrap();
        assert!(
            !mol_check.agrees,
            "should detect tampered state: dashboard={}, oracle={}",
            mol_check.dashboard, mol_check.oracle
        );
        assert!(!result.calibrated);
    }

    /// Empty state: both dashboard and oracle see zero molecules.
    #[test]
    fn test_selfcheck_empty_state() {
        let workspace = TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let view = FileCockpitView::new(&state_dir);

        let result = run_selfcheck(&view, &state_dir, workspace.path());
        assert!(result.calibrated);

        let mol = result
            .observables
            .iter()
            .find(|o| o.name == "molecule_count")
            .unwrap();
        assert_eq!(mol.dashboard, "0");
        assert_eq!(mol.oracle, "0");
    }

    /// Verify event line counting.
    #[test]
    fn test_selfcheck_event_count() {
        let workspace = TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let events_path = workspace.path().join(".cosmon/events.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        std::fs::write(&events_path, "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n").unwrap();

        let view = FileCockpitView::new(&state_dir);
        let result = run_selfcheck(&view, &state_dir, workspace.path());

        let evt = result
            .observables
            .iter()
            .find(|o| o.name == "event_line_count")
            .unwrap();
        assert_eq!(evt.oracle, "3");
    }

    /// `assert_calibration` helper rejects when expectation is wrong.
    #[test]
    fn test_assert_calibration_helper() {
        let workspace = TempDir::new().unwrap();
        let state_dir = workspace.path().join(".cosmon/state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let view = FileCockpitView::new(&state_dir);

        // Expect calibrated=true on empty state → should pass
        let result = assert_calibration(&view, &state_dir, workspace.path(), true);
        assert!(result.is_ok());

        // Expect calibrated=false on empty state → should fail
        let result = assert_calibration(&view, &state_dir, workspace.path(), false);
        assert!(result.is_err());
    }
}
