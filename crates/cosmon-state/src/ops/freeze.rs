// SPDX-License-Identifier: AGPL-3.0-only

//! `freeze` — `Running → Frozen` molecule-level transition.
//!
//! Mutation verb extracted under `cosmon_state::ops`. Symmetric pair
//! with [`super::thaw`](fn@super::thaw).
//!
//! `freeze` operates on the **molecule** lifecycle (status transition),
//! not on a worker session. The cs-cli `cs freeze <worker>` subcommand
//! preempts a worker (Slurm-style); this verb is the thinner cut of
//! "the molecule is suspended" used by remote pilots over the V1 RPP
//! API.
//!
//! Idempotent: freezing an already-frozen molecule is a no-op (no
//! error). Rejects terminal states (`Completed` / `Collapsed`) — those
//! are not legal sources for a Running transition.

use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use cosmon_core::auth::Subject;
use cosmon_core::error::CosmonError;
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use serde::Serialize;

use crate::event_log;
use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::StateStore;

/// Errors returned by [`freeze`].
#[derive(Debug, thiserror::Error)]
pub enum FreezeError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// Molecule is in a terminal state (`Completed` / `Collapsed`).
    #[error("molecule {0} is in terminal status `{1}` — cannot freeze")]
    TerminalStatus(MoleculeId, MoleculeStatus),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl FreezeError {
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for FreezeError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::TerminalStatus(_, _) => "terminal-status",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::TerminalStatus(_, _) => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Inputs for [`freeze`].
#[derive(Debug, Clone, Default)]
pub struct FreezeRequest {
    /// Optional reason recorded in the molecule log; the lib path itself
    /// does not persist a free-form `freeze_reason` on the `MoleculeData`
    /// (that field is reserved for `cs stuck`'s `StuckReason` event), but
    /// the value flows into `MoleculeFrozen` for traceability.
    pub reason: Option<String>,
}

/// Result of a successful [`freeze`] call.
#[derive(Debug, Clone)]
pub struct FreezeView {
    /// Molecule that was frozen (or already was).
    pub id: MoleculeId,
    /// Status the molecule held before this call.
    pub previous_status: MoleculeStatus,
    /// `true` iff the call was a no-op (already frozen).
    pub already_frozen: bool,
}

/// Move a molecule to the `Frozen` status.
///
/// Idempotent on `Frozen → Frozen`; rejects terminal states with 409.
///
/// # Errors
///
/// See [`FreezeError`] for the full failure shape: molecule-not-found,
/// terminal-status (Completed/Collapsed), store-unavailable.
///
/// # `#[verb]` registration
///
/// Annotated as `POST /v1/molecules/:id/freeze` (principal `tenant`).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/freeze",
    principal = "tenant"
)]
#[allow(clippy::needless_pass_by_value)]
pub fn freeze(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    id: &MoleculeId,
    request: FreezeRequest,
) -> Result<FreezeView, FreezeError> {
    let started = Instant::now();
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };

    let mol = store
        .load_molecule(id)
        .map_err(|e| FreezeError::from_cosmon(id, e))?;
    let prev_status = mol.status;

    if prev_status == MoleculeStatus::Frozen {
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        emit_authz_decision(
            state_dir,
            "freeze",
            &subject_kind,
            None,
            decision,
            latency_ms,
        );
        return Ok(FreezeView {
            id: id.clone(),
            previous_status: prev_status,
            already_frozen: true,
        });
    }

    if matches!(
        prev_status,
        MoleculeStatus::Completed | MoleculeStatus::Collapsed
    ) {
        return Err(FreezeError::TerminalStatus(id.clone(), prev_status));
    }

    let mut updated = mol;
    updated.status = MoleculeStatus::Frozen;
    updated.updated_at = Utc::now();
    store
        .save_molecule(&updated.id.clone(), &updated)
        .map_err(|e| FreezeError::from_cosmon(id, e))?;

    let events_path = state_dir.join("events.jsonl");
    let _ = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: id.clone(),
            from: prev_status.to_string(),
            to: "frozen".to_owned(),
        },
        None,
    );
    // Emit MoleculeStuck only when a reason is supplied — `freeze` is
    // the symmetrical pair with `thaw`, distinct from `stuck`'s
    // "blocker recorded" semantics. Without a reason we just record the
    // status transition above.
    if let Some(reason) = request.reason.clone() {
        let _ = event_log::emit_one(
            &events_path,
            EventV2::MoleculeStuck {
                molecule_id: id.clone(),
                reason: cosmon_core::event_v2::StuckReason::from(reason),
            },
            None,
        );
    }

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(
        state_dir,
        "freeze",
        &subject_kind,
        None,
        decision,
        latency_ms,
    );

    Ok(FreezeView {
        id: id.clone(),
        previous_status: prev_status,
        already_frozen: false,
    })
}

fn derive_subject_kind(subject: &Subject) -> String {
    if subject.id().as_str() == "operator" {
        "operator".to_owned()
    } else {
        format!("jwt:{}", subject.id().as_str())
    }
}

// ---------------------------------------------------------------------------
// Wire-format renderer
// ---------------------------------------------------------------------------

/// JSON body emitted by `cs --json` molecule-freeze flow and
/// `POST /v1/molecules/:id/freeze`.
#[derive(Debug, Clone, Serialize)]
pub struct FreezeJson {
    /// Molecule id (string form).
    pub molecule: String,
    /// Status the molecule held before this call.
    pub previous_status: String,
    /// Always `"frozen"` after a successful call.
    pub status: &'static str,
    /// `true` iff this call was a no-op.
    pub already_frozen: bool,
}

impl FreezeJson {
    /// Render a [`FreezeView`] into the canonical JSON wire shape.
    #[must_use]
    pub fn from_view(view: &FreezeView) -> Self {
        Self {
            molecule: view.id.to_string(),
            previous_status: view.previous_status.to_string(),
            status: "frozen",
            already_frozen: view.already_frozen,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Mutex;

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use tempfile::TempDir;

    use super::*;
    use crate::ops::error::is_kebab_case;
    use crate::{MoleculeData, MoleculeFilter};

    #[derive(Default)]
    struct FakeStore {
        molecules: Mutex<HashMap<MoleculeId, MoleculeData>>,
    }

    impl StateStore for FakeStore {
        fn load_fleet(&self) -> Result<crate::Fleet, CosmonError> {
            Ok(crate::Fleet::default())
        }
        fn save_fleet(&self, _fleet: &crate::Fleet) -> Result<(), CosmonError> {
            Ok(())
        }
        fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
            self.molecules
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(|| CosmonError::MoleculeNotFound(id.clone()))
        }
        fn save_molecule(&self, id: &MoleculeId, data: &MoleculeData) -> Result<(), CosmonError> {
            self.molecules
                .lock()
                .unwrap()
                .insert(id.clone(), data.clone());
            Ok(())
        }
        fn list_molecules(
            &self,
            _filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, CosmonError> {
            Ok(self.molecules.lock().unwrap().values().cloned().collect())
        }
    }

    fn mol(id: &str, status: MoleculeStatus) -> MoleculeData {
        let now = Utc::now();
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: now,
            updated_at: now,
            total_steps: 1,
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
    fn freeze_running_molecule_succeeds() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = freeze(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            FreezeRequest::default(),
        )
        .unwrap();
        assert!(!view.already_frozen);
        assert_eq!(view.previous_status, MoleculeStatus::Running);

        let reloaded = store.load_molecule(&id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Frozen);
    }

    #[test]
    fn freeze_already_frozen_is_idempotent() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Frozen);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = freeze(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            FreezeRequest::default(),
        )
        .unwrap();
        assert!(view.already_frozen);
    }

    #[test]
    fn freeze_completed_is_409() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Completed);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let err = freeze(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            FreezeRequest::default(),
        )
        .unwrap_err();
        assert!(matches!(err, FreezeError::TerminalStatus(_, _)));
        assert_eq!(err.http_status(), 409);
    }

    #[test]
    fn freeze_returns_not_found_for_unknown_id() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-zzzz").unwrap();
        let err = freeze(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            FreezeRequest::default(),
        )
        .unwrap_err();
        assert!(matches!(err, FreezeError::MoleculeNotFound(_)));
    }

    #[test]
    fn freeze_error_tags_are_kebab_case() {
        for e in &[
            FreezeError::MoleculeNotFound(MoleculeId::new("task-20260504-aaaa").unwrap()),
            FreezeError::TerminalStatus(
                MoleculeId::new("task-20260504-aaaa").unwrap(),
                MoleculeStatus::Completed,
            ),
            FreezeError::StoreUnavailable("x".into()),
        ] {
            assert!(
                is_kebab_case(e.tag()),
                "tag must be kebab-case: {}",
                e.tag()
            );
        }
    }

    #[test]
    fn freeze_json_renders_canonical_shape() {
        let view = FreezeView {
            id: MoleculeId::new("task-20260504-aaaa").unwrap(),
            previous_status: MoleculeStatus::Running,
            already_frozen: false,
        };
        let json = FreezeJson::from_view(&view);
        assert_eq!(json.molecule, "task-20260504-aaaa");
        assert_eq!(json.previous_status, "running");
        assert_eq!(json.status, "frozen");
        assert!(!json.already_frozen);
    }
}
