// SPDX-License-Identifier: AGPL-3.0-only

//! Tenant fleet vitals: lifecycle state joined with observed worker health.
//!
//! This module owns the single aggregation used by `GET /v1/vitals` and by
//! future renderers. The state store supplies durable intent, while an
//! injected [`WorkerHealthProbe`] supplies the transport observation. Keeping
//! that effect behind a port leaves the aggregation deterministic and keeps
//! transport I/O out of `cosmon-state`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};
use cosmon_core::auth::Subject;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::operator_block::awaits_operator;
use cosmon_core::staleness::{self, BacklogItem};
use serde::Serialize;

use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::{MoleculeFilter, StateStore};

/// External witness used to observe whether a recorded worker is alive.
///
/// Implementations may call tmux, a process supervisor, or an in-memory test
/// double. Probe failure is data, not a failed fleet read: callers receive
/// [`ObservedHealth::Unknown`] instead of losing every row.
pub trait WorkerHealthProbe {
    /// Observe one worker without mutating it.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic when the transport cannot determine liveness.
    /// The aggregation converts that failure to [`ObservedHealth::Unknown`].
    fn is_alive(&self, worker: &WorkerId) -> Result<bool, String>;
}

impl<F> WorkerHealthProbe for F
where
    F: Fn(&WorkerId) -> Result<bool, String>,
{
    fn is_alive(&self, worker: &WorkerId) -> Result<bool, String> {
        self(worker)
    }
}

/// Transport health observed for one non-terminal molecule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedHealth {
    /// The molecule has no process record, so there is no worker to probe.
    Unassigned,
    /// The transport confirmed that the recorded worker is alive.
    Live,
    /// A process is recorded but the transport confirmed it is no longer alive.
    Orphaned,
    /// A process is recorded but the transport could not answer.
    Unknown,
}

/// One molecule-keyed row in the tenant fleet view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VitalsRow {
    /// Molecule identity; there is exactly one row per non-terminal molecule.
    pub id: String,
    /// Persisted lifecycle status. This is intent/history, not liveness.
    pub status: String,
    /// Current formula step, zero-based as in the state store.
    pub current_step: usize,
    /// Number of formula steps.
    pub total_steps: usize,
    /// Recorded worker identity, when a process is bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Observed transport health.
    pub health: ObservedHealth,
    /// Whether the molecule declares the `temp:awaiting-op` control-plane tag.
    pub awaiting_operator: bool,
    /// Molecule creation time.
    pub created_at: DateTime<Utc>,
    /// Most recent durable molecule update.
    pub updated_at: DateTime<Utc>,
}

/// Backlog-age summary shared with `cs status` and `cs peek`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VitalsBacklog {
    /// Pending, non-lease molecules.
    pub count: usize,
    /// Count older than [`cosmon_core::staleness::stale_backlog_after`].
    pub stale: usize,
    /// Age of the oldest counted molecule in seconds.
    pub oldest_age_seconds: Option<i64>,
    /// Identity of that oldest molecule.
    pub oldest_id: Option<String>,
    /// Pending pilot-lease molecules deliberately excluded.
    pub leases_excluded: usize,
}

/// Counts derived from the same row set returned to the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VitalsCounts {
    /// Number of non-terminal molecules.
    pub molecules: usize,
    /// Rows whose worker was observed alive.
    pub live: usize,
    /// Rows whose recorded worker was observed absent.
    pub orphaned: usize,
    /// Rows whose worker probe failed.
    pub unknown: usize,
    /// Rows with no process record.
    pub unassigned: usize,
    /// Rows declaring an operator wait.
    pub awaiting_operator: usize,
}

/// Complete tenant fleet vitals projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VitalsView {
    /// One row per non-terminal molecule.
    pub molecules: Vec<VitalsRow>,
    /// Counts derived from `molecules`.
    pub counts: VitalsCounts,
    /// Shared backlog-age projection.
    pub backlog: VitalsBacklog,
    /// Instant at which clock-dependent arithmetic was evaluated.
    pub observed_at: DateTime<Utc>,
}

/// Errors returned by [`vitals`].
#[derive(Debug, thiserror::Error)]
pub enum VitalsError {
    /// The state store could not enumerate the tenant's molecules.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for VitalsError {
    fn tag(&self) -> &'static str {
        "store-unavailable"
    }

    fn http_status(&self) -> u16 {
        503
    }
}

/// Aggregate a tenant's non-terminal molecules and observed worker health.
///
/// `lease_missions` and `now` are inputs so the operation remains free of
/// filesystem and clock I/O. A failed individual liveness probe produces an
/// `unknown` row; only failure to read the molecule set fails the operation.
///
/// # Errors
///
/// Returns [`VitalsError::StoreUnavailable`] when the state store cannot list
/// molecules.
pub fn vitals(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    probe: &dyn WorkerHealthProbe,
    lease_missions: &BTreeSet<MoleculeId>,
    now: DateTime<Utc>,
) -> Result<VitalsView, VitalsError> {
    let started = Instant::now();
    let all = store
        .list_molecules(&MoleculeFilter::default())
        .map_err(|e| VitalsError::StoreUnavailable(e.to_string()))?;

    let backlog = staleness::backlog_age(
        all.iter().map(|m| BacklogItem {
            id: m.id.clone(),
            status: m.status,
            created_at: Some(m.created_at),
            is_lease: lease_missions.contains(&m.id),
        }),
        now,
    );

    let mut molecules = Vec::new();
    for molecule in all.iter().filter(|m| !m.status.is_terminal()) {
        let (worker, health) =
            molecule
                .process
                .as_ref()
                .map_or((None, ObservedHealth::Unassigned), |process| {
                    let health = match probe.is_alive(&process.worker_id) {
                        Ok(true) => ObservedHealth::Live,
                        Ok(false) => ObservedHealth::Orphaned,
                        Err(_) => ObservedHealth::Unknown,
                    };
                    (Some(process.worker_id.to_string()), health)
                });
        molecules.push(VitalsRow {
            id: molecule.id.to_string(),
            status: molecule.status.to_string(),
            current_step: molecule.current_step,
            total_steps: molecule.total_steps,
            worker,
            health,
            awaiting_operator: awaits_operator(&molecule.tags),
            created_at: molecule.created_at,
            updated_at: molecule.updated_at,
        });
    }
    molecules.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let counts = VitalsCounts {
        molecules: molecules.len(),
        live: count_health(&molecules, ObservedHealth::Live),
        orphaned: count_health(&molecules, ObservedHealth::Orphaned),
        unknown: count_health(&molecules, ObservedHealth::Unknown),
        unassigned: count_health(&molecules, ObservedHealth::Unassigned),
        awaiting_operator: molecules.iter().filter(|row| row.awaiting_operator).count(),
    };
    let view = VitalsView {
        molecules,
        counts,
        backlog: VitalsBacklog {
            count: backlog.counted,
            stale: backlog.stale,
            oldest_age_seconds: backlog.oldest.map(|age| age.num_seconds().max(0)),
            oldest_id: backlog.oldest_id.map(|id| id.to_string()),
            leases_excluded: backlog.leases_excluded,
        },
        observed_at: now,
    };

    let subject_kind = if subject.id().as_str() == "operator" {
        "operator".to_owned()
    } else {
        format!("jwt:{}", subject.id().as_str())
    };
    emit_authz_decision(
        state_dir,
        "vitals",
        &subject_kind,
        None,
        if subject.id().as_str() == "operator" {
            AuthzDecision::Allow
        } else {
            AuthzDecision::Absent
        },
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );

    Ok(view)
}

fn count_health(rows: &[VitalsRow], health: ObservedHealth) -> usize {
    rows.iter().filter(|row| row.health == health).count()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Mutex;

    use chrono::Duration;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::process::MoleculeProcess;
    use cosmon_core::tag::Tag;

    use super::*;
    use crate::{Fleet, MoleculeData};

    struct FakeStore {
        molecules: Mutex<Vec<MoleculeData>>,
    }

    impl StateStore for FakeStore {
        fn load_fleet(&self) -> Result<Fleet, cosmon_core::error::CosmonError> {
            unreachable!("vitals does not read fleet metadata")
        }

        fn save_fleet(&self, _fleet: &Fleet) -> Result<(), cosmon_core::error::CosmonError> {
            unreachable!("read-only fixture")
        }

        fn save_molecule(
            &self,
            _id: &MoleculeId,
            _data: &MoleculeData,
        ) -> Result<(), cosmon_core::error::CosmonError> {
            unreachable!("read-only fixture")
        }

        fn load_molecule(
            &self,
            _id: &MoleculeId,
        ) -> Result<MoleculeData, cosmon_core::error::CosmonError> {
            unreachable!("vitals lists once")
        }

        fn list_molecules(
            &self,
            _filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, cosmon_core::error::CosmonError> {
            Ok(self.molecules.lock().unwrap().clone())
        }
    }

    fn molecule(id: &str, status: MoleculeStatus, created_at: DateTime<Utc>) -> MoleculeData {
        MoleculeData {
            harvest_reason: None,
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at,
            updated_at: created_at,
            total_steps: 2,
            current_step: 0,
            completed_steps: vec![],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: vec![],
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![],
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
            escalations: vec![],
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            base_branch: None,
            pending_step: None,
            merged_at: None,
            non_integration: None,
            prompt_seal: None,
            briefing_seals: vec![],
            bootstrap_seals: vec![],
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
    fn one_row_per_non_terminal_molecule_with_observed_health() {
        let now = Utc::now();
        let mut pending = molecule(
            "task-20260925-aaaa",
            MoleculeStatus::Pending,
            now - Duration::hours(49),
        );
        pending
            .tags
            .insert(Tag::new(cosmon_core::operator_block::AWAITING_OP_TAG).unwrap());
        let mut live = molecule(
            "task-20260925-bbbb",
            MoleculeStatus::Running,
            now - Duration::hours(2),
        );
        live.process = Some(MoleculeProcess::new(
            WorkerId::new("live-worker").unwrap(),
            "live-worker",
        ));
        let mut orphan = molecule(
            "task-20260925-cccc",
            MoleculeStatus::Running,
            now - Duration::hours(1),
        );
        orphan.process = Some(MoleculeProcess::new(
            WorkerId::new("dead-worker").unwrap(),
            "dead-worker",
        ));
        let completed = molecule("task-20260925-dddd", MoleculeStatus::Completed, now);
        let store = FakeStore {
            molecules: Mutex::new(vec![completed, orphan, live, pending]),
        };

        let view = vitals(
            &store,
            tempfile::tempdir().unwrap().path(),
            &Subject::operator(),
            &|worker: &WorkerId| Ok(worker.as_str() == "live-worker"),
            &BTreeSet::new(),
            now,
        )
        .unwrap();

        assert_eq!(view.molecules.len(), 3, "terminal rows are excluded");
        assert_eq!(view.counts.live, 1);
        assert_eq!(view.counts.orphaned, 1);
        assert_eq!(view.counts.unassigned, 1);
        assert_eq!(view.counts.awaiting_operator, 1);
        assert_eq!(view.backlog.count, 1);
        assert_eq!(view.backlog.stale, 1);
        assert_eq!(
            view.backlog.oldest_id.as_deref(),
            Some("task-20260925-aaaa")
        );
    }

    #[test]
    fn probe_failure_is_unknown_without_losing_the_row() {
        let now = Utc::now();
        let mut row = molecule("task-20260925-eeee", MoleculeStatus::Running, now);
        row.process = Some(MoleculeProcess::new(
            WorkerId::new("uncertain-worker").unwrap(),
            "uncertain-worker",
        ));
        let store = FakeStore {
            molecules: Mutex::new(vec![row]),
        };

        let view = vitals(
            &store,
            tempfile::tempdir().unwrap().path(),
            &Subject::operator(),
            &|_: &WorkerId| Err("transport unavailable".to_owned()),
            &BTreeSet::new(),
            now,
        )
        .unwrap();

        assert_eq!(view.molecules[0].health, ObservedHealth::Unknown);
        assert_eq!(view.counts.unknown, 1);
    }
}
