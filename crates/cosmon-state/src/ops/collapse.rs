// SPDX-License-Identifier: AGPL-3.0-only

//! `collapse` — terminal Active|Pending|Starved → Collapsed transition.
//!
//! Mutation verb extracted under `cosmon_state::ops`. Mirrors the
//! cs-cli `cs collapse` handler at the
//! V1 RPP API: a structured, idempotent transition to the Collapsed
//! terminal state with a free-form reason.
//!
//! Idempotent: collapsing an already-collapsed molecule is a no-op (no
//! error). Calling collapse on a `Completed` molecule is a hard error
//! — completing and collapsing are mutually exclusive terminal states.
//!
//! The lib path covers the body shape `{ reason: String, cause?,
//! account?, kind? }` that the V1 RPP exposes; the rich `--ops-dir`
//! discovery, `--cause rate_limit` triple validation, and log.md
//! appender stay in cs-cli for now.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::time::Instant;

use chrono::Utc;
use cosmon_core::auth::Subject;
use cosmon_core::error::CosmonError;
use cosmon_core::event_v2::{CollapseReason, EventV2};
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::{CollapseCause, MoleculeStatus};
use serde::Serialize;

use crate::event_log;
use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::StateStore;

/// Errors returned by [`collapse`].
#[derive(Debug, thiserror::Error)]
pub enum CollapseError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// Caller supplied a `cause` string that is not a known
    /// [`CollapseCause`] variant.
    #[error("invalid cause: {0}")]
    InvalidCause(String),
    /// `--account` / `--kind` were supplied without `cause=rate_limit`.
    #[error("--account / --kind only valid with cause=rate_limit (received cause={0})")]
    MismatchedAccountKind(String),
    /// The molecule is already in a terminal `Completed` state and
    /// cannot be collapsed.
    #[error("molecule {0} is completed — cannot collapse a completed molecule")]
    AlreadyCompleted(MoleculeId),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl CollapseError {
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for CollapseError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::InvalidCause(_) => "invalid-cause",
            Self::MismatchedAccountKind(_) => "mismatched-account-kind",
            Self::AlreadyCompleted(_) => "already-completed",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::InvalidCause(_) | Self::MismatchedAccountKind(_) => 400,
            Self::AlreadyCompleted(_) => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Inputs for [`collapse`].
#[derive(Debug, Clone)]
pub struct CollapseRequest {
    /// Free-form reason for the collapse.
    pub reason: String,
    /// Structured cause attribution (`rate_limit`, `inference_stall`, …).
    pub cause: Option<String>,
    /// Account alias, only meaningful when `cause = rate_limit`.
    pub account: Option<String>,
    /// Quota currency name, only meaningful when `cause = rate_limit`.
    pub kind: Option<String>,
    /// Operator-facing collapse classification (`worker_crashed`,
    /// `gate_failed`, `blocker_stuck`, `manual_abort`,
    /// `resource_exhausted`, or any free-form string for `Other`).
    /// Drives `cs errors` aggregation.
    pub reason_kind: Option<CollapseReason>,
}

impl CollapseRequest {
    /// Build a request with just a reason; everything else `None`.
    #[must_use]
    pub fn for_reason(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            cause: None,
            account: None,
            kind: None,
            reason_kind: None,
        }
    }
}

/// Result of a successful [`collapse`] call.
#[derive(Debug, Clone)]
pub struct CollapseView {
    /// Molecule that was collapsed (or already was).
    pub id: MoleculeId,
    /// Status the molecule held before this call (always `Collapsed`
    /// when `already_collapsed` is `true`).
    pub previous_status: MoleculeStatus,
    /// Reason recorded on the molecule.
    pub reason: String,
    /// Structured cause attribution, when supplied.
    pub cause: Option<CollapseCause>,
    /// `true` iff the molecule was already collapsed and the call was a
    /// no-op.
    pub already_collapsed: bool,
    /// `true` iff `previous_status` is `Frozen` *and* the prior
    /// transition to `Frozen` was via `cs stuck` (not `cs freeze`).
    /// Read from `MoleculeData.stuck_at`. Drives
    /// the wire-level `previous_status: "stuck"` rendering in
    /// [`CollapseJson`] and the `MoleculeStatusChanged.from = "stuck"`
    /// event so audit trails preserve the operator's gesture.
    pub previous_was_stuck: bool,
}

/// Move a molecule to the `Collapsed` terminal state.
///
/// Idempotent in the `Collapsed → Collapsed` direction (returns
/// `already_collapsed = true`). Rejects `Completed` molecules with a
/// 409 — completing and collapsing are mutually exclusive.
///
/// # Errors
///
/// See [`CollapseError`] for the full failure shape.
///
/// # `#[verb]` registration
///
/// Annotated as `POST /v1/molecules/:id/collapse` (principal `tenant`).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/collapse",
    principal = "tenant"
)]
#[allow(clippy::too_many_lines)]
pub fn collapse(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    id: &MoleculeId,
    request: CollapseRequest,
) -> Result<CollapseView, CollapseError> {
    let started = Instant::now();
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };

    // Parse cause + account/kind triple before touching the store.
    let cause = parse_cause(&request)?;

    let mol = store
        .load_molecule(id)
        .map_err(|e| CollapseError::from_cosmon(id, e))?;
    let prev_status = mol.status;
    // Stuck-flavored Frozen: if the molecule reached `Frozen` via
    // `cs stuck` (not `cs freeze`), the wire-level `previous_status`
    // should render as `"stuck"`. The `stuck_at` marker is set by
    // `cs stuck` and cleared on the way back to `Running`
    // (`task-20260509-177e`).
    let previous_was_stuck = prev_status == MoleculeStatus::Frozen && mol.stuck_at.is_some();

    // Idempotent re-entry.
    if prev_status == MoleculeStatus::Collapsed {
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        emit_authz_decision(
            state_dir,
            "collapse",
            &subject_kind,
            None,
            decision,
            latency_ms,
        );
        return Ok(CollapseView {
            id: id.clone(),
            previous_status: prev_status,
            reason: request.reason,
            cause,
            already_collapsed: true,
            previous_was_stuck,
        });
    }

    if prev_status == MoleculeStatus::Completed {
        return Err(CollapseError::AlreadyCompleted(id.clone()));
    }

    let mut updated = mol;
    updated.status = MoleculeStatus::Collapsed;
    updated.collapse_reason = Some(request.reason.clone());
    updated.collapse_cause.clone_from(&cause);
    updated
        .collapse_reason_kind
        .clone_from(&request.reason_kind);
    updated.collapsed_step = Some(updated.current_step);
    if updated.process.is_some() {
        updated.release_process();
    }
    updated.updated_at = Utc::now();
    store
        .save_molecule(&updated.id.clone(), &updated)
        .map_err(|e| CollapseError::from_cosmon(id, e))?;

    // Append to log.md best-effort.
    let mol_dir = state_dir
        .join("fleets")
        .join(updated.fleet_id.as_str())
        .join("molecules")
        .join(updated.id.as_str());
    let log_path = mol_dir.join("log.md");
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M UTC");
    let mut log_entry = String::new();
    let _ = write!(
        log_entry,
        "\n## {timestamp} — Collapsed\n\n{}\n\nCollapsed at step {}/{}.\n",
        request.reason, updated.current_step, updated.total_steps
    );
    if let Ok(existing) = fs::read_to_string(&log_path) {
        let new_log = if existing.is_empty() {
            format!("# Evolution Log\n{log_entry}")
        } else {
            format!("{existing}{log_entry}")
        };
        let _ = fs::write(&log_path, new_log);
    } else {
        let _ = fs::create_dir_all(&mol_dir);
        let _ = fs::write(&log_path, format!("# Evolution Log\n{log_entry}"));
    }

    // Emit EventV2 records. Legacy `cosmon_filestore::event::append`
    // calls remain in cs-cli; the lib path keeps the canonical V2
    // stream and lets cs-cli own the legacy mirror.
    let events_path = state_dir.join("events.jsonl");
    // When the prior transition was a stuck-flavored Frozen, emit
    // `from: "stuck"` so the audit trail records the operator's gesture
    // rather than the underlying physical status (`task-20260509-177e`).
    let from_label = if previous_was_stuck {
        "stuck".to_owned()
    } else {
        prev_status.to_string()
    };
    let status_seq = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: id.clone(),
            from: from_label,
            to: "collapsed".to_owned(),
        },
        None,
    )
    .ok();
    let _ = event_log::emit_one(
        &events_path,
        EventV2::MoleculeCollapsed {
            molecule_id: id.clone(),
            reason: request.reason.clone(),
            kind: request.reason_kind.clone(),
        },
        status_seq,
    );

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(
        state_dir,
        "collapse",
        &subject_kind,
        None,
        decision,
        latency_ms,
    );

    Ok(CollapseView {
        id: id.clone(),
        previous_status: prev_status,
        reason: request.reason,
        cause,
        already_collapsed: false,
        previous_was_stuck,
    })
}

fn parse_cause(request: &CollapseRequest) -> Result<Option<CollapseCause>, CollapseError> {
    let Some(raw) = request.cause.as_deref() else {
        if request.account.is_some() || request.kind.is_some() {
            return Err(CollapseError::MismatchedAccountKind("<absent>".to_owned()));
        }
        return Ok(None);
    };
    let mut cause =
        CollapseCause::from_str(raw).map_err(|_| CollapseError::InvalidCause(raw.to_owned()))?;
    if let CollapseCause::RateLimit {
        account,
        kind_quota,
    } = &mut cause
    {
        account.clone_from(&request.account);
        kind_quota.clone_from(&request.kind);
    } else if request.account.is_some() || request.kind.is_some() {
        return Err(CollapseError::MismatchedAccountKind(raw.to_owned()));
    }
    Ok(Some(cause))
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

/// JSON body emitted by `cs --json collapse` and
/// `POST /v1/molecules/:id/collapse`.
///
/// Wire shape mirrors the cs-cli renderer exactly so cs-thin parity is
/// trivial: `molecule`, `previous_status`, `status`, `reason`, `cause`,
/// `archived`. We omit `archived` here because the lib path doesn't
/// implement archiving (that stays in cs-cli for now); rev parity test
/// strips it on the cs side.
#[derive(Debug, Clone, Serialize)]
pub struct CollapseJson {
    /// Molecule id (string form).
    pub molecule: String,
    /// Status the molecule held before this call.
    pub previous_status: String,
    /// Always `"collapsed"` after a successful call.
    pub status: &'static str,
    /// Reason recorded on the molecule.
    pub reason: String,
    /// Structured cause attribution, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<CollapseCause>,
    /// `true` iff this call was a no-op (already collapsed).
    pub already_collapsed: bool,
}

impl CollapseJson {
    /// Render a [`CollapseView`] into the canonical JSON wire shape.
    ///
    /// When the molecule reached `Frozen` via `cs stuck` (not `cs freeze`),
    /// the wire-level `previous_status` field is rendered as `"stuck"`
    /// rather than the physical `"frozen"` — preserving the operator's
    /// gesture in audit trails.
    #[must_use]
    pub fn from_view(view: &CollapseView) -> Self {
        let previous_status = if view.previous_was_stuck {
            "stuck".to_owned()
        } else {
            view.previous_status.to_string()
        };
        Self {
            molecule: view.id.to_string(),
            previous_status,
            status: "collapsed",
            reason: view.reason.clone(),
            cause: view.cause.clone(),
            already_collapsed: view.already_collapsed,
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
    fn collapse_running_molecule_succeeds() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = collapse(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            CollapseRequest::for_reason("test"),
        )
        .unwrap();
        assert_eq!(view.previous_status, MoleculeStatus::Running);
        assert!(!view.already_collapsed);

        let reloaded = store.load_molecule(&id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        assert_eq!(reloaded.collapse_reason.as_deref(), Some("test"));
    }

    #[test]
    fn collapse_already_collapsed_is_idempotent() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Collapsed);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let view = collapse(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            CollapseRequest::for_reason("again"),
        )
        .unwrap();
        assert!(view.already_collapsed);
    }

    #[test]
    fn collapse_completed_is_409() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Completed);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let err = collapse(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            CollapseRequest::for_reason("nope"),
        )
        .unwrap_err();
        assert!(matches!(err, CollapseError::AlreadyCompleted(_)));
        assert_eq!(err.http_status(), 409);
    }

    #[test]
    fn collapse_invalid_cause_is_400() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let mut req = CollapseRequest::for_reason("test");
        req.cause = Some("not-a-cause".to_owned());
        let err = collapse(&store, tmp.path(), &Subject::operator(), &id, req).unwrap_err();
        assert!(matches!(err, CollapseError::InvalidCause(_)));
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn collapse_account_without_rate_limit_rejected() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let mut req = CollapseRequest::for_reason("test");
        req.cause = Some("manual".to_owned());
        req.account = Some("tenant_auditor".to_owned());
        let err = collapse(&store, tmp.path(), &Subject::operator(), &id, req).unwrap_err();
        assert!(matches!(err, CollapseError::MismatchedAccountKind(_)));
    }

    #[test]
    fn collapse_records_rate_limit_cause_with_account_and_kind() {
        let store = FakeStore::default();
        let m = mol("task-20260504-aaaa", MoleculeStatus::Running);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-aaaa").unwrap();

        let mut req = CollapseRequest::for_reason("Claude usage limit reached");
        req.cause = Some("rate_limit".to_owned());
        req.account = Some("you".to_owned());
        req.kind = Some("max_rolling_5h".to_owned());
        let view = collapse(&store, tmp.path(), &Subject::operator(), &id, req).unwrap();
        match view.cause {
            Some(CollapseCause::RateLimit {
                account,
                kind_quota,
            }) => {
                assert_eq!(account.as_deref(), Some("you"));
                assert_eq!(kind_quota.as_deref(), Some("max_rolling_5h"));
            }
            other => panic!("expected RateLimit cause, got {other:?}"),
        }
    }

    #[test]
    fn collapse_returns_not_found_for_unknown_id() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260504-zzzz").unwrap();

        let err = collapse(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            CollapseRequest::for_reason("ghost"),
        )
        .unwrap_err();
        assert!(matches!(err, CollapseError::MoleculeNotFound(_)));
        assert_eq!(err.http_status(), 404);
    }

    #[test]
    fn collapse_error_tags_are_kebab_case() {
        for e in &[
            CollapseError::MoleculeNotFound(MoleculeId::new("task-20260504-aaaa").unwrap()),
            CollapseError::InvalidCause("x".into()),
            CollapseError::MismatchedAccountKind("x".into()),
            CollapseError::AlreadyCompleted(MoleculeId::new("task-20260504-aaaa").unwrap()),
            CollapseError::StoreUnavailable("x".into()),
        ] {
            assert!(
                is_kebab_case(e.tag()),
                "tag must be kebab-case: {}",
                e.tag()
            );
        }
    }

    #[test]
    fn collapse_json_renders_canonical_shape() {
        let view = CollapseView {
            id: MoleculeId::new("task-20260504-aaaa").unwrap(),
            previous_status: MoleculeStatus::Running,
            reason: "blocker".to_owned(),
            cause: None,
            already_collapsed: false,
            previous_was_stuck: false,
        };
        let json = CollapseJson::from_view(&view);
        assert_eq!(json.molecule, "task-20260504-aaaa");
        assert_eq!(json.previous_status, "running");
        assert_eq!(json.status, "collapsed");
        assert_eq!(json.reason, "blocker");
        assert!(!json.already_collapsed);
    }

    #[test]
    fn collapse_after_stuck_renders_previous_status_as_stuck() {
        // Regression: `task-20260509-177e`. After `cs stuck`, the
        // molecule is `Frozen` with `stuck_at = Some(_)`. A subsequent
        // `cs collapse` must render `previous_status: "stuck"` on the
        // wire (and emit `from: "stuck"` in the status-changed event)
        // so the audit trail preserves the operator's gesture.
        let store = FakeStore::default();
        let mut m = mol("task-20260509-stuck", MoleculeStatus::Frozen);
        m.stuck_at = Some(Utc::now());
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260509-stuck").unwrap();

        let view = collapse(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            CollapseRequest::for_reason("after stuck"),
        )
        .unwrap();
        assert_eq!(view.previous_status, MoleculeStatus::Frozen);
        assert!(
            view.previous_was_stuck,
            "stuck-flavored Frozen must propagate to the view"
        );

        let json = CollapseJson::from_view(&view);
        assert_eq!(
            json.previous_status, "stuck",
            "wire `previous_status` must be `stuck` after `cs stuck → cs collapse`"
        );
    }

    #[test]
    fn collapse_after_freeze_renders_previous_status_as_frozen() {
        // Symmetric guard: Frozen reached via `cs freeze` (no
        // `stuck_at`) must continue to render as `"frozen"` — the fix
        // for `task-20260509-177e` must not regress the freeze path.
        let store = FakeStore::default();
        let m = mol("task-20260509-frozn", MoleculeStatus::Frozen);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260509-frozn").unwrap();

        let view = collapse(
            &store,
            tmp.path(),
            &Subject::operator(),
            &id,
            CollapseRequest::for_reason("after freeze"),
        )
        .unwrap();
        assert!(!view.previous_was_stuck);

        let json = CollapseJson::from_view(&view);
        assert_eq!(json.previous_status, "frozen");
    }
}
