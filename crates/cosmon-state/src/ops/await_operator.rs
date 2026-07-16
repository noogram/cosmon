// SPDX-License-Identifier: AGPL-3.0-only

//! `await-operator` — the **only** sanctioned way for a worker to block
//! on an operator decision (ADR-123).
//!
//! This is the worker-emitted half of the operator-block fix. A worker
//! that reaches an *undecidable-AND-irreversible* boundary calls
//! `cs await-operator`, which routes on the molecule's typed capability:
//!
//! - **capability present** (tag `op-block:<boundary>`, granted at
//!   `cs nucleate`) ⇒ **block**: stamp the derived surface marker
//!   [`AWAITING_OP_TAG`] + the boundary's
//!   [`IrreversibleBoundary::alert_tag`], emit
//!   [`EventV2::WorkerBlockedOnOperator`] **before** yielding, and return
//!   [`AwaitOperatorOutcome::Blocked`]. The molecule stays `Running`.
//! - **capability absent** ⇒ **surface-and-continue** (the safe default):
//!   emit nothing, mutate nothing, return
//!   [`AwaitOperatorOutcome::SurfaceAndContinue`]. The generic *"DO NOT
//!   wait"* wins by construction.
//!
//! The capability is read **only** from the molecule's tags (granted at
//! nucleation) — never from a caller-supplied flag — so a worker cannot
//! self-grant the right to block. *Emit, do not infer* (turing, CV-2):
//! the block is an authoritative typed event on the ledger, not a state a
//! patrol has to guess at.
//!
//! Artifact writes (`blocked_on.json`, the surfaced `needs-review`
//! proposal) are the caller's responsibility — this op owns the **state
//! mutation + event**, the data plane is the filesystem (CLAUDE.md
//! "Control plane vs Data plane").

use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};
use cosmon_core::auth::Subject;
use cosmon_core::error::CosmonError;
use cosmon_core::event_v2::EventV2;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::operator_block::{IrreversibleBoundary, OperatorBlockCapability, AWAITING_OP_TAG};
use cosmon_core::tag::Tag;
use serde::Serialize;

use crate::event_log;
use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::StateStore;

/// Errors returned by [`await_operator`].
#[derive(Debug, thiserror::Error)]
pub enum AwaitOperatorError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// Caller supplied no questions — a block must carry the decision the
    /// operator is being asked to make.
    #[error("await-operator request must include at least one question")]
    NoQuestions,
    /// The molecule is in a terminal state — nothing can block on it.
    #[error("molecule {0} is in terminal status `{1}` — cannot await operator")]
    TerminalStatus(MoleculeId, MoleculeStatus),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl AwaitOperatorError {
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for AwaitOperatorError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::NoQuestions => "no-questions",
            Self::TerminalStatus(_, _) => "terminal-status",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::NoQuestions => 400,
            Self::TerminalStatus(_, _) => 409,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Inputs for [`await_operator`].
#[derive(Debug, Clone)]
pub struct AwaitOperatorRequest {
    /// The decision(s) the operator is being asked to make. At least one
    /// is required — a block with no question is meaningless.
    pub questions: Vec<String>,
}

/// What [`await_operator`] resolved to — the routing over ADR-123's
/// matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwaitOperatorOutcome {
    /// The molecule carried the capability: the worker blocked. The typed
    /// signal was emitted and the surface marker tagged **before** the
    /// caller yields.
    Blocked {
        /// The boundary that authorised the pause.
        boundary: IrreversibleBoundary,
        /// Wall-clock time the block was emitted.
        since: DateTime<Utc>,
    },
    /// The molecule carried no capability: the worker must
    /// surface-and-continue. Nothing was emitted or mutated; the caller
    /// writes the options + a recommended default to a surface and keeps
    /// working.
    SurfaceAndContinue,
}

/// Result of a successful [`await_operator`] call.
#[derive(Debug, Clone)]
pub struct AwaitOperatorView {
    /// Molecule the call targeted.
    pub id: MoleculeId,
    /// The questions carried in the request (echoed for artifact writes).
    pub questions: Vec<String>,
    /// The routing outcome.
    pub outcome: AwaitOperatorOutcome,
}

/// Route a worker's request to pause for an operator over ADR-123's
/// capability guard.
///
/// Owns the **state mutation + event emission**; the caller owns artifact
/// writes (`blocked_on.json` / surfaced proposal). Idempotent on the tag
/// set — re-blocking an already-`temp:awaiting-op` molecule re-inserts the
/// same tags (a `BTreeSet`, so a no-op) and re-emits the append-only
/// event (the ledger is append-only by design; a second block is a second
/// honest observation).
///
/// # Errors
///
/// See [`AwaitOperatorError`]: no-questions, molecule-not-found,
/// terminal-status, store-unavailable.
///
/// # No `#[verb]` route
///
/// Unlike `stuck` / `freeze` / `collapse`, this verb carries **no**
/// `#[cosmon_thin_macro::verb]` attribute: it is worker-internal (the
/// same NEVER-on-the-wire class as `cs evolve` / `cs complete`). A block
/// is emitted by a *live worker inside its worktree* via the `cs` CLI;
/// there is no remote-caller use case, so it gets no latent HTTP route.
#[allow(clippy::needless_pass_by_value)]
pub fn await_operator(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    id: &MoleculeId,
    request: AwaitOperatorRequest,
) -> Result<AwaitOperatorView, AwaitOperatorError> {
    let started = Instant::now();
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };

    let questions: Vec<String> = request
        .questions
        .iter()
        .map(|q| q.trim().to_owned())
        .filter(|q| !q.is_empty())
        .collect();
    if questions.is_empty() {
        return Err(AwaitOperatorError::NoQuestions);
    }

    let mol = store
        .load_molecule(id)
        .map_err(|e| AwaitOperatorError::from_cosmon(id, e))?;

    if matches!(
        mol.status,
        MoleculeStatus::Completed | MoleculeStatus::Collapsed
    ) {
        return Err(AwaitOperatorError::TerminalStatus(id.clone(), mol.status));
    }

    // The capability is read ONLY from the tags granted at nucleation —
    // never from a caller flag. A worker cannot self-grant the right to
    // block (ADR-123 Q5).
    let capability = OperatorBlockCapability::from_tags(&mol.tags);

    let outcome = match capability {
        Some(cap) => {
            let boundary = cap.boundary();
            let now = Utc::now();

            // Stamp the derived surface marker + the C1-recognised
            // irreversible-class alert tag (belt-and-suspenders with
            // task-20260608-014f). `BTreeSet` insert is idempotent.
            let mut updated = mol;
            if let Ok(t) = Tag::new(AWAITING_OP_TAG) {
                updated.tags.insert(t);
            }
            if let Ok(t) = Tag::new(boundary.alert_tag()) {
                updated.tags.insert(t);
            }
            updated.updated_at = now;
            store
                .save_molecule(&updated.id.clone(), &updated)
                .map_err(|e| AwaitOperatorError::from_cosmon(id, e))?;

            // Emit the typed block signal BEFORE the caller yields. This
            // is the load-bearing line: the block is emitted, never
            // inferred (CV-2 (i)).
            let events_path = state_dir.join("events.jsonl");
            let _ = event_log::emit_one(
                &events_path,
                EventV2::WorkerBlockedOnOperator {
                    molecule_id: id.clone(),
                    boundary,
                    since: now,
                },
                None,
            );

            AwaitOperatorOutcome::Blocked {
                boundary,
                since: now,
            }
        }
        None => AwaitOperatorOutcome::SurfaceAndContinue,
    };

    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(
        state_dir,
        "await-operator",
        &subject_kind,
        None,
        decision,
        latency_ms,
    );

    Ok(AwaitOperatorView {
        id: id.clone(),
        questions,
        outcome,
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

/// JSON body emitted by `cs --json await-operator` and
/// `POST /v1/molecules/:id/await-operator`.
#[derive(Debug, Clone, Serialize)]
pub struct AwaitOperatorJson {
    /// `"blocked"` or `"surface-and-continue"`.
    pub status: &'static str,
    /// Molecule id (string form).
    pub molecule: String,
    /// The boundary that authorised the pause — `None` when surfacing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<String>,
    /// The questions carried.
    pub questions: Vec<String>,
}

impl AwaitOperatorJson {
    /// Render an [`AwaitOperatorView`] into the canonical JSON wire shape.
    #[must_use]
    pub fn from_view(view: &AwaitOperatorView) -> Self {
        match &view.outcome {
            AwaitOperatorOutcome::Blocked { boundary, .. } => Self {
                status: "blocked",
                molecule: view.id.to_string(),
                boundary: Some(boundary.to_string()),
                questions: view.questions.clone(),
            },
            AwaitOperatorOutcome::SurfaceAndContinue => Self {
                status: "surface-and-continue",
                molecule: view.id.to_string(),
                boundary: None,
                questions: view.questions.clone(),
            },
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

    fn mol(id: &str, status: MoleculeStatus, tags: BTreeSet<Tag>) -> MoleculeData {
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
            tags,
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

    fn run(
        store: &FakeStore,
        dir: &Path,
        id: &str,
        questions: &[&str],
    ) -> Result<AwaitOperatorView, AwaitOperatorError> {
        await_operator(
            store,
            dir,
            &Subject::operator(),
            &MoleculeId::new(id).unwrap(),
            AwaitOperatorRequest {
                questions: questions.iter().map(|s| (*s).to_owned()).collect(),
            },
        )
    }

    /// (a) A capability-bearing block emits the signal and tags the
    /// molecule — the molecule stays `Running`.
    #[test]
    fn capability_bearing_block_emits_and_tags() {
        let store = FakeStore::default();
        let cap = OperatorBlockCapability::new(IrreversibleBoundary::Signature);
        let mut tags = BTreeSet::new();
        tags.insert(cap.to_tag());
        let m = mol("task-20260608-aaaa", MoleculeStatus::Running, tags);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();

        let view = run(&store, tmp.path(), "task-20260608-aaaa", &["Sign the act?"]).unwrap();

        assert!(matches!(
            view.outcome,
            AwaitOperatorOutcome::Blocked {
                boundary: IrreversibleBoundary::Signature,
                ..
            }
        ));

        // The surface marker + the C1 alert tag were stamped; status
        // unchanged (still Running).
        let reloaded = store.load_molecule(&m.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Running);
        assert!(reloaded.tags.contains(&Tag::new(AWAITING_OP_TAG).unwrap()));
        assert!(reloaded.tags.contains(&Tag::new("signature").unwrap()));

        // The typed block signal was appended to the ledger.
        let events = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
        assert!(
            events.contains("worker_blocked_on_operator"),
            "block event must be on the ledger: {events}"
        );
    }

    /// (b) A worker that tries to block WITHOUT the capability
    /// surfaces-and-continues — nothing emitted, nothing tagged.
    #[test]
    fn no_capability_surfaces_and_continues() {
        let store = FakeStore::default();
        let m = mol(
            "task-20260608-bbbb",
            MoleculeStatus::Running,
            BTreeSet::new(),
        );
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();

        let view = run(&store, tmp.path(), "task-20260608-bbbb", &["Pick a name?"]).unwrap();

        assert_eq!(view.outcome, AwaitOperatorOutcome::SurfaceAndContinue);

        // No awaiting-op tag was added.
        let reloaded = store.load_molecule(&m.id).unwrap();
        assert!(!reloaded.tags.contains(&Tag::new(AWAITING_OP_TAG).unwrap()));

        // No block event was emitted (file may not exist at all).
        let events = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap_or_default();
        assert!(
            !events.contains("worker_blocked_on_operator"),
            "surface-and-continue must NOT emit a block: {events}"
        );
    }

    #[test]
    fn empty_questions_is_400() {
        let store = FakeStore::default();
        let cap = OperatorBlockCapability::new(IrreversibleBoundary::Publish);
        let mut tags = BTreeSet::new();
        tags.insert(cap.to_tag());
        let m = mol("task-20260608-cccc", MoleculeStatus::Running, tags);
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();

        let err = run(&store, tmp.path(), "task-20260608-cccc", &["   "]).unwrap_err();
        assert!(matches!(err, AwaitOperatorError::NoQuestions));
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn terminal_molecule_is_409() {
        let store = FakeStore::default();
        let m = mol(
            "task-20260608-dddd",
            MoleculeStatus::Completed,
            BTreeSet::new(),
        );
        store.save_molecule(&m.id, &m).unwrap();
        let tmp = TempDir::new().unwrap();

        let err = run(&store, tmp.path(), "task-20260608-dddd", &["q?"]).unwrap_err();
        assert!(matches!(err, AwaitOperatorError::TerminalStatus(_, _)));
        assert_eq!(err.http_status(), 409);
    }

    #[test]
    fn unknown_molecule_is_not_found() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let err = run(&store, tmp.path(), "task-20260608-zzzz", &["q?"]).unwrap_err();
        assert!(matches!(err, AwaitOperatorError::MoleculeNotFound(_)));
    }

    #[test]
    fn error_tags_are_kebab_case() {
        for e in &[
            AwaitOperatorError::MoleculeNotFound(MoleculeId::new("task-20260608-aaaa").unwrap()),
            AwaitOperatorError::NoQuestions,
            AwaitOperatorError::TerminalStatus(
                MoleculeId::new("task-20260608-aaaa").unwrap(),
                MoleculeStatus::Completed,
            ),
            AwaitOperatorError::StoreUnavailable("x".into()),
        ] {
            assert!(
                is_kebab_case(e.tag()),
                "tag must be kebab-case: {}",
                e.tag()
            );
        }
    }

    #[test]
    fn json_renders_both_outcomes() {
        let blocked = AwaitOperatorView {
            id: MoleculeId::new("task-20260608-aaaa").unwrap(),
            questions: vec!["q?".to_owned()],
            outcome: AwaitOperatorOutcome::Blocked {
                boundary: IrreversibleBoundary::Signature,
                since: Utc::now(),
            },
        };
        let j = AwaitOperatorJson::from_view(&blocked);
        assert_eq!(j.status, "blocked");
        assert_eq!(j.boundary.as_deref(), Some("signature"));

        let surfaced = AwaitOperatorView {
            id: MoleculeId::new("task-20260608-aaaa").unwrap(),
            questions: vec!["q?".to_owned()],
            outcome: AwaitOperatorOutcome::SurfaceAndContinue,
        };
        let j = AwaitOperatorJson::from_view(&surfaced);
        assert_eq!(j.status, "surface-and-continue");
        assert_eq!(j.boundary, None);
    }
}
