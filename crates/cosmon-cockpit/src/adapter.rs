// SPDX-License-Identifier: AGPL-3.0-only

//! File-backed adapter implementing [`DashboardView`] over `cosmon-filestore`.
//!
//! Reads directly from `.cosmon/state/` via [`FileStore`] + [`StateStore`],
//! projecting raw `MoleculeData` into cockpit view-model types.
//!
//! This adapter is the only place that imports `cosmon-core` types — it maps
//! domain enums (`MoleculeStatus`, `MoleculeKind`) to plain strings for the
//! cockpit DTOs.

use std::path::PathBuf;

use chrono::Utc;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeFilter, StateStore};

use crate::view::{
    CockpitError, DashboardView, EventEntry, FleetSummary, Liveness, MoleculeDetail,
    MoleculeSummary, Revision, SparkIntake,
};

/// File-backed [`DashboardView`] adapter.
///
/// Wraps a [`FileStore`] and translates state-store queries into
/// dashboard view-model types. All reads are synchronous file I/O
/// against `.cosmon/state/`. Domain enums are serialized to strings
/// at this boundary.
#[derive(Debug, Clone)]
pub struct FileCockpitView {
    store: FileStore,
    /// Path to `events.jsonl` (sibling of the state directory under `.cosmon/`).
    events_path: PathBuf,
}

impl FileCockpitView {
    /// Create a new adapter rooted at the given `.cosmon/state/` directory.
    ///
    /// The `events.jsonl` path is `<state_root>/events.jsonl` — commands
    /// like `cs evolve`, `cs complete`, `cs collapse` write events there.
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        let state_root = state_root.into();
        let events_path = state_root.join("events.jsonl");
        Self {
            store: FileStore::new(&state_root),
            events_path,
        }
    }
}

/// Map a `CosmonError` to a `CockpitError`.
#[allow(clippy::needless_pass_by_value)] // map_err passes owned errors
fn map_store_err(e: cosmon_core::error::CosmonError) -> CockpitError {
    CockpitError::Store(e.to_string())
}

/// Serialize a [`MoleculeStatus`] to its lowercase JSON representation.
fn status_to_string(s: MoleculeStatus) -> String {
    // MoleculeStatus derives Serialize with rename_all = "snake_case".
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Serialize a `MoleculeKind` to its lowercase JSON representation.
fn kind_to_string(k: cosmon_core::kind::MoleculeKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Map a core [`Event`] to a cockpit [`EventEntry`] DTO.
#[allow(clippy::too_many_lines)] // exhaustive match on every Event variant
fn event_to_entry(envelope: &cosmon_core::event::Envelope) -> EventEntry {
    use cosmon_core::event::Event;

    let (kind, summary, molecule_id, worker_id) = match &envelope.event {
        Event::WorkerSpawned { worker_id, agent } => (
            "worker_spawned",
            format!("Worker {worker_id} spawned (agent: {agent})"),
            None,
            Some(worker_id.as_str().to_owned()),
        ),
        Event::WorkerTerminated { worker_id, reason } => (
            "worker_terminated",
            format!("Worker {worker_id} terminated: {reason}"),
            None,
            Some(worker_id.as_str().to_owned()),
        ),
        Event::WorkerFrozen {
            worker_id,
            preempted_by,
        } => (
            "worker_frozen",
            if let Some(by) = preempted_by {
                format!("Worker {worker_id} frozen (preempted by {by})")
            } else {
                format!("Worker {worker_id} frozen")
            },
            None,
            Some(worker_id.as_str().to_owned()),
        ),
        Event::WorkerThawed { worker_id } => (
            "worker_thawed",
            format!("Worker {worker_id} thawed"),
            None,
            Some(worker_id.as_str().to_owned()),
        ),
        Event::WorkerPreempted {
            incumbent,
            challenger,
        } => (
            "worker_preempted",
            format!("Worker {incumbent} preempted by {challenger}"),
            None,
            Some(incumbent.as_str().to_owned()),
        ),
        Event::WorkerKilled { worker_id } => (
            "worker_killed",
            format!("Worker {worker_id} killed"),
            None,
            Some(worker_id.as_str().to_owned()),
        ),
        Event::WorkerRespawned {
            worker_id,
            restart_count,
        } => (
            "worker_respawned",
            format!("Worker {worker_id} respawned (#{restart_count})"),
            None,
            Some(worker_id.as_str().to_owned()),
        ),
        Event::MoleculeDispatched {
            molecule_id,
            worker_id,
        } => (
            "molecule_dispatched",
            format!("{molecule_id} dispatched to {worker_id}"),
            Some(molecule_id.as_str().to_owned()),
            Some(worker_id.as_str().to_owned()),
        ),
        Event::MoleculeTransitioned {
            molecule_id,
            from,
            to,
        } => (
            "molecule_transitioned",
            format!("{molecule_id}: {from} → {to}"),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::MoleculeEvolved {
            molecule_id,
            step,
            total,
        } => (
            "molecule_evolved",
            format!("{molecule_id} evolved step {}/{total}", step + 1),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::MoleculeCompleted {
            molecule_id,
            reason,
        } => (
            "molecule_completed",
            format!("{molecule_id} completed: {reason}"),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::MoleculeCollapsed {
            molecule_id,
            reason,
        } => (
            "molecule_collapsed",
            format!("{molecule_id} collapsed: {reason}"),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::MoleculeFrozen { molecule_id } => (
            "molecule_frozen",
            format!("{molecule_id} frozen"),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::MoleculeThawed { molecule_id } => (
            "molecule_thawed",
            format!("{molecule_id} thawed"),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::MoleculeDecayed {
            molecule_id,
            products,
            reason,
        } => (
            "molecule_decayed",
            format!(
                "{molecule_id} decayed into {} products: {reason}",
                products.len()
            ),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::MoleculeMerged {
            sources,
            product,
            reason,
        } => (
            "molecule_merged",
            format!(
                "{} molecules merged into {product}: {reason}",
                sources.len()
            ),
            Some(product.as_str().to_owned()),
            None,
        ),
        Event::MoleculeTransformed {
            molecule_id,
            from_kind,
            to_kind,
            reason,
        } => (
            "molecule_transformed",
            format!("{molecule_id} transformed {from_kind} → {to_kind}: {reason}"),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::StepCompleted {
            molecule_id,
            step,
            total,
        } => (
            "step_completed",
            format!("{molecule_id} step {}/{total} done", step + 1),
            Some(molecule_id.as_str().to_owned()),
            None,
        ),
        Event::TaskDispatched { title, target, .. } => (
            "task_dispatched",
            format!("Task dispatched to {target}: {title}"),
            None,
            None,
        ),
        Event::IntentDeclared {
            agent_id,
            target_domain,
            mutation_type,
            ..
        } => (
            "intent_declared",
            format!("{agent_id} declares {mutation_type} on {target_domain}"),
            None,
            None,
        ),
        Event::ErrorOccurred { context, message } => (
            "error_occurred",
            format!("Error in {context}: {message}"),
            None,
            None,
        ),
        Event::ClaimEmitted {
            claim_id,
            molecule_id: mol,
            claim_type,
            ..
        } => (
            "claim_emitted",
            format!("Claim {claim_id} ({claim_type:?}) from {mol}"),
            Some(mol.to_string()),
            None,
        ),
        Event::ClaimVerified {
            claim_id,
            verifier_kind,
            verdict,
            ..
        } => (
            "claim_verified",
            format!("Claim {claim_id} via {verifier_kind}: {verdict:?}"),
            None,
            None,
        ),
    };

    EventEntry {
        timestamp: envelope.timestamp,
        kind: kind.to_owned(),
        summary,
        molecule_id,
        worker_id,
    }
}

/// Parse one JSONL line into an [`EventEntry`], handling both the typed
/// `Envelope` format (uses `"kind"` tag) and the legacy raw JSON format
/// (uses `"type"` tag, emitted by `cs nucleate` and `cs dispatch`).
///
/// Returns `None` if the line cannot be parsed in either format.
fn parse_event_line(line: &str) -> Option<EventEntry> {
    // Try typed Envelope first (newer events).
    if let Ok(envelope) = serde_json::from_str::<cosmon_core::event::Envelope>(line) {
        return Some(event_to_entry(&envelope));
    }

    // Fall back to legacy raw JSON with "type" tag.
    let raw: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = raw.get("type")?.as_str()?.to_owned();
    let timestamp = raw
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))?;
    let molecule_id = raw
        .get("molecule_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let worker_id = raw
        .get("worker_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let summary = match kind.as_str() {
        "molecule_nucleated" => {
            let mol = molecule_id.as_deref().unwrap_or("?");
            let formula = raw
                .get("formula_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("{mol} nucleated ({formula})")
        }
        "molecule_dispatched" => {
            let mol = molecule_id.as_deref().unwrap_or("?");
            let worker = raw.get("worker_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{mol} dispatched to {worker}")
        }
        other => {
            let mol = molecule_id.as_deref().unwrap_or("");
            if mol.is_empty() {
                other.replace('_', " ")
            } else {
                format!("{mol}: {}", other.replace('_', " "))
            }
        }
    };

    Some(EventEntry {
        timestamp,
        kind,
        summary,
        molecule_id,
        worker_id,
    })
}

impl DashboardView for FileCockpitView {
    fn molecules(&self, status: Option<&str>) -> Result<Vec<MoleculeSummary>, CockpitError> {
        let status_filter = status.and_then(|s| s.parse().ok());
        let filter = MoleculeFilter {
            status: status_filter,
            ..Default::default()
        };
        let mols = self.store.list_molecules(&filter).map_err(map_store_err)?;
        Ok(mols
            .into_iter()
            .map(|m| MoleculeSummary {
                id: m.id.as_str().to_owned(),
                status: status_to_string(m.status),
                kind: m.kind.map(kind_to_string),
                formula: m.formula_id.as_str().to_owned(),
                current_step: m.current_step,
                total_steps: m.total_steps,
                worker: m.assigned_worker.map(|w| w.as_str().to_owned()),
                worker_live: None,
                liveness: Liveness::Unknown,
                updated_at: m.updated_at,
            })
            .collect())
    }

    fn molecule(&self, id: &str) -> Result<MoleculeDetail, CockpitError> {
        let mol_id = MoleculeId::new(id).map_err(|e| CockpitError::NotFound(e.to_string()))?;
        let m = self.store.load_molecule(&mol_id).map_err(|e| {
            if matches!(e, cosmon_core::error::CosmonError::MoleculeNotFound(_)) {
                CockpitError::NotFound(id.to_owned())
            } else {
                CockpitError::Store(e.to_string())
            }
        })?;
        Ok(MoleculeDetail {
            id: m.id.as_str().to_owned(),
            fleet_id: m.fleet_id.as_str().to_owned(),
            status: status_to_string(m.status),
            kind: m.kind.map(kind_to_string),
            formula: m.formula_id.as_str().to_owned(),
            current_step: m.current_step,
            total_steps: m.total_steps,
            worker: m.assigned_worker.map(|w| w.as_str().to_owned()),
            variables: m.variables,
            links: m.links,
            completed_steps: m
                .completed_steps
                .into_iter()
                .map(|s| s.as_str().to_owned())
                .collect(),
            collapse_reason: m.collapse_reason,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }

    fn fleet(&self) -> Result<FleetSummary, CockpitError> {
        let fleet = self.store.load_fleet().map_err(map_store_err)?;
        Ok(FleetSummary {
            worker_count: fleet.workers.len(),
            repo_count: fleet.repos.len(),
            attention_budget: fleet.attention_budget,
        })
    }

    fn links(&self, id: &str) -> Result<Vec<String>, CockpitError> {
        let detail = self.molecule(id)?;
        Ok(detail.links)
    }

    fn revision(&self) -> Result<Revision, CockpitError> {
        let mols = self
            .store
            .list_molecules(&MoleculeFilter::default())
            .map_err(map_store_err)?;
        let latest = mols
            .iter()
            .map(|m| m.updated_at)
            .max()
            .unwrap_or_else(Utc::now);
        Ok(Revision {
            timestamp: latest,
            molecule_count: mols.len(),
        })
    }

    fn events_tail(&self, limit: usize) -> Result<Vec<EventEntry>, CockpitError> {
        if !self.events_path.exists() {
            return Ok(Vec::new());
        }
        // Read all events, then take the last N in reverse order.
        // For a large log this could be optimized with tail-seek, but
        // for the cockpit's small polling window this is fine.
        let content = std::fs::read_to_string(&self.events_path)
            .map_err(|e| CockpitError::Store(e.to_string()))?;

        let mut entries: Vec<EventEntry> = Vec::new();
        for line in content.lines().rev() {
            if entries.len() >= limit {
                break;
            }
            if line.is_empty() {
                continue;
            }
            if let Some(entry) = parse_event_line(line) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

impl SparkIntake for FileCockpitView {
    fn ingest(&self, _payload: &serde_json::Value) -> Result<(), CockpitError> {
        // Placeholder — spark persistence will be implemented when claudion sparks land.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::compute_liveness;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_state::{MoleculeData, StateStore};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_view() -> (TempDir, FileCockpitView) {
        let tmp = TempDir::new().unwrap();
        let view = FileCockpitView::new(tmp.path());
        (tmp, view)
    }

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
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn test_molecules_returns_all() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());
        let m1 = sample_mol("aaa1", MoleculeStatus::Running);
        let m2 = sample_mol("bbb2", MoleculeStatus::Completed);
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();

        let result = view.molecules(None).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_molecules_filter_by_status() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());
        let m1 = sample_mol("ccc3", MoleculeStatus::Running);
        let m2 = sample_mol("ddd4", MoleculeStatus::Completed);
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();

        let result = view.molecules(Some("running")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "task-20260410-ccc3");
    }

    #[test]
    fn test_molecule_detail() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());
        let m = sample_mol("eee5", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();

        let detail = view.molecule("task-20260410-eee5").unwrap();
        assert_eq!(detail.id, "task-20260410-eee5");
        assert_eq!(detail.formula, "task-work");
        assert_eq!(detail.worker.as_deref(), Some("w-test"));
    }

    #[test]
    fn test_molecule_not_found() {
        let (_tmp, view) = make_view();
        let err = view.molecule("task-20260410-nope").unwrap_err();
        assert!(matches!(err, CockpitError::NotFound(_)));
    }

    #[test]
    fn test_fleet_summary() {
        let (_tmp, view) = make_view();
        let summary = view.fleet().unwrap();
        assert_eq!(summary.worker_count, 0);
        assert_eq!(summary.repo_count, 0);
    }

    #[test]
    fn test_revision() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());
        let m = sample_mol("fff6", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();

        let rev = view.revision().unwrap();
        assert_eq!(rev.molecule_count, 1);
    }

    #[test]
    fn test_spark_intake_placeholder() {
        let (_tmp, view) = make_view();
        let result = view.ingest(&serde_json::json!({"type": "heartbeat"}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_molecules_default_liveness_unknown() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());
        let m = sample_mol("ggg7", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();

        let result = view.molecules(None).unwrap();
        assert_eq!(result[0].liveness, Liveness::Unknown);
        assert!(result[0].worker_live.is_none());
    }

    /// Golden fixture: one zombie molecule (state=running, worker=idle).
    ///
    /// Verifies that `compute_liveness` correctly detects the zombie when
    /// the molecule status says Running but the worker's live field says idle.
    #[test]
    fn test_zombie_detection_golden_fixture() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());

        // Create a running molecule assigned to a worker.
        let m = sample_mol("zombie1", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();

        let mut mols = view.molecules(None).unwrap();
        assert_eq!(mols.len(), 1);

        // Simulate worker liveness enrichment — worker says "idle".
        let worker_live = "idle";
        mols[0].worker_live = Some(worker_live.to_owned());
        mols[0].liveness = compute_liveness(&mols[0].status, Some(worker_live));

        // The molecule should be flagged as zombie (amber, not green).
        assert_eq!(mols[0].liveness, Liveness::Zombie);
    }

    #[test]
    fn test_healthy_molecule() {
        let liveness = compute_liveness("running", Some("working:fixing bug"));
        assert_eq!(liveness, Liveness::Healthy);
    }

    #[test]
    fn test_zombie_dead_worker() {
        let liveness = compute_liveness("running", Some("dead"));
        assert_eq!(liveness, Liveness::Zombie);
    }

    #[test]
    fn test_zombie_stale_worker() {
        let liveness = compute_liveness("running", Some("stale"));
        assert_eq!(liveness, Liveness::Zombie);
    }

    #[test]
    fn test_zombie_error_worker() {
        let liveness = compute_liveness("running", Some("error:restart limit"));
        assert_eq!(liveness, Liveness::Zombie);
    }

    #[test]
    fn test_zombie_dash_worker() {
        let liveness = compute_liveness("running", Some("-"));
        assert_eq!(liveness, Liveness::Zombie);
    }

    #[test]
    fn test_mismatch_waiting_worker() {
        let liveness = compute_liveness("running", Some("waiting"));
        assert_eq!(liveness, Liveness::Mismatch);
    }

    #[test]
    fn test_nonrunning_always_unknown() {
        assert_eq!(
            compute_liveness("pending", Some("working")),
            Liveness::Unknown
        );
        assert_eq!(
            compute_liveness("completed", Some("dead")),
            Liveness::Unknown
        );
    }

    #[test]
    fn test_status_serialized_as_string() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());
        let m = sample_mol("str1", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();

        let result = view.molecules(None).unwrap();
        assert_eq!(result[0].status, "running");
    }

    #[test]
    fn test_kind_serialized_as_string() {
        let (tmp, view) = make_view();
        let store = FileStore::new(tmp.path());
        let m = sample_mol("knd1", MoleculeStatus::Pending);
        store.save_molecule(&m.id, &m).unwrap();

        let detail = view.molecule("task-20260410-knd1").unwrap();
        assert_eq!(detail.kind.as_deref(), Some("task"));
    }

    // ── events_tail tests ──────────────────────────────────────────

    #[test]
    fn test_events_tail_empty_when_no_file() {
        let (_tmp, view) = make_view();
        let events = view.events_tail(5).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_events_tail_typed_envelope_format() {
        let (tmp, view) = make_view();
        let events_path = tmp.path().join("events.jsonl");

        // Write typed Envelope events (newer format with "kind").
        let e1 = cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeEvolved {
            molecule_id: MoleculeId::new("task-20260410-aaaa").unwrap(),
            step: 0,
            total: 2,
        });
        let e2 = cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeCompleted {
            molecule_id: MoleculeId::new("task-20260410-aaaa").unwrap(),
            reason: "done".to_owned(),
        });
        cosmon_filestore::event::append(&events_path, &e1).unwrap();
        cosmon_filestore::event::append(&events_path, &e2).unwrap();

        let entries = view.events_tail(5).unwrap();
        assert_eq!(entries.len(), 2);
        // Reverse chronological: completed first, evolved second.
        assert_eq!(entries[0].kind, "molecule_completed");
        assert_eq!(entries[1].kind, "molecule_evolved");
        assert_eq!(
            entries[0].molecule_id.as_deref(),
            Some("task-20260410-aaaa")
        );
        assert!(entries[0].summary.contains("completed"));
    }

    #[test]
    fn test_events_tail_legacy_type_format() {
        let (tmp, view) = make_view();
        let events_path = tmp.path().join("events.jsonl");

        // Write legacy format (raw JSON with "type" tag).
        let legacy = serde_json::json!({
            "type": "molecule_nucleated",
            "molecule_id": "task-20260410-bbbb",
            "formula_id": "task-work",
            "assigned_worker": null,
            "timestamp": "2026-04-10T10:00:00Z",
        });
        let mut line = serde_json::to_string(&legacy).unwrap();
        line.push('\n');
        std::fs::write(&events_path, &line).unwrap();

        let entries = view.events_tail(5).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "molecule_nucleated");
        assert_eq!(
            entries[0].molecule_id.as_deref(),
            Some("task-20260410-bbbb")
        );
        assert!(entries[0].summary.contains("nucleated"));
    }

    #[test]
    fn test_events_tail_mixed_formats() {
        let (tmp, view) = make_view();
        let events_path = tmp.path().join("events.jsonl");

        // Legacy nucleation event.
        let legacy = serde_json::json!({
            "type": "molecule_nucleated",
            "molecule_id": "task-20260410-cccc",
            "formula_id": "task-work",
            "assigned_worker": null,
            "timestamp": "2026-04-10T10:00:00Z",
        });
        let mut content = serde_json::to_string(&legacy).unwrap();
        content.push('\n');

        // Typed evolved event.
        let e = cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeEvolved {
            molecule_id: MoleculeId::new("task-20260410-cccc").unwrap(),
            step: 0,
            total: 2,
        });
        content.push_str(&serde_json::to_string(&e).unwrap());
        content.push('\n');

        std::fs::write(&events_path, &content).unwrap();

        let entries = view.events_tail(5).unwrap();
        assert_eq!(entries.len(), 2);
        // Reverse order: evolved first, nucleated second.
        assert_eq!(entries[0].kind, "molecule_evolved");
        assert_eq!(entries[1].kind, "molecule_nucleated");
    }

    #[test]
    fn test_events_tail_respects_limit() {
        let (tmp, view) = make_view();
        let events_path = tmp.path().join("events.jsonl");

        for i in 0..10 {
            let e = cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeEvolved {
                molecule_id: MoleculeId::new(format!("task-20260410-{i:04}")).unwrap(),
                step: 0,
                total: 2,
            });
            cosmon_filestore::event::append(&events_path, &e).unwrap();
        }

        let entries = view.events_tail(3).unwrap();
        assert_eq!(entries.len(), 3);
        // Should be the last 3 (indices 9, 8, 7).
        assert!(entries[0].molecule_id.as_deref().unwrap().ends_with("0009"));
        assert!(entries[1].molecule_id.as_deref().unwrap().ends_with("0008"));
        assert!(entries[2].molecule_id.as_deref().unwrap().ends_with("0007"));
    }

    #[test]
    fn test_events_tail_skips_bad_lines() {
        let (tmp, view) = make_view();
        let events_path = tmp.path().join("events.jsonl");

        let e = cosmon_core::event::Envelope::now(cosmon_core::event::Event::MoleculeCompleted {
            molecule_id: MoleculeId::new("task-20260410-dddd").unwrap(),
            reason: "ok".to_owned(),
        });
        let mut content = String::from("this is not valid json\n");
        content.push_str(&serde_json::to_string(&e).unwrap());
        content.push('\n');
        content.push_str("{\"invalid\": true}\n");

        std::fs::write(&events_path, &content).unwrap();

        // Only the valid Envelope line should parse.
        let entries = view.events_tail(5).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "molecule_completed");
    }
}
