// SPDX-License-Identifier: AGPL-3.0-only

//! `observe` — read-only inspection of a molecule.
//!
//! First verb extracted under `cosmon_state::ops`. The `cs observe <id>`
//! CLI handler and the cs-api `GET /molecules/{id}` route both call
//! [`observe`] instead of shelling out to a child process — that is the
//! library-first promotion.
//!
//! `observe` is chosen as the first verb because it is read-only: there
//! is no double-writer hazard, the blast radius of a bug is bounded to
//! "wrong JSON returned to the caller", and the operation already lives
//! at the boundary of the state store today (`load_molecule` +
//! `coupling_report_snapshot` + `detect_ghost`). The cs-cli renderer
//! reproduces the wire format byte-for-byte via [`ObserveJson`] so
//! existing scripts that parse `cs --json observe` keep working.

use std::path::Path;
use std::time::Instant;

use cosmon_core::auth::Subject;
use cosmon_core::id::MoleculeId;
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::{CollapseCause, MoleculeStatus};
use cosmon_core::run_state::{project_run_state, GhostKind};
use cosmon_core::worker::TransportState;
use serde::Serialize;

use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::wait::{coupling_report_snapshot, EnergyMetrics, EntropyMetrics, WaitMetrics};
use crate::{EscalationEntry, MoleculeData, StateStore};
use cosmon_core::error::CosmonError;

/// Probe-freshness window used by [`detect_ghost`].
///
/// Same 90 s default as `cs ensemble` / cs-cli's observe handler — see
/// ADR-052 §D2 for the patrol-vs-hook split.
pub const GHOST_PROBE_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// Errors returned by [`observe`].
///
/// Dedicated to the verb on purpose: we do **not** flatten through a
/// mega-`CosmonError` because callers (cs-cli, cs-api, future MCP) want
/// to map specific failures to specific surfaces (HTTP 404 vs 500, exit
/// code 1 vs 2). The [`OpsError`] impl below carries the kebab-case wire
/// tag and HTTP status — RPP and cs-api dispatch on those, never on the
/// `Display` string.
///
/// `#[non_exhaustive]` — V1 will add a `ByokRequired` variant for the
/// backend-bound bring-your-own-key flow (see
/// `cosmon_core::auth::TenantApiKey`); a minor bump must remain
/// non-breaking for downstream `match` sites in cs-cli, cs-api,
/// cosmon-rpp-adapter.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ObserveError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// The state store could not be read (I/O failure, lock contention,
    /// schema mismatch). Maps to HTTP 503 — the request is well-formed,
    /// the substrate is just unavailable right now.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl ObserveError {
    /// Translate a [`CosmonError`] into the verb-specific error variant
    /// while keeping the rendered message stable.
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for ObserveError {
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

/// In-memory view of a molecule, returned by [`observe`].
///
/// Carries the persisted data plus the two derived fields the cs-cli
/// has historically printed: a coupling-report snapshot (THESIS Part XVIII)
/// and an ADR-052 ghost detection. A view is a *projection* — there is
/// no I/O behind it, the renderer can serialize it freely.
#[derive(Debug, Clone)]
pub struct MoleculeView {
    /// Persisted molecule fields straight from the store.
    pub data: MoleculeData,
    /// Coupling report — five scalars, frozen surface (THESIS Part XVIII).
    pub metrics: WaitMetrics,
    /// Ghost detection (ADR-052). `None` when the run-state is
    /// internally consistent.
    pub ghost: Option<GhostKind>,
    /// Per-molecule API token totals, summed from the canonical
    /// `{state_dir}/instrumentation/tokens.jsonl` sink keyed by
    /// `molecule_id`. `None` when no LLM call was recorded for the
    /// molecule (omit-if-none). This is **distinct** from `metrics.energy`:
    /// the coupling-report energy reads the legacy `log/energy.jsonl`,
    /// while this reads the IFBDD token-meter that carries `molecule_id`
    /// on every event (the source the RPP "Dave" instance writes to).
    pub api_tokens: Option<crate::token_meter::MoleculeTokenTotals>,
    /// Model attribution for this molecule (delib-20260704-b476 C3),
    /// projected from the latest `ModelSelected` event on `events.jsonl`.
    /// `None` for a legacy or never-tackled molecule with no recorded
    /// selection. Surfaces the `(adapter, model, source)` bundle so
    /// `cs observe` answers "which model ran, and why?" at a glance.
    pub model: Option<crate::ops::model_attribution::ModelAttribution>,
}

// ---------------------------------------------------------------------------
// ObserveResponse — wire-shaped envelope for /v1/molecules/:id
// ---------------------------------------------------------------------------

/// Per-response measurement scalars surfaced to the caller.
///
/// The fields are deliberately drafted V0-thin: the runtime today only
/// has cheap clock-based latency and the backend tag. V1 adds
/// `cost_micros` (BYOK billing), V2 may add `cache_hits`, etc. — every
/// addition stays a minor bump because the struct is `#[non_exhaustive]`.
///
/// # Why `Option` everywhere
///
/// `cost_micros` and `backend` are `None` in V0 because the wire
/// adapters that produce them (BYOK billing, multi-backend router)
/// have not landed yet. Once they do, populating the slot is a *patch*
/// to the producer, not a breaking change to consumers. (tolnay IS4,
/// delib §10.)
#[non_exhaustive]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResponseMetrics {
    /// Cost charged to the tenant, in millionths of a USD.
    ///
    /// `None` in V0; `Some(n)` becomes available the moment BYOK +
    /// per-tenant accounting lands. Filling the slot is a producer
    /// patch — consumers never need to widen their match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
    /// Wall-clock latency of the read, in milliseconds.
    pub latency_ms: u64,
    /// Provider tag of the backend that served the request, if known.
    ///
    /// `None` for in-process state reads (no LLM hop). `Some` for
    /// LLM-augmented verbs once they land.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

impl ResponseMetrics {
    /// V0 helper — only the latency is observable, the rest is `None`.
    #[must_use]
    pub fn v0(latency_ms: u64) -> Self {
        Self {
            cost_micros: None,
            latency_ms,
            backend: None,
        }
    }
}

/// Wire-shaped envelope returned from `/v1/molecules/:id`.
///
/// Wraps a [`MoleculeView`] (or its byte-stable [`ObserveJson`]
/// projection) with [`ResponseMetrics`] and a free-form `warnings`
/// list. The envelope is `#[non_exhaustive]` so V1's `pagination`,
/// `next_cursor`, etc. can land as minor bumps.
///
/// # Why `warnings` is a `Vec<String>` rather than typed
///
/// V0 emits human-readable strings — a probe-stale notice, a
/// degraded-mode marker, etc. The taxonomy is not yet stable enough
/// to seal as an enum; once it is, a future `WarningCode` enum will
/// be added alongside `warnings` (not replacing it), and the seal
/// hardens.
///
/// # Wire form
///
/// ```json
/// {
///   "molecule": { /* ObserveJson */ },
///   "metrics": { "latency_ms": 4 },
///   "warnings": []
/// }
/// ```
///
/// The `molecule` field is the byte-stable [`ObserveJson`] projection
/// the cs-cli already prints. Adapters that previously returned a
/// flat `ObserveJson` (cs-api `GET /molecules/{id}` and the
/// cosmon-rpp-adapter `GET /v1/molecules/:id`) wrap it into this
/// envelope; existing scripts that read `body["molecule"]["id"]` keep
/// working.
#[non_exhaustive]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ObserveResponse {
    /// The byte-stable molecule projection.
    pub molecule: ObserveJson,
    /// Per-response measurement scalars.
    pub metrics: ResponseMetrics,
    /// Human-readable advisory messages.
    pub warnings: Vec<String>,
}

impl ObserveResponse {
    /// Construct an envelope from a [`MoleculeView`] plus a
    /// stringified molecule directory (passed through to
    /// [`ObserveJson::from_view`]) and a measured latency.
    #[must_use]
    pub fn from_view(view: &MoleculeView, molecule_dir: &str, latency_ms: u64) -> Self {
        Self {
            molecule: ObserveJson::from_view(view, molecule_dir),
            metrics: ResponseMetrics::v0(latency_ms),
            warnings: Vec::new(),
        }
    }
}

/// Read a molecule from the store and project the read-only view.
///
/// `subject` is the typed [`Subject`] carrying the acting Nucléon's
/// id and scopes.
/// In V0 the verb-side check is trivial — the operator subject
/// (`Subject::operator()`) is granted, every other subject yields
/// `Absent` until the scope-by-verb grid lands (T-RPP-V0). The
/// instrumentation derives a stable `subject_kind` label
/// (`"operator"` or `"jwt:<sub>"`) for the
/// [`AuthzDecisionEvaluated`](crate::instrumentation::AuthzDecisionEvaluated)
/// event emitted at every call.
///
/// # Errors
///
/// Returns [`ObserveError::MoleculeNotFound`] when no molecule has the
/// given ID in the store, and [`ObserveError::StoreUnavailable`] for any
/// other I/O failure surfaced by the underlying [`StateStore`].
///
/// # Why a `state_dir` parameter
///
/// The energy aggregation lives at `{state_dir}/log/energy.jsonl` —
/// outside the [`StateStore`] trait, which only knows about the molecule
/// document itself. The caller (cs-cli, cs-api) already has a path
/// handy, so threading it through is the smallest possible coupling.
/// A future trait method would be a strictly internal refactor.
///
/// # `#[verb]` registration
///
/// The annotation registers `observe` in the `cs-thin` verb registry as
/// `GET /v1/molecules/:id` (principal `tenant`), so that the mechanical
/// HTTP client surfaces this verb in `cs-thin verbs --check` and the
/// `api_surface_freeze` test can assert the §8p subset matches the
/// rpp-adapter routes byte-for-byte. The macro emits an `ObserveVerb`
/// marker — the function itself is unchanged.
#[cosmon_thin_macro::verb(method = "GET", path = "/v1/molecules/:id", principal = "tenant")]
pub fn observe(
    store: &dyn StateStore,
    state_dir: &Path,
    subject: &Subject,
    id: &MoleculeId,
) -> Result<MoleculeView, ObserveError> {
    let data = store
        .load_molecule(id)
        .map_err(|e| ObserveError::from_cosmon(id, e))?;
    Ok(observe_loaded(data, state_dir, subject))
}

/// Project a [`MoleculeView`] from an already-loaded [`MoleculeData`].
///
/// This is the cs-cli's prefix-resolution fast path: the resolver
/// already paid for a `list_molecules` walk, so a second `load_molecule`
/// inside [`observe`] would double-read the same JSON. Callers that
/// need to thread an existing snapshot reach for this helper instead.
///
/// Both this function and [`observe`] emit a single
/// [`AuthzDecisionEvaluated`](crate::instrumentation::AuthzDecisionEvaluated)
/// — emitting here covers both entry points without double-recording.
#[must_use]
pub fn observe_loaded(data: MoleculeData, state_dir: &Path, subject: &Subject) -> MoleculeView {
    let started = Instant::now();
    // V0 trivial check: the operator subject (id == "operator", carries
    // the wildcard scope by construction) is implicitly granted; every
    // other subject yields `Absent` so the instrumentation surfaces the
    // missing rule. The future `check_authz` (T-RPP-V0) replaces this
    // with a real scope-by-verb grid that consults `subject.scopes`.
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(
        state_dir,
        "observe",
        &subject_kind,
        None,
        decision,
        latency_ms,
    );

    let metrics = coupling_report_snapshot(state_dir, &data.id);
    let ghost = detect_ghost(&data);
    let api_tokens = crate::token_meter::molecule_token_totals(state_dir, &data.id);
    // delib-20260704-b476 C3: fold the `ModelSelected` event log into a
    // per-molecule attribution so the detail view surfaces the model + its
    // source. Advisory read (trace-not-lock): `None` when nothing was
    // recorded.
    let model = crate::ops::model_attribution::latest_model_selection(state_dir, &data.id);
    MoleculeView {
        data,
        metrics,
        ghost,
        api_tokens,
        model,
    }
}

/// Stringly-typed instrumentation label for a [`Subject`].
///
/// V0 vocabulary preserved across the T-SUBJECT migration:
/// `"operator"` for the trusted CLI subject, `"jwt:<sub>"` for every
/// other Nucléon (which today only reach the boundary through a future
/// JWT-bearing transport). The label flows into
/// [`AuthzDecisionEvaluated::subject_kind`](crate::instrumentation::AuthzDecisionEvaluated)
/// so external scrapers see the same field shape they did before
/// T-RECTIFY.
fn derive_subject_kind(subject: &Subject) -> String {
    if subject.id().as_str() == "operator" {
        "operator".to_owned()
    } else {
        format!("jwt:{}", subject.id().as_str())
    }
}

/// Detect a ghost from a snapshot of the molecule, without re-probing
/// transport.
///
/// `cs observe` does not re-probe tmux on every invocation — it is a
/// snapshot of the persisted state plus cheap disk reads. When no
/// liveness observation is available, `transport` defaults to `Unknown`
/// in the run-state projection. The detection logic still catches the
/// I5 (`UnHarvested`), I9 (`UnnamedMerge`), and I3 (`VanishedWorker`)
/// shapes because those do not depend on a fresh liveness reading.
/// `DeadPane` (I4) and `StaleProbe` (I10) are out of reach from a single
/// snapshot.
#[must_use]
pub fn detect_ghost(mol: &MoleculeData) -> Option<GhostKind> {
    // Invariant `archived ⇒ status.is_terminal()` (idea-20260618-1b10):
    // an archived molecule has been carried off the shelf and cannot be a
    // *live* ghost, whatever its (possibly stale, pre-fix) `status` says.
    // This heals every legacy `{archived: true, status: Running}` row already
    // on disk (e.g. `task-20260418-d0c4` in sandbox) with zero
    // migration — the row stays as written but no longer renders as a ghost.
    // Defense-in-depth alongside the writer fix in `cs done --force`.
    if mol.archived {
        return None;
    }

    // ADR-062 — `QuotaExhausted` is not derivable from the run-state
    // alone; we read it off the persisted `MoleculeStatus::Starved`
    // marker or the structured `CollapseCause::RateLimit`.
    if mol.status == MoleculeStatus::Starved {
        return Some(GhostKind::QuotaExhausted);
    }
    if matches!(mol.collapse_cause, Some(CollapseCause::RateLimit { .. })) {
        return Some(GhostKind::QuotaExhausted);
    }

    let rs = project_run_state(
        mol.status,
        TransportState::Unknown,
        mol.merged_at,
        chrono::Utc::now(),
    );
    rs.ghost(chrono::Utc::now(), GHOST_PROBE_TTL)
}

// ---------------------------------------------------------------------------
// Wire-format renderer — keeps the byte layout consumed by external
// scripts stable across cs-cli and cs-api.
// ---------------------------------------------------------------------------

/// JSON body emitted by `cs observe <id> --json` and `GET /molecules/{id}`.
///
/// Carries the molecule's structural state plus the **coupling report**
/// (THESIS Part XVIII): `poll_count` + `transitions` are always present,
/// while `energy`, `entropy`, `temperature` follow the same omit-if-none
/// discipline as `cs wait`. Identical wire format to `cs wait`'s metrics
/// block so agents and humans use one vocabulary for both verbs.
///
/// `molecule_dir` is supplied by the caller because the trait
/// [`StateStore`] does not expose it; the file-backed adapter computes
/// it via `FileStore::molecule_dir`, the cs-api passes the resolved
/// state dir.
#[derive(Debug, Clone, Serialize)]
pub struct ObserveJson {
    /// Molecule id (string form).
    pub id: String,
    /// Formula id (string form).
    pub formula: String,
    /// Status (string form).
    pub status: String,
    /// Current step index.
    pub current_step: usize,
    /// Total number of steps.
    pub total_steps: usize,
    /// Names of completed steps in evolution order.
    pub completed_steps: Vec<String>,
    /// Assigned worker, when any.
    pub worker: Option<String>,
    /// Actor class holding the dispatch claim — `"human"` or
    /// `"runtime:<pid>"` (anti-preemption lease).
    ///
    /// Read by the resident runtime's pre-dispatch re-read
    /// (`cs run` → `recheck_tackle_candidate`) to enforce "manual always
    /// wins". Skipped from the wire for legacy / never-tackled molecules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tackled_by: Option<String>,
    /// Variables stamped at nucleation.
    pub variables: std::collections::BTreeMap<String, String>,
    /// Free-form links (string form).
    pub links: Vec<String>,
    /// Typed DAG edges (`blocked_by`, `blocks`, …) — emitted with the
    /// `MoleculeLink` wire shape so downstream readers don't re-parse
    /// `state.json`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub typed_links: Vec<MoleculeLink>,
    /// Tags (string form, deterministic order).
    pub tags: Vec<String>,
    /// Escalation audit trail.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<EscalationEntry>,
    /// Created-at RFC3339.
    pub created_at: String,
    /// Updated-at RFC3339.
    pub updated_at: String,
    /// Last observable forward motion. Skipped when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<String>,
    /// `cs patrol --nudge` counter. Skipped when zero.
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub nudge_count: u32,
    /// Free-form collapse reason, when collapsed.
    pub collapse_reason: Option<String>,
    /// Step at which the molecule collapsed, when collapsed.
    pub collapsed_step: Option<usize>,
    /// Resolved on-disk directory of the molecule (caller-supplied).
    pub molecule_dir: String,
    /// Always `1` for a single-snapshot read.
    pub poll_count: u32,
    /// Always `0` for a single-snapshot read.
    pub transitions: u32,
    /// Energy aggregate. Skipped when no energy log matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy: Option<EnergyMetrics>,
    /// Entropy aggregate. Reserved for a future probe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entropy: Option<EntropyMetrics>,
    /// Last sampled temperature. Reserved for a future probe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Per-molecule API token totals (`tokens_in` / `tokens_out` /
    /// `cost_micros_estimated` / `invocations`) summed from the
    /// canonical token-meter sink keyed by `molecule_id`. Skipped when
    /// no LLM call was recorded for the molecule. This is the
    /// per-`molecule_id` token tracking surfaced to `cs observe` and to
    /// the RPP `GET /v1/molecules/:id` read the tenant ("Dave")
    /// consumes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_tokens: Option<crate::token_meter::MoleculeTokenTotals>,
    /// ADR-052 ghost marker — string form of [`GhostKind`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ghost: Option<String>,
    /// Resolved model id pinned for this molecule (delib-20260704-b476 C3).
    /// `None` at the von-neumann floor (no pin → adapter default applies) or
    /// when no `ModelSelected` event was recorded. Distinguish the two via
    /// [`model_source`](Self::model_source): a floor selection carries
    /// `Some("default")` there, an absent one carries `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Stable slug of where the model choice came from — `flag` /
    /// `formula_pin` / `env_var` / `config` / `global_config` / `default`.
    /// `None` when no `ModelSelected` event was recorded for the molecule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_source: Option<String>,
    /// Adapter the model id is scoped to (a model id only has meaning inside
    /// its adapter). `None` when no `ModelSelected` event was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_adapter: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl ObserveJson {
    /// Render a [`MoleculeView`] into the canonical JSON wire shape.
    ///
    /// The `molecule_dir` parameter is threaded explicitly because the
    /// trait [`StateStore`] does not expose a path; cs-cli passes the
    /// `FileStore::molecule_dir(...)` it already computed, cs-api can
    /// pass an empty string (the iOS pilot does not consume the field
    /// today).
    #[must_use]
    pub fn from_view(view: &MoleculeView, molecule_dir: &str) -> Self {
        let mol = &view.data;
        Self {
            id: mol.id.to_string(),
            formula: mol.formula_id.to_string(),
            status: mol.status.to_string(),
            current_step: mol.current_step,
            total_steps: mol.total_steps,
            completed_steps: mol
                .completed_steps
                .iter()
                .map(ToString::to_string)
                .collect(),
            worker: mol.assigned_worker.as_ref().map(ToString::to_string),
            tackled_by: mol.tackled_by.as_ref().map(ToString::to_string),
            variables: mol
                .variables
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            links: mol.links.clone(),
            typed_links: mol.typed_links.clone(),
            tags: mol.tags.iter().map(ToString::to_string).collect(),
            escalations: mol.escalations.clone(),
            created_at: mol.created_at.to_rfc3339(),
            updated_at: mol.updated_at.to_rfc3339(),
            last_progress_at: mol.last_progress_at.map(|t| t.to_rfc3339()),
            nudge_count: mol.nudge_count,
            collapse_reason: mol.collapse_reason.clone(),
            collapsed_step: mol.collapsed_step,
            molecule_dir: molecule_dir.to_owned(),
            poll_count: view.metrics.poll_count,
            transitions: view.metrics.transitions,
            energy: view.metrics.energy.clone(),
            entropy: view.metrics.entropy.clone(),
            temperature: view.metrics.temperature,
            api_tokens: view.api_tokens,
            ghost: view.ghost.map(|g| g.as_str().to_owned()),
            model: view.model.as_ref().and_then(|m| m.model.clone()),
            model_source: view.model.as_ref().map(|m| m.source_slug().to_owned()),
            model_adapter: view.model.as_ref().map(|m| m.adapter_name.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, StepId, WorkerId};
    use tempfile::TempDir;

    use super::*;
    use crate::MoleculeFilter;

    /// Fake [`StateStore`] backed by an in-memory `HashMap`.
    ///
    /// Lets tests exercise [`observe`] without any filesystem I/O — the
    /// canonical "no `cs` installed" smoke test described in the T2
    /// prompt.
    #[derive(Default)]
    struct FakeStore {
        molecules: std::sync::Mutex<HashMap<MoleculeId, MoleculeData>>,
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
            _filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, CosmonError> {
            Ok(self.molecules.lock().unwrap().values().cloned().collect())
        }
    }

    fn make_molecule(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: Some(WorkerId::new("ruby").unwrap()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 1,
            completed_steps: vec![StepId::new("implement").unwrap()],
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: BTreeSet::new(),
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
            adapter: None,
        }
    }

    #[test]
    fn observe_returns_view_for_known_molecule() {
        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-aaaa", MoleculeStatus::Running));
        let tmp = TempDir::new().unwrap();

        let id = MoleculeId::new("task-20260503-aaaa").unwrap();
        let view = observe(&store, tmp.path(), &Subject::operator(), &id).unwrap();
        assert_eq!(view.data.id, id);
        assert_eq!(view.data.status, MoleculeStatus::Running);
        // Single-snapshot read invariants.
        assert_eq!(view.metrics.poll_count, 1);
        assert_eq!(view.metrics.transitions, 0);
        assert!(view.metrics.energy.is_none());
        assert!(view.ghost.is_none());
    }

    #[test]
    fn detect_ghost_returns_none_when_archived() {
        // The reproduction shape (idea-20260618-1b10): a molecule that never
        // left `Running` but carries `merged_at` — i.e. the legacy
        // `{archived: true, status: Running, merged_at: Some}` row. Without the
        // archived short-circuit this projects as a permanent `UnnamedMerge`
        // ghost. With it, an archived molecule is never a live ghost.
        let mut mol = make_molecule("task-20260418-d0c4", MoleculeStatus::Running);
        mol.merged_at = Some(Utc::now());

        // Pre-fix shape: alive + merged → UnnamedMerge ghost.
        assert_eq!(detect_ghost(&mol), Some(GhostKind::UnnamedMerge));

        // Archived heals it on read, no migration.
        mol.archived = true;
        assert_eq!(detect_ghost(&mol), None);
    }

    #[test]
    fn observe_returns_not_found_for_unknown_id() {
        let store = FakeStore::default();
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-zzzz").unwrap();
        let err = observe(&store, tmp.path(), &Subject::operator(), &id).unwrap_err();
        assert!(matches!(err, ObserveError::MoleculeNotFound(_)));
    }

    #[test]
    fn observe_error_tags_are_stable_kebab_case() {
        use crate::ops::error::is_kebab_case;
        let nf = ObserveError::MoleculeNotFound(MoleculeId::new("task-20260503-aaaa").unwrap());
        let su = ObserveError::StoreUnavailable("backing fs gone".to_string());

        assert_eq!(nf.tag(), "molecule-not-found");
        assert_eq!(su.tag(), "store-unavailable");
        assert!(is_kebab_case(nf.tag()));
        assert!(is_kebab_case(su.tag()));
    }

    #[test]
    fn observe_error_http_status_mapping() {
        let nf = ObserveError::MoleculeNotFound(MoleculeId::new("task-20260503-aaaa").unwrap());
        let su = ObserveError::StoreUnavailable("backing fs gone".to_string());
        assert_eq!(nf.http_status(), 404);
        assert_eq!(su.http_status(), 503);
    }

    #[test]
    fn observe_error_to_wire_round_trips_tag_and_status() {
        let nf = ObserveError::MoleculeNotFound(MoleculeId::new("task-20260503-aaaa").unwrap());
        let wire = nf.to_wire();
        assert_eq!(wire.tag, "molecule-not-found");
        assert_eq!(wire.http_status, 404);
        assert!(wire.message.contains("task-20260503-aaaa"));

        // Round-trip through JSON — the wire form is what RPP emits.
        let s = serde_json::to_string(&wire).unwrap();
        let back: crate::ops::ErrorWire = serde_json::from_str(&s).unwrap();
        assert_eq!(back, wire);
    }

    #[test]
    fn observe_flags_ghost_for_starved_molecule() {
        let store = FakeStore::default();
        let mol = make_molecule("task-20260503-bbbb", MoleculeStatus::Starved);
        store.insert(mol);
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-bbbb").unwrap();

        let view = observe(&store, tmp.path(), &Subject::operator(), &id).unwrap();
        assert_eq!(view.ghost, Some(GhostKind::QuotaExhausted));
    }

    #[test]
    fn observe_json_roundtrips_view_fields() {
        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-cccc", MoleculeStatus::Running));
        let tmp = TempDir::new().unwrap();
        let id = MoleculeId::new("task-20260503-cccc").unwrap();

        let view = observe(&store, tmp.path(), &Subject::operator(), &id).unwrap();
        let json = ObserveJson::from_view(&view, "/tmp/fake/molecule/dir");
        assert_eq!(json.id, "task-20260503-cccc");
        assert_eq!(json.status, "running");
        assert_eq!(json.formula, "task-work");
        assert_eq!(json.current_step, 1);
        assert_eq!(json.total_steps, 2);
        assert_eq!(json.poll_count, 1);
        assert_eq!(json.transitions, 0);
        assert_eq!(json.molecule_dir, "/tmp/fake/molecule/dir");
        assert!(json.energy.is_none());
        assert!(json.ghost.is_none());

        // Wire format is byte-stable.
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains("\"id\":\"task-20260503-cccc\""));
        assert!(s.contains("\"poll_count\":1"));
    }

    /// T-AUTHZ-INSTR integration test — observing a molecule emits
    /// exactly one [`AuthzDecisionEvaluated`](crate::instrumentation::AuthzDecisionEvaluated)
    /// to `{state_dir}/instrumentation/authz.jsonl` with
    /// `decision=Allow, subject_kind=operator` for the trusted CLI
    /// subject. Mirrors the smoke shape requested in the briefing.
    #[test]
    fn observe_emits_authz_decision_for_operator() {
        use crate::instrumentation::{
            read_authz_ndjson, AuthzDecision, AUTHZ_NDJSON_RELATIVE_PATH,
        };

        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-dddd", MoleculeStatus::Running));
        let tmp = TempDir::new().unwrap();
        // Make sure the env-var override is not interfering.
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

        let id = MoleculeId::new("task-20260503-dddd").unwrap();
        let _view = observe(&store, tmp.path(), &Subject::operator(), &id).unwrap();

        let path = tmp.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
        let events = read_authz_ndjson(&path).unwrap();
        assert_eq!(events.len(), 1, "expected one authz event, got {events:?}");
        let ev = &events[0];
        assert_eq!(ev.verb, "observe");
        assert_eq!(ev.subject_kind, "operator");
        assert_eq!(ev.decision, AuthzDecision::Allow);
        assert!(ev.scope_required.is_none());
    }

    /// Non-operator subjects yield `Absent` in V0 — the grid is not
    /// figée, so the JWT vocabulary captures the pattern without
    /// granting or denying anything. The `subject_kind` label is
    /// derived as `"jwt:<sub>"` from the typed [`Subject`] so the wire
    /// shape consumed by external scrapers stays stable across the
    /// T-RECTIFY migration.
    #[test]
    fn observe_emits_absent_for_jwt_subject() {
        use cosmon_core::auth::JwtClaims;

        use crate::instrumentation::{
            read_authz_ndjson, AuthzDecision, AUTHZ_NDJSON_RELATIVE_PATH,
        };

        let store = FakeStore::default();
        store.insert(make_molecule("task-20260503-eeee", MoleculeStatus::Running));
        let tmp = TempDir::new().unwrap();
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

        let tenant_auditor = Subject::from_jwt_claims(&JwtClaims {
            sub: "tenant_auditor".to_string(),
            scopes: vec!["molecule-observe".to_string()],
        })
        .unwrap();

        let id = MoleculeId::new("task-20260503-eeee").unwrap();
        let _view = observe(&store, tmp.path(), &tenant_auditor, &id).unwrap();

        let path = tmp.path().join(AUTHZ_NDJSON_RELATIVE_PATH);
        let events = read_authz_ndjson(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].subject_kind, "jwt:tenant_auditor");
        assert_eq!(events[0].decision, AuthzDecision::Absent);
    }
}
