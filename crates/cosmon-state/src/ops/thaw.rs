// SPDX-License-Identifier: AGPL-3.0-only

//! `thaw` — `Frozen → Running` molecule-level transition.
//!
//! Symmetric pair with [`super::freeze`](fn@super::freeze). Idempotent in the
//! `Running → Running` direction (returns `already_thawed = true`),
//! rejects molecules that are not Frozen and not Running.

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

/// Errors returned by [`thaw`].
#[derive(Debug, thiserror::Error)]
pub enum ThawError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// The molecule is not in a thaw-able state (must be `Frozen` or
    /// already `Running`).
    #[error("molecule {0} is in status `{1}` — cannot thaw")]
    InvalidStatus(MoleculeId, MoleculeStatus),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl ThawError {
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for ThawError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::InvalidStatus(_, _) => "invalid-status",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::InvalidStatus(_, _) => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Inputs for [`thaw`].
///
/// Fusion v1.0.0-rc: when callers transit through
/// `POST /v1/molecules/{id}/freeze {"state":"active","reason":...}` the
/// adapter forwards the reason here so the `MoleculeStatusChanged`
/// (Frozen → Running) event carries the operator's words.
#[derive(Debug, Clone, Default)]
pub struct ThawRequest {
    /// Optional reason recorded on the `MoleculeStatusChanged` event
    /// (fusion v1.0.0-rc). When `None`, behaviour matches V0
    /// (no reason logged).
    pub reason: Option<String>,
}

/// Result of a successful [`thaw`] call.
#[derive(Debug, Clone)]
pub struct ThawView {
    /// Molecule that was thawed (or already was).
    pub id: MoleculeId,
    /// Status the molecule held before this call.
    pub previous_status: MoleculeStatus,
    /// `true` iff this call was a no-op (already Running).
    pub already_thawed: bool,
}

/// Move a molecule back from `Frozen` to `Running`.
///
/// Idempotent on `Running → Running`; rejects everything else with 409.
///
/// # Errors
///
/// See [`ThawError`] for the full failure shape: molecule-not-found,
/// invalid-status, store-unavailable.
///
/// # `#[verb]` registration
///
/// **Not** annotated as a `#[verb]` after fusion v1.0.0-rc.
/// The library entry point is still public —
/// the rpp-adapter dispatches to it from `POST /v1/molecules/:id/freeze`
/// when the body carries `state: "active"`. There is no longer a
/// dedicated wire route.
pub fn thaw(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    id: &MoleculeId,
    request: ThawRequest,
) -> Result<ThawView, ThawError> {
    let started = Instant::now();
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };

    // Fusion v1.0.0-rc: the reason is carried into the per-request
    // whisper by the rpp-adapter spark before this function runs (see
    // `crates/cosmon-rpp-adapter/src/admission.rs`). The library entry
    // point keeps the field on `ThawRequest` so non-HTTP callers
    // (cli, integration tests, future direct embeds) can supply one,
    // but does not double-log it here.
    drop(request.reason);

    let mol = store
        .load_molecule(id)
        .map_err(|e| ThawError::from_cosmon(id, e))?;
    let prev_status = mol.status;

    if prev_status == MoleculeStatus::Running {
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        emit_authz_decision(state_dir, "thaw", &subject_kind, None, decision, latency_ms);
        return Ok(ThawView {
            id: id.clone(),
            previous_status: prev_status,
            already_thawed: true,
        });
    }

    if prev_status != MoleculeStatus::Frozen {
        return Err(ThawError::InvalidStatus(id.clone(), prev_status));
    }

    let mut updated = mol;
    updated.status = MoleculeStatus::Running;
    updated.updated_at = Utc::now();
    // Clear the stuck-flavor marker on the way out of Frozen
    // (`task-20260509-177e`): once a molecule has been thawed, a later
    // `cs collapse` should report `previous_status: "running"`, not
    // `"stuck"`.
    updated.stuck_at = None;
    store
        .save_molecule(&updated.id.clone(), &updated)
        .map_err(|e| ThawError::from_cosmon(id, e))?;

    let events_path = state_dir.join("events.jsonl");
    let _ = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: id.clone(),
            from: prev_status.to_string(),
            to: "running".to_owned(),
        },
        None,
    );

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(state_dir, "thaw", &subject_kind, None, decision, latency_ms);

    Ok(ThawView {
        id: id.clone(),
        previous_status: prev_status,
        already_thawed: false,
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

/// JSON body emitted by `cs --json` molecule-thaw flow and
/// `POST /v1/molecules/:id/thaw`.
#[derive(Debug, Clone, Serialize)]
pub struct ThawJson {
    /// Molecule id (string form).
    pub molecule: String,
    /// Status the molecule held before this call.
    pub previous_status: String,
    /// Always `"running"` after a successful call.
    pub status: &'static str,
    /// `true` iff this call was a no-op.
    pub already_thawed: bool,
}

impl ThawJson {
    /// Render a [`ThawView`] into the canonical JSON wire shape.
    #[must_use]
    pub fn from_view(view: &ThawView) -> Self {
        Self {
            molecule: view.id.to_string(),
            previous_status: view.previous_status.to_string(),
            status: "running",
            already_thawed: view.already_thawed,
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
    fn thaw_frozen_molecule_succeeds() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Frozen);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = thaw(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            ThawRequest::default(),
        )
        .unwrap();
        assert!(!view.already_thawed);
        assert_eq!(view.previous_status, MoleculeStatus::Frozen);

        let reloaded = store.load_molecule(&id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Running);
    }

    #[test]
    fn thaw_running_molecule_is_idempotent() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = thaw(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            ThawRequest::default(),
        )
        .unwrap();
        assert!(view.already_thawed);
    }

    #[test]
    fn thaw_pending_molecule_is_409() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Pending);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let err = thaw(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            ThawRequest::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ThawError::InvalidStatus(_, _)));
        assert_eq!(err.http_status(), 409);
    }

    #[test]
    fn thaw_returns_not_found_for_unknown_id() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-zzzz").unwrap();
        let err = thaw(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            ThawRequest::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ThawError::MoleculeNotFound(_)));
    }

    #[test]
    fn thaw_error_tags_are_kebab_case() {
        for e in &[
            ThawError::MoleculeNotFound(MoleculeId::new("task-20260504-aaaa").unwrap()),
            ThawError::InvalidStatus(
                MoleculeId::new("task-20260504-aaaa").unwrap(),
                MoleculeStatus::Pending,
            ),
            ThawError::StoreUnavailable("x".into()),
        ] {
            assert!(
                is_kebab_case(e.tag()),
                "tag must be kebab-case: {}",
                e.tag()
            );
        }
    }

    #[test]
    fn thaw_json_renders_canonical_shape() {
        let view = ThawView {
            id: MoleculeId::new("task-20260504-aaaa").unwrap(),
            previous_status: MoleculeStatus::Frozen,
            already_thawed: false,
        };
        let json = ThawJson::from_view(&view);
        assert_eq!(json.molecule, "task-20260504-aaaa");
        assert_eq!(json.previous_status, "frozen");
        assert_eq!(json.status, "running");
        assert!(!json.already_thawed);
    }
}
