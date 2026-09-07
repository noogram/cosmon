// SPDX-License-Identifier: AGPL-3.0-only

//! `molecule_status` — the cheapest honest answer about one molecule.
//!
//! # Why a verb of its own, next to `observe`
//!
//! A client that waits on a molecule asks the same question every few
//! seconds: *has it moved, and is it over?* [`observe`](fn@super::observe)
//! answers it, but it answers a great deal more on the way — a coupling
//! report, the per-molecule token totals, and the model attribution, each
//! of which scans an append-only log whose length grows with the
//! molecule's lifetime. A poller therefore pays *more* per poll the
//! longer it waits, which is exactly backwards.
//!
//! This verb reads one file, `state.json`, through
//! [`StateStore::load_molecule`], and projects four fields out of it.
//! That it *cannot* do more is structural rather than disciplined: the
//! signature takes no `state_dir`, so the three log scans are not merely
//! skipped here, they are unreachable. Adding one back would mean
//! widening the signature, which is a change a reviewer sees.
//!
//! # What the four fields are for
//!
//! `status` and `updated_at` are the poll's payload. `phase` is the
//! band ([`Phase`]) the status belongs to, carried so a client can render
//! progress without owning a copy of the status→phase table. `terminal`
//! answers *is it over* directly, so a client polling for the default
//! terminal set does not have to hard-code `{completed, collapsed}` and
//! then be wrong on the day the set changes.
//!
//! # The validator
//!
//! [`StatusView::validator`] is the whole answer, compressed: two
//! molecules with the same `(status, updated_at)` yield the same answer
//! but not the same *bytes* (a per-request id rides the envelope over
//! HTTP), which is why the transport that carries this projects it as a
//! **weak** entity-tag. See `cosmon_rpp_adapter::routes::status`.

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::{MoleculeStatus, Phase};
use serde::Serialize;

use crate::ops::error::OpsError;
use crate::StateStore;
use cosmon_core::error::CosmonError;

/// Errors returned by [`molecule_status`](fn@molecule_status).
///
/// Deliberately the same two shapes [`ObserveError`](super::ObserveError)
/// has — a status read fails for exactly the reasons a molecule read
/// fails, and a caller that already maps one maps the other with no new
/// arm. `#[non_exhaustive]` for the same semver reason.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// The state store could not be read.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for StatusError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// The projection: everything a poller needs and nothing that costs a
/// second file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusView {
    /// Molecule id, echoed so a batching client can key on the answer.
    pub id: MoleculeId,
    /// Lifecycle status.
    pub status: MoleculeStatus,
    /// The band `status` belongs to — a derivation, never stored.
    pub phase: Phase,
    /// Last write to the molecule's state, RFC3339 in the wire form.
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Whether `status` is terminal. Derived from
    /// [`MoleculeStatus::is_terminal`], so a client never restates the set.
    pub terminal: bool,
}

impl StatusView {
    /// Project a view from an already-loaded molecule.
    ///
    /// Split out so a caller holding a snapshot (a list walk, a test
    /// fixture) does not pay a second `load_molecule` — the same reason
    /// [`observe_loaded`](super::observe_loaded) exists.
    #[must_use]
    pub fn from_data(data: &crate::MoleculeData) -> Self {
        Self {
            id: data.id.clone(),
            status: data.status,
            phase: data.status.phase(),
            updated_at: data.updated_at,
            terminal: data.status.is_terminal(),
        }
    }

    /// The whole answer as one opaque token: `<status>:<updated_at>`.
    ///
    /// Every other field is a pure function of these two, so two views
    /// with equal validators are equal views. A transport turns this into
    /// a conditional-request tag; nothing else may parse it — its shape is
    /// this function's business and may change.
    #[must_use]
    pub fn validator(&self) -> String {
        format!("{}:{}", self.status, self.updated_at.to_rfc3339())
    }
}

/// Wire projection of [`StatusView`], with the field names and the string
/// forms external clients parse.
///
/// A separate type from the view for the same reason
/// [`ObserveJson`](super::ObserveJson) is: the view is free to hold typed
/// values, the JSON is a frozen contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusJson {
    /// Molecule id (string form).
    pub id: String,
    /// Status (`snake_case`, `MoleculeStatus`'s serde repr).
    pub status: String,
    /// Phase band (`live` · `waiting` · `blocked` · `parked` · `failed` · `done`).
    pub phase: String,
    /// Last state write, RFC3339.
    pub updated_at: String,
    /// Whether the molecule is over.
    pub terminal: bool,
}

impl StatusJson {
    /// Render a view onto the wire shape.
    #[must_use]
    pub fn from_view(view: &StatusView) -> Self {
        Self {
            id: view.id.as_str().to_owned(),
            status: view.status.to_string(),
            phase: view.phase.to_string(),
            updated_at: view.updated_at.to_rfc3339(),
            terminal: view.terminal,
        }
    }
}

/// Read one molecule's status.
///
/// Exactly one store read, and no `state_dir` in scope to read anything
/// else from — see the module docs for why that is the point.
///
/// # Errors
///
/// [`StatusError::MoleculeNotFound`] when no such molecule exists in the
/// store, [`StatusError::StoreUnavailable`] for every other read failure.
pub fn molecule_status(store: &dyn StateStore, id: &MoleculeId) -> Result<StatusView, StatusError> {
    let data = store.load_molecule(id).map_err(|e| match e {
        CosmonError::MoleculeNotFound(_) => StatusError::MoleculeNotFound(id.clone()),
        other => StatusError::StoreUnavailable(other.to_string()),
    })?;
    Ok(StatusView::from_data(&data))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fixture through the persisted form rather than a 50-field
    /// literal: this projection reads five fields, and a struct literal
    /// here would need editing every time an unrelated field joins
    /// `MoleculeData` — which is how a test stops being read.
    fn data(status: MoleculeStatus) -> crate::MoleculeData {
        let raw = serde_json::json!({
            "id": "task-20260907-b25f",
            "fleet_id": "default",
            "formula_id": "task-work",
            "status": status,
            "created_at": "2026-09-07T10:00:00Z",
            "updated_at": "2026-09-07T12:34:56Z",
            "total_steps": 2,
            "current_step": 1,
            "variables": {},
            "completed_steps": [],
            "links": [],
        });
        serde_json::from_value(raw).expect("the minimal persisted form deserialises")
    }

    #[test]
    fn terminal_is_the_core_set_not_a_local_copy() {
        for s in MoleculeStatus::ALL {
            assert_eq!(
                StatusView::from_data(&data(s)).terminal,
                s.is_terminal(),
                "{s} must not disagree with the core"
            );
        }
    }

    #[test]
    fn phase_is_the_core_band() {
        for s in MoleculeStatus::ALL {
            assert_eq!(StatusView::from_data(&data(s)).phase, s.phase());
        }
    }

    /// The validator is the answer: two views agree on it exactly when
    /// they agree on every field. This is what makes a conditional poll
    /// sound — a `304` on an equal validator can never hide a moved
    /// molecule.
    #[test]
    fn equal_validators_mean_equal_views() {
        let a = StatusView::from_data(&data(MoleculeStatus::Running));
        let mut b_data = data(MoleculeStatus::Running);
        b_data.updated_at = a.updated_at;
        let b = StatusView::from_data(&b_data);
        assert_eq!(a.validator(), b.validator());
        assert_eq!(a, b);

        let mut moved = data(MoleculeStatus::Completed);
        moved.updated_at = a.updated_at;
        assert_ne!(a.validator(), StatusView::from_data(&moved).validator());
    }

    #[test]
    fn wire_shape_has_exactly_the_five_fields() {
        let json = serde_json::to_value(StatusJson::from_view(&StatusView::from_data(&data(
            MoleculeStatus::Completed,
        ))))
        .expect("StatusJson serialises");
        let obj = json.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["id", "phase", "status", "terminal", "updated_at"],
            "a field added here is a field the poller pays for on every poll"
        );
    }
}
