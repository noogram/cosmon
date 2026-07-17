// SPDX-License-Identifier: AGPL-3.0-only

//! `stuck` — record a blocker on a molecule and freeze it.
//!
//! Mutation verb extracted under `cosmon_state::ops`. Lib path of
//! cs-cli `cs stuck`. Transitions any
//! non-terminal molecule to `Frozen` and emits a `MoleculeStuck` event
//! with the operator-supplied reason.
//!
//! Distinct from [`super::freeze`](fn@super::freeze): `stuck` always records a `reason`
//! (mandatory) and emits a `MoleculeStuck` event; `freeze` is the
//! symmetric pair with `thaw` and the reason is optional.

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

/// Errors returned by [`stuck`].
#[derive(Debug, thiserror::Error)]
pub enum StuckError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// Caller did not supply a `reason` (it is mandatory for stuck).
    #[error("stuck request must include a `reason`")]
    EmptyReason,
    /// The molecule is in a terminal state.
    #[error("molecule {0} is in terminal status `{1}` — cannot mark stuck")]
    TerminalStatus(MoleculeId, MoleculeStatus),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl StuckError {
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for StuckError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::EmptyReason => "empty-reason",
            Self::TerminalStatus(_, _) => "terminal-status",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::EmptyReason => 400,
            Self::TerminalStatus(_, _) => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Inputs for [`stuck`].
#[derive(Debug, Clone)]
pub struct StuckRequest {
    /// Mandatory free-form reason for the blocker.
    pub reason: String,
}

/// Result of a successful [`stuck`] call.
#[derive(Debug, Clone)]
pub struct StuckView {
    /// Molecule that was marked stuck.
    pub id: MoleculeId,
    /// Status the molecule held before this call.
    pub previous_status: MoleculeStatus,
    /// Reason recorded.
    pub reason: String,
    /// `true` iff the molecule was already Frozen and the call was a
    /// no-op transition (still emits the `StuckReason` event).
    pub already_frozen: bool,
}

/// Mark a molecule as stuck and freeze it.
///
/// Idempotent on `Frozen → Frozen`. Rejects terminal molecules with 409.
///
/// # Errors
///
/// See [`StuckError`] for the full failure shape: empty-reason,
/// molecule-not-found, terminal-status, store-unavailable.
///
/// # `#[verb]` registration
///
/// Annotated as `POST /v1/molecules/:id/stuck` (principal `tenant`).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/stuck",
    principal = "tenant"
)]
#[allow(clippy::needless_pass_by_value)]
pub fn stuck(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    id: &MoleculeId,
    request: StuckRequest,
) -> Result<StuckView, StuckError> {
    let started = Instant::now();
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };

    if request.reason.trim().is_empty() {
        return Err(StuckError::EmptyReason);
    }

    let mol = store
        .load_molecule(id)
        .map_err(|e| StuckError::from_cosmon(id, e))?;
    let prev_status = mol.status;

    if matches!(
        prev_status,
        MoleculeStatus::Completed | MoleculeStatus::Collapsed
    ) {
        return Err(StuckError::TerminalStatus(id.clone(), prev_status));
    }

    let already_frozen = prev_status == MoleculeStatus::Frozen;

    let now = Utc::now();
    let mut updated = mol;
    updated.status = MoleculeStatus::Frozen;
    updated.updated_at = now;
    // Mark this Frozen as stuck-flavored (`task-20260509-177e`): a later
    // `cs collapse` reads this to render `previous_status: "stuck"` rather
    // than `"frozen"`, preserving the cognitive context of the operator's
    // gesture that `cs freeze` would not carry.
    updated.stuck_at = Some(now);
    store
        .save_molecule(&updated.id.clone(), &updated)
        .map_err(|e| StuckError::from_cosmon(id, e))?;

    let events_path = state_dir.join("events.jsonl");
    if !already_frozen {
        let _ = event_log::emit_one(
            &events_path,
            EventV2::MoleculeStatusChanged {
                molecule_id: id.clone(),
                from: prev_status.to_string(),
                to: "frozen".to_owned(),
            },
            None,
        );
    }
    let _ = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStuck {
            molecule_id: id.clone(),
            reason: cosmon_core::event_v2::StuckReason::from(request.reason.clone()),
        },
        None,
    );

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(
        state_dir,
        "stuck",
        &subject_kind,
        None,
        decision,
        latency_ms,
    );

    Ok(StuckView {
        id: id.clone(),
        previous_status: prev_status,
        reason: request.reason,
        already_frozen,
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

/// JSON body emitted by `cs --json stuck` and
/// `POST /v1/molecules/:id/stuck`.
///
/// Matches the cs-cli `cs stuck` renderer modulo the `archived` field
/// (see [`super::collapse::CollapseJson`] note). Wire-stable: `status`,
/// `molecule`, `reason` are the canonical field names.
#[derive(Debug, Clone, Serialize)]
pub struct StuckJson {
    /// Always `"stuck"` after a successful call (mirrors `cs stuck` cli).
    pub status: &'static str,
    /// Molecule id (string form).
    pub molecule: String,
    /// Reason recorded.
    pub reason: String,
}

impl StuckJson {
    /// Render a [`StuckView`] into the canonical JSON wire shape.
    #[must_use]
    pub fn from_view(view: &StuckView) -> Self {
        Self {
            status: "stuck",
            molecule: view.id.to_string(),
            reason: view.reason.clone(),
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
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn stuck_running_molecule_succeeds() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = stuck(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            StuckRequest {
                reason: "blocker".into(),
            },
        )
        .unwrap();
        assert_eq!(view.previous_status, MoleculeStatus::Running);
        assert!(!view.already_frozen);
        assert_eq!(view.reason, "blocker");
        let reloaded = store.load_molecule(&id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Frozen);
    }

    #[test]
    fn stuck_already_frozen_emits_event_idempotently() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Frozen);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = stuck(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            StuckRequest {
                reason: "still blocked".into(),
            },
        )
        .unwrap();
        assert!(view.already_frozen);
    }

    #[test]
    fn stuck_completed_is_409() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Completed);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();
        let err = stuck(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            StuckRequest { reason: "x".into() },
        )
        .unwrap_err();
        assert!(matches!(err, StuckError::TerminalStatus(_, _)));
        assert_eq!(err.http_status(), 409);
    }

    #[test]
    fn stuck_empty_reason_is_400() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();
        let err = stuck(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            StuckRequest {
                reason: "   ".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, StuckError::EmptyReason));
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn stuck_returns_not_found_for_unknown_id() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-zzzz").unwrap();
        let err = stuck(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            StuckRequest { reason: "x".into() },
        )
        .unwrap_err();
        assert!(matches!(err, StuckError::MoleculeNotFound(_)));
    }

    #[test]
    fn stuck_error_tags_are_kebab_case() {
        for e in &[
            StuckError::MoleculeNotFound(MoleculeId::new("task-20260504-aaaa").unwrap()),
            StuckError::EmptyReason,
            StuckError::TerminalStatus(
                MoleculeId::new("task-20260504-aaaa").unwrap(),
                MoleculeStatus::Completed,
            ),
            StuckError::StoreUnavailable("x".into()),
        ] {
            assert!(
                is_kebab_case(e.tag()),
                "tag must be kebab-case: {}",
                e.tag()
            );
        }
    }

    #[test]
    fn stuck_json_renders_canonical_shape() {
        let view = StuckView {
            id: MoleculeId::new("task-20260504-aaaa").unwrap(),
            previous_status: MoleculeStatus::Running,
            reason: "blocker".to_owned(),
            already_frozen: false,
        };
        let json = StuckJson::from_view(&view);
        assert_eq!(json.molecule, "task-20260504-aaaa");
        assert_eq!(json.status, "stuck");
        assert_eq!(json.reason, "blocker");
    }
}
