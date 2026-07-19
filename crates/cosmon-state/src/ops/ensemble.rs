// SPDX-License-Identifier: AGPL-3.0-only

//! `ensemble` — read-only listing of molecules with optional filters.
//!
//! Fourth verb extracted under `cosmon_state::ops`. Read-only sibling
//! of [`super::observe`](fn@super::observe): where
//! `observe` projects a single molecule, `ensemble` projects a filtered
//! slice of the fleet's molecules.
//!
//! The V1 RPP API exposes the verb as `GET /v1/molecules` with optional
//! query parameters `?tag=&kind=&status=`. The filter is intentionally
//! narrow — the rich `cs ensemble` CLI surface (per-fleet aggregation,
//! cluster mode, energy roll-up) stays in the cs-cli; the lib path covers
//! the per-tenant "show me my molecules" cut operator-demo-facing pilots need.
//!
//! # Why a separate verb (and not a wider observe)
//!
//! `observe` and `ensemble` differ in *cardinality*, not in projection.
//! Threading a list shape through `observe`'s response would force every
//! caller to parse a discriminated union; instead we keep the two slim
//! views (`MoleculeView` per molecule, `EnsembleView` per call) and let
//! the wire shape stay flat. Future filters (`?worker=`, `?formula=`)
//! land here, not on `observe`.

use std::path::Path;
use std::time::Instant;

use cosmon_core::auth::Subject;
use cosmon_core::id::FleetId;
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use serde::Serialize;

use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::{MoleculeData, MoleculeFilter, StateStore};
use cosmon_core::error::CosmonError;

/// Errors returned by [`ensemble`].
///
/// Per-verb pattern: dedicated enum, no flattening through a mega
/// `CosmonError`. Both variants ride [`OpsError`] for stable kebab-case
/// tags + HTTP status mapping.
#[derive(Debug, thiserror::Error)]
pub enum EnsembleError {
    /// A filter argument failed to parse (`status=foo` for an unknown
    /// status, `kind=quux` for an unknown kind, …).
    #[error("invalid filter: {0}")]
    InvalidFilter(String),
    /// The state store could not be read.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for EnsembleError {
    fn tag(&self) -> &'static str {
        match self {
            Self::InvalidFilter(_) => "invalid-filter",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::InvalidFilter(_) => 400,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Inputs for [`ensemble`].
///
/// Each field maps to a query parameter on `GET /v1/molecules`:
///
/// - `status` → `?status=running` (parsed via `MoleculeStatus::from_str`).
/// - `kind` → `?kind=task` (parsed via [`MoleculeKind`]).
/// - `tag_globs` → repeated `?tag=temp:hot&tag=deferred:*` (each entry is
///   a glob pattern matched against the molecule's tag set).
/// - `fleet` → reserved for a future `?fleet=foo`; today we always read
///   from the tenant's whole state store.
#[derive(Debug, Clone, Default)]
pub struct EnsembleRequest {
    /// Optional status filter. `None` → don't filter on status.
    pub status: Option<String>,
    /// Optional kind filter. `None` → don't filter on kind.
    pub kind: Option<String>,
    /// Optional tag glob filter; each entry is a tag glob pattern.
    pub tag_globs: Vec<String>,
    /// Optional fleet filter. `None` → don't filter on fleet.
    pub fleet: Option<String>,
}

/// Result of a successful [`ensemble`] call.
#[derive(Debug, Clone)]
pub struct EnsembleView {
    /// Matching molecules, in insertion order (deterministic per
    /// underlying [`StateStore::list_molecules`] contract).
    pub molecules: Vec<MoleculeData>,
}

/// List molecules matching the given filter.
///
/// Read-only — never writes to the store, never emits domain events.
/// Emits exactly one [`AuthzDecisionEvaluated`](crate::instrumentation::AuthzDecisionEvaluated)
/// per call, same V0 grid as the other verbs.
///
/// # Errors
///
/// Returns [`EnsembleError::InvalidFilter`] when a filter string fails
/// to parse, and [`EnsembleError::StoreUnavailable`] when the underlying
/// store cannot be read.
///
/// # `#[verb]` registration
///
/// Annotated as `GET /v1/molecules` (principal `tenant`).
#[cosmon_thin_macro::verb(method = "GET", path = "/v1/molecules", principal = "tenant")]
#[allow(clippy::needless_pass_by_value)]
pub fn ensemble(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    request: EnsembleRequest,
) -> Result<EnsembleView, EnsembleError> {
    let started = Instant::now();
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };

    let mut filter = MoleculeFilter::default();
    if let Some(s) = &request.status {
        let parsed: MoleculeStatus = s
            .parse()
            .map_err(|_| EnsembleError::InvalidFilter(format!("status={s}")))?;
        filter.status = Some(parsed);
    }
    if let Some(k) = &request.kind {
        let parsed: MoleculeKind = k
            .parse()
            .map_err(|_| EnsembleError::InvalidFilter(format!("kind={k}")))?;
        filter.kind = Some(parsed);
    }
    if !request.tag_globs.is_empty() {
        filter.tag_globs.clone_from(&request.tag_globs);
    }
    if let Some(f) = &request.fleet {
        let parsed =
            FleetId::new(f).map_err(|e| EnsembleError::InvalidFilter(format!("fleet={f}: {e}")))?;
        filter.fleet = Some(parsed);
    }

    let molecules = store
        .list_molecules(&filter)
        .map_err(|e| EnsembleError::StoreUnavailable(format_cosmon(&e)))?;

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(
        state_dir,
        "ensemble",
        &subject_kind,
        None,
        decision,
        latency_ms,
    );

    Ok(EnsembleView { molecules })
}

fn format_cosmon(err: &CosmonError) -> String {
    err.to_string()
}

fn derive_subject_kind(subject: &Subject) -> String {
    if subject.id().as_str() == "operator" {
        "operator".to_owned()
    } else {
        format!("jwt:{}", subject.id().as_str())
    }
}

// ---------------------------------------------------------------------------
// Wire-format renderer — keeps the byte layout consumed by external
// scripts stable across cs-cli and cs-api.
// ---------------------------------------------------------------------------

/// JSON body emitted by `cs --json ensemble`-equivalent flow and
/// `GET /v1/molecules`. One entry per matching molecule.
///
/// Slim by design: just `id`, `formula`, `status`, `current_step`,
/// `total_steps`, `worker`, `tags`, `created_at`. Callers that need the
/// full molecule projection (coupling report, ghost, energy) reach for
/// `observe :id` per molecule — `ensemble` is the index, `observe` is
/// the page.
#[derive(Debug, Clone, Serialize)]
pub struct EnsembleEntryJson {
    /// Molecule id.
    pub id: String,
    /// Source formula id.
    pub formula: String,
    /// Lifecycle status string.
    pub status: String,
    /// Current step index.
    pub current_step: usize,
    /// Total number of steps.
    pub total_steps: usize,
    /// Assigned worker, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// Tags (sorted lexically).
    pub tags: Vec<String>,
    /// Created-at RFC3339.
    pub created_at: String,
}

impl EnsembleEntryJson {
    /// Render a single [`MoleculeData`] into the canonical entry shape.
    #[must_use]
    pub fn from_data(mol: &MoleculeData) -> Self {
        Self {
            id: mol.id.to_string(),
            formula: mol.formula_id.to_string(),
            status: mol.status.to_string(),
            current_step: mol.current_step,
            total_steps: mol.total_steps,
            worker: mol.assigned_worker.as_ref().map(ToString::to_string),
            tags: mol.tags.iter().map(ToString::to_string).collect(),
            created_at: mol.created_at.to_rfc3339(),
        }
    }
}

/// Wire-format wrapper: `{ "molecules": [ EnsembleEntryJson, ... ] }`.
///
/// Returned verbatim from the `GET /v1/molecules` route under `body`.
/// The same shape is what `cs-thin ensemble` prints to stdout.
#[derive(Debug, Clone, Serialize)]
pub struct EnsembleJson {
    /// Matching molecules in canonical entry shape.
    pub molecules: Vec<EnsembleEntryJson>,
    /// Total number of matching molecules — duplicates `molecules.len()`
    /// for clients that want a quick summary without parsing the array.
    pub total: usize,
}

impl EnsembleJson {
    /// Render an [`EnsembleView`] into the canonical wire shape.
    #[must_use]
    pub fn from_view(view: &EnsembleView) -> Self {
        let molecules: Vec<EnsembleEntryJson> = view
            .molecules
            .iter()
            .map(EnsembleEntryJson::from_data)
            .collect();
        let total = molecules.len();
        Self { molecules, total }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Mutex;

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::tag::Tag;
    use tempfile::TempDir;

    use super::*;
    use crate::ops::error::is_kebab_case;

    #[derive(Default)]
    struct FakeStore {
        molecules: Mutex<HashMap<MoleculeId, MoleculeData>>,
    }

    impl FakeStore {
        fn insert(&self, mol: MoleculeData) {
            self.molecules.lock().unwrap().insert(mol.id.clone(), mol);
        }
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
            filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, CosmonError> {
            let mols = self.molecules.lock().unwrap();
            let out: Vec<MoleculeData> = mols
                .values()
                .filter(|m| filter.status.is_none_or(|s| m.status == s))
                .filter(|m| filter.kind.is_none_or(|k| m.kind == Some(k)))
                .filter(|m| {
                    if filter.tag_globs.is_empty() {
                        return true;
                    }
                    filter
                        .tag_globs
                        .iter()
                        .any(|glob| m.tags.iter().any(|t| t.matches_glob(glob)))
                })
                .filter(|m| filter.fleet.as_ref().is_none_or(|f| &m.fleet_id == f))
                .cloned()
                .collect();
            Ok(out)
        }
    }

    fn mol(id: &str, status: MoleculeStatus, tags: &[&str]) -> MoleculeData {
        let now = Utc::now();
        let tag_set: BTreeSet<Tag> = tags.iter().map(|t| Tag::new(*t).unwrap()).collect();
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: Some(WorkerId::new("ruby").unwrap()),
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
            tags: tag_set,
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
        }
    }

    #[test]
    fn ensemble_returns_all_molecules_when_filter_empty() {
        let store = FakeStore::default();
        store.insert(mol("task-20260504-aaaa", MoleculeStatus::Running, &[]));
        store.insert(mol("task-20260504-bbbb", MoleculeStatus::Pending, &[]));
        let tmp = TempDir::new().unwrap();

        let view = ensemble(
            &store,
            tmp.path(),
            &Subject::operator(),
            EnsembleRequest::default(),
        )
        .unwrap();
        assert_eq!(view.molecules.len(), 2);
    }

    #[test]
    fn ensemble_filters_by_status() {
        let store = FakeStore::default();
        store.insert(mol("task-20260504-aaaa", MoleculeStatus::Running, &[]));
        store.insert(mol("task-20260504-bbbb", MoleculeStatus::Pending, &[]));
        let tmp = TempDir::new().unwrap();

        let req = EnsembleRequest {
            status: Some("running".to_owned()),
            ..EnsembleRequest::default()
        };
        let view = ensemble(&store, tmp.path(), &Subject::operator(), req).unwrap();
        assert_eq!(view.molecules.len(), 1);
        assert_eq!(view.molecules[0].status, MoleculeStatus::Running);
    }

    #[test]
    fn ensemble_returns_invalid_filter_for_garbage_status() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let req = EnsembleRequest {
            status: Some("not-a-status".to_owned()),
            ..EnsembleRequest::default()
        };
        let err = ensemble(&store, tmp.path(), &Subject::operator(), req).unwrap_err();
        assert!(matches!(err, EnsembleError::InvalidFilter(_)));
        assert_eq!(err.tag(), "invalid-filter");
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn ensemble_filters_by_tag_glob() {
        let store = FakeStore::default();
        store.insert(mol(
            "task-20260504-aaaa",
            MoleculeStatus::Running,
            &["temp:hot"],
        ));
        store.insert(mol(
            "task-20260504-bbbb",
            MoleculeStatus::Running,
            &["temp:warm"],
        ));
        let tmp = TempDir::new().unwrap();

        let req = EnsembleRequest {
            tag_globs: vec!["temp:hot".to_owned()],
            ..EnsembleRequest::default()
        };
        let view = ensemble(&store, tmp.path(), &Subject::operator(), req).unwrap();
        assert_eq!(view.molecules.len(), 1);
    }

    #[test]
    fn ensemble_error_tags_are_kebab_case() {
        for e in &[
            EnsembleError::InvalidFilter("x".into()),
            EnsembleError::StoreUnavailable("x".into()),
        ] {
            assert!(
                is_kebab_case(e.tag()),
                "tag must be kebab-case: {}",
                e.tag()
            );
        }
    }

    #[test]
    fn ensemble_json_roundtrips() {
        let store = FakeStore::default();
        store.insert(mol(
            "task-20260504-aaaa",
            MoleculeStatus::Running,
            &["temp:hot"],
        ));
        let tmp = TempDir::new().unwrap();

        let view = ensemble(
            &store,
            tmp.path(),
            &Subject::operator(),
            EnsembleRequest::default(),
        )
        .unwrap();
        let json = EnsembleJson::from_view(&view);
        assert_eq!(json.total, 1);
        assert_eq!(json.molecules.len(), 1);
        assert_eq!(json.molecules[0].id, "task-20260504-aaaa");
        assert_eq!(json.molecules[0].status, "running");
        assert_eq!(json.molecules[0].tags, vec!["temp:hot"]);
    }

    #[test]
    fn ensemble_emits_authz_decision_for_operator() {
        use crate::instrumentation::{read_authz_ndjson, AUTHZ_NDJSON_RELATIVE_PATH};

        let store = FakeStore::default();
        store.insert(mol("task-20260504-aaaa", MoleculeStatus::Running, &[]));
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

        let _ = ensemble(
            &store,
            tmp.path(),
            &Subject::operator(),
            EnsembleRequest::default(),
        )
        .unwrap();

        let path = tmp.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
        let events = read_authz_ndjson(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verb, "ensemble");
        assert_eq!(events[0].subject_kind, "operator");
    }
}
