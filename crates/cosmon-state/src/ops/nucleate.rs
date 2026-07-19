// SPDX-License-Identifier: AGPL-3.0-only

//! `nucleate` — instantiate a new molecule from a formula.
//!
//! Third verb extracted under `cosmon_state::ops`. The cs-cli
//! `cs nucleate` handler keeps its rich
//! CLI surface (cross-galaxy edges, `--blocks`, `--ttl`, …) and is *not*
//! refactored on top of this function in this pass — the lib entry point
//! covers the V1 RPP-API subset (formula, kind, variables, tags) and is
//! the call path the §8j `cosmon-rpp-adapter` exercises after a tenant
//! audit caught the silent shell-out to
//! the `cs` binary inside the container.
//!
//! The function is the smallest viable lib-direct shape: it takes a
//! [`StateStore`] reference, a `state_dir`, a `formulas_dir`, a typed
//! [`Subject`] and a [`NucleateRequest`]; loads + parses the formula,
//! delegates pure nucleation to [`cosmon_core::nucleate::nucleate`],
//! persists `state.json` via the store, writes the proof-of-work
//! artefacts (`briefing.md`, `prompt.md`, `log.md`), emits the canonical
//! `MoleculeNucleated` event, seals `prompt.md`, and emits an
//! `AuthzDecisionEvaluated` like the other verbs. No DAG edges, no
//! cross-galaxy refs, no symmetric-link maintenance — those stay in
//! cs-cli until a follow-up promotes them.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_core::auth::Subject;
use cosmon_core::error::CosmonError;
use cosmon_core::event_v2::EventV2;
use cosmon_core::formula::Formula;
use cosmon_core::id::FleetId;
use cosmon_core::kind::MoleculeKind;
use cosmon_core::nucleate::{
    self as core_nucleate, NucleateRequest as CoreNucleateRequest, NucleateResult,
};
use cosmon_core::tag::Tag;
use serde::Serialize;

use crate::briefing_seal::BriefingSeal;
use crate::event_log;
use crate::instrumentation::{emit_authz_decision, AuthzDecision};
use crate::ops::error::OpsError;
use crate::{MoleculeData, StateStore};

/// Errors returned by [`nucleate`].
///
/// Mirrors the per-verb pattern set by [`super::observe::ObserveError`]
/// and [`super::tag::TagError`]: dedicated enum, no flattening through a
/// mega-`CosmonError`, every variant maps to a stable kebab-case
/// [`OpsError::tag`] and an HTTP status. The variants are tuned to the
/// V1 RPP API: a missing formula is a 404 (the request is valid but
/// the named formula isn't installed for this tenant), a malformed body
/// is a 400 (`InvalidKind` / `InvalidTag` / `MissingVariable`), domain
/// failures collapse to 500, and any I/O hiccup at the store boundary
/// is 503.
#[derive(Debug, thiserror::Error)]
pub enum NucleateError {
    /// No formula file exists at `<formulas_dir>/<name>.formula.toml`.
    #[error("formula not found: {0}")]
    FormulaNotFound(String),
    /// Formula file is present but cannot be parsed.
    #[error("formula parse failed: {0}")]
    FormulaParse(String),
    /// Caller supplied a `kind` that is not a valid [`MoleculeKind`].
    #[error("invalid kind: {0}")]
    InvalidKind(String),
    /// Caller supplied a tag that does not match the [`Tag`] grammar.
    #[error("invalid tag: {0}")]
    InvalidTag(String),
    /// A required formula variable was not supplied and has no default.
    #[error("missing required variable: {0}")]
    MissingVariable(String),
    /// A required formula variable was supplied blank (empty or
    /// whitespace-only) — a briefless value that carries no intent. The
    /// nucleation half of the briefless-molecule guard (task-20260711-919a).
    #[error("required variable is empty: {0} (provide a non-blank value)")]
    EmptyVariable(String),
    /// Any other domain-level nucleation failure (e.g. id generation).
    #[error("domain nucleation failed: {0}")]
    Domain(String),
    /// A `blocked_by` entry is not a valid molecule id.
    #[error("invalid blocked_by reference: {0}")]
    InvalidBlockedBy(String),
    /// A `blocked_by` entry references a molecule absent from the
    /// tenant store. Dangling edges would leave a half-formed DAG no
    /// policy can schedule, so nucleation aborts (same discipline as
    /// `cs nucleate --blocked-by`).
    #[error("blocked_by target not found: {0}")]
    BlockedByNotFound(String),
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
}

impl OpsError for NucleateError {
    fn tag(&self) -> &'static str {
        match self {
            Self::FormulaNotFound(_) => "formula-not-found",
            Self::FormulaParse(_) => "formula-parse-failed",
            Self::InvalidKind(_) => "invalid-kind",
            Self::InvalidTag(_) => "invalid-tag",
            Self::MissingVariable(_) => "missing-variable",
            Self::EmptyVariable(_) => "empty-variable",
            Self::Domain(_) => "domain-nucleation-failed",
            Self::InvalidBlockedBy(_) => "invalid-blocked-by",
            Self::BlockedByNotFound(_) => "blocked-by-not-found",
            Self::StoreUnavailable(_) => "store-unavailable",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::FormulaNotFound(_) | Self::BlockedByNotFound(_) => 404,
            Self::FormulaParse(_)
            | Self::InvalidKind(_)
            | Self::InvalidTag(_)
            | Self::InvalidBlockedBy(_)
            | Self::MissingVariable(_)
            | Self::EmptyVariable(_) => 400,
            Self::Domain(_) => 500,
            Self::StoreUnavailable(_) => 503,
        }
    }
}

/// Inputs for a single library-direct nucleation.
///
/// Kept intentionally narrow: the V1 RPP API exposes exactly these four
/// fields (`formula`, `kind?`, `variables?`, `tags?`) plus the implicit
/// fleet (always `default` at the boundary; the cs-cli still owns the
/// rich `--fleet` / `--blocks` / `--ttl` surface).
#[derive(Debug, Clone)]
pub struct NucleateRequest {
    /// Formula name. Resolved as `<formulas_dir>/<name>.formula.toml`.
    pub formula: String,
    /// Optional molecule kind (e.g. `task`, `idea`, `decision`).
    pub kind: Option<String>,
    /// Variable bindings (`{key: value}`) overlaid on the formula's
    /// declared `[vars]` table.
    pub variables: HashMap<String, String>,
    /// Typed labels; each entry is parsed via [`Tag::new`].
    pub tags: Vec<String>,
    /// Fleet to nucleate into. Default `"default"`.
    pub fleet: FleetId,
    /// Molecule ids this nucleation is blocked by. Each entry must reference an
    /// existing molecule in the tenant store; the new molecule gets a
    /// `BlockedBy` typed link and each referenced blocker gets the
    /// symmetric `Blocks` link — the same DAG-edge semantics as
    /// `cs nucleate --blocked-by`. This is what lets a tenant nucleate
    /// a drainable DAG through the §8p surface.
    pub blocked_by: Vec<String>,
}

impl NucleateRequest {
    /// Build a request with sane defaults — empty vars/tags, no kind,
    /// fleet `"default"`.
    ///
    /// # Panics
    ///
    /// Infallible in practice: the static literal `"default"` is the
    /// canonical [`FleetId`] used by every cosmon-core test and CLI
    /// path. Kept as `expect` rather than `unwrap` to surface a clear
    /// message if the macro contract ever changes.
    #[must_use]
    pub fn for_formula(formula: impl Into<String>) -> Self {
        Self {
            formula: formula.into(),
            kind: None,
            variables: HashMap::new(),
            tags: Vec::new(),
            fleet: FleetId::new("default").expect("`default` is a valid fleet id"),
            blocked_by: Vec::new(),
        }
    }
}

/// View of a successful nucleation, returned by [`nucleate`].
///
/// Carries the freshly persisted [`MoleculeData`] plus the resolved
/// on-disk directory (so the caller can hand back a `Location` header
/// or a paste-able path). Wire renderers (cs-cli, RPP) consume this and
/// project to their channel-specific shapes.
#[derive(Debug, Clone)]
pub struct NucleateView {
    /// Persisted molecule fields straight from the store.
    pub data: MoleculeData,
    /// Resolved on-disk directory of the new molecule.
    pub molecule_dir: PathBuf,
}

/// Wire-format JSON body emitted by `cs nucleate --json` and the
/// V1 RPP `POST /v1/molecules` route.
///
/// Mirrors the cs-cli renderer (`cmd::nucleate::serialize_one`) so the
/// two callers stay byte-stable.
#[derive(Debug, Clone, Serialize)]
pub struct NucleateJson {
    /// Generated molecule id (string form).
    pub id: String,
    /// Source formula id.
    pub formula: String,
    /// Lifecycle status string. Always `"active"` to match cs-cli's
    /// historical wire shape — any post-creation transition is observable
    /// via `observe`.
    pub status: &'static str,
    /// Number of steps in the formula.
    pub total_steps: usize,
    /// Worker assigned at creation time, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_worker: Option<String>,
    /// Resolved variable bindings.
    pub variables: HashMap<String, String>,
    /// Created-at RFC3339.
    pub created_at: String,
}

impl NucleateJson {
    /// Render a [`NucleateView`] into the canonical JSON wire shape.
    #[must_use]
    pub fn from_view(view: &NucleateView) -> Self {
        let mol = &view.data;
        Self {
            id: mol.id.to_string(),
            formula: mol.formula_id.to_string(),
            status: "active",
            total_steps: mol.total_steps,
            assigned_worker: mol.assigned_worker.as_ref().map(ToString::to_string),
            variables: mol.variables.clone(),
            created_at: mol.created_at.to_rfc3339(),
        }
    }
}

/// Library-direct nucleation.
///
/// Loads the formula, runs domain nucleation, persists `state.json`,
/// writes the proof-of-work artefacts (`briefing.md`, `prompt.md`,
/// `log.md`), seals `prompt.md`, emits the `MoleculeNucleated` event,
/// and emits an authz-decision instrumentation point. Returns a
/// [`NucleateView`] the caller can render to its channel.
///
/// `subject` drives the V0 trivial authz check the same way the
/// `observe` and `tag` verbs do — `Subject::operator()` is granted,
/// every other subject yields `Absent`.
///
/// # Errors
///
/// See [`NucleateError`] for the failure shapes. The function never
/// touches DAG edges, cross-galaxy refs, or symmetric link maintenance;
/// those stay in `cs nucleate` until the broader CLI surface is
/// promoted to lib-direct.
///
/// # Defensive seal
///
/// `prompt.md` sealing is best-effort — any I/O or hash failure is
/// swallowed and logged. Mirrors the cs-cli convention: the seal is a
/// trace, not a lock.
///
/// # `#[verb]` registration
///
/// The annotation registers `nucleate` in the `cs-thin` verb registry
/// as `POST /v1/molecules` (principal `tenant`), so that the mechanical
/// HTTP client surfaces this verb in `cs-thin verbs --check` and the
/// `api_surface_freeze` test can assert the §8p subset matches the
/// rpp-adapter routes byte-for-byte. The macro emits a `NucleateVerb`
/// marker that does not collide with the existing [`NucleateRequest`]
/// type defined above — that name fight is the reason the marker is
/// suffixed `Verb`, not `Request`.
#[cosmon_thin_macro::verb(method = "POST", path = "/v1/molecules", principal = "tenant")]
pub fn nucleate(
    store: &dyn StateStore,
    state_dir: &Path,
    formulas_dir: &Path,
    subject: &Subject,
    request: NucleateRequest,
) -> Result<NucleateView, NucleateError> {
    use std::time::Instant;
    let started = Instant::now();

    // --- 1. Authz instrumentation (Allow for operator, Absent otherwise).
    let subject_kind = derive_subject_kind(subject);
    let decision = if subject.id().as_str() == "operator" {
        AuthzDecision::Allow
    } else {
        AuthzDecision::Absent
    };

    // Destructure once so subsequent steps can move the owned fields
    // freely (clippy `needless_pass_by_value` cleanup).
    let NucleateRequest {
        formula: formula_name,
        kind,
        variables,
        tags,
        fleet,
        blocked_by,
    } = request;

    // --- 2. Load + parse formula.
    let formula = load_formula(formulas_dir, &formula_name)?;

    // --- 3. Validate kind / tags BEFORE running domain nucleation so
    //        callers get a 400 on bad input instead of a half-applied
    //        write. cosmon_core::nucleate::nucleate would still reject
    //        missing variables, but kind/tag are CLI-validated today —
    //        the lib path mirrors the same up-front discipline.
    let mol_kind = kind
        .as_deref()
        .map(str::parse::<MoleculeKind>)
        .transpose()
        .map_err(|e| NucleateError::InvalidKind(e.to_string()))?;
    let parsed_tags: Vec<Tag> = tags
        .into_iter()
        .map(|s| Tag::new(s).map_err(|e| NucleateError::InvalidTag(e.to_string())))
        .collect::<Result<_, _>>()?;
    // Validate the blocked_by refs BEFORE domain nucleation: malformed
    // ids are a 400, dangling references a 404 — never a half-applied
    // write. Same up-front discipline as kind / tags above and the same
    // dangling-edge refusal as `cs nucleate --blocked-by`.
    let blockers: Vec<cosmon_core::id::MoleculeId> = blocked_by
        .into_iter()
        .map(|raw| {
            cosmon_core::id::MoleculeId::new(&raw)
                .map_err(|_| NucleateError::InvalidBlockedBy(raw.clone()))
        })
        .collect::<Result<_, _>>()?;
    for blocker in &blockers {
        if store.load_molecule(blocker).is_err() {
            return Err(NucleateError::BlockedByNotFound(blocker.to_string()));
        }
    }

    // --- 4. Domain nucleation.
    let core_request = CoreNucleateRequest {
        formula: &formula,
        variables,
        assign: None,
    };
    let result =
        core_nucleate::nucleate(core_request, &mut rand::thread_rng()).map_err(|e| match e {
            cosmon_core::nucleate::NucleateError::MissingVariable(k) => {
                NucleateError::MissingVariable(k)
            }
            cosmon_core::nucleate::NucleateError::EmptyVariable(k) => {
                NucleateError::EmptyVariable(k)
            }
            cosmon_core::nucleate::NucleateError::IdGeneration(err) => {
                NucleateError::Domain(err.to_string())
            }
        })?;

    // --- 5. Persist + write artefacts. Any I/O failure here is a
    //        StoreUnavailable: the request was valid, the substrate
    //        is the bottleneck.
    let mol_dir = persist_and_record(
        store,
        state_dir,
        &fleet,
        &formula,
        &result,
        mol_kind,
        &parsed_tags,
        &blockers,
    )?;

    // --- 6. Reload the persisted record so we hand back the canonical
    //        on-disk shape (incl. the prompt_seal stamp).
    let data = store
        .load_molecule(&result.id)
        .map_err(|e| NucleateError::StoreUnavailable(e.to_string()))?;

    // --- 7. Authz emit (after the work — keeps latency_ms truthful).
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    emit_authz_decision(
        state_dir,
        "nucleate",
        &subject_kind,
        None,
        decision,
        latency_ms,
    );

    Ok(NucleateView {
        data,
        molecule_dir: mol_dir,
    })
}

// -- internals ----------------------------------------------------------

fn derive_subject_kind(subject: &Subject) -> String {
    if subject.id().as_str() == "operator" {
        "operator".to_owned()
    } else {
        format!("jwt:{}", subject.id().as_str())
    }
}

fn load_formula(formulas_dir: &Path, name: &str) -> Result<Formula, NucleateError> {
    let formula_path = formulas_dir.join(format!("{name}.formula.toml"));
    if !formula_path.exists() {
        return Err(NucleateError::FormulaNotFound(name.to_owned()));
    }
    let text = fs::read_to_string(&formula_path)
        .map_err(|e| NucleateError::FormulaParse(format!("read {name}: {e}")))?;
    Formula::parse(&text).map_err(|e| NucleateError::FormulaParse(e.to_string()))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn persist_and_record(
    store: &dyn StateStore,
    state_dir: &Path,
    fleet_id: &FleetId,
    formula: &Formula,
    result: &NucleateResult,
    kind: Option<MoleculeKind>,
    tags: &[Tag],
    blockers: &[cosmon_core::id::MoleculeId],
) -> Result<PathBuf, NucleateError> {
    let mut data = MoleculeData {
        id: result.id.clone(),
        fleet_id: fleet_id.clone(),
        formula_id: result.formula_id.clone(),
        status: result.status,
        variables: result.variables.clone(),
        assigned_worker: result.assigned_worker.clone(),
        created_at: result.created_at,
        updated_at: result.created_at,
        total_steps: result.total_steps,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: Vec::new(),
        kind,
        class: cosmon_core::molecule_class::MoleculeClass::default(),
        typed_links: blockers
            .iter()
            .map(|source| cosmon_core::interaction::MoleculeLink::BlockedBy {
                source: source.clone(),
            })
            .collect(),
        project_id: None,
        assigned_role: None,
        session_name: None,
        tags: tags.iter().cloned().collect(),
        escalations: Vec::new(),
        freeze_on_last_step: formula.freeze_on_last_step,
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
    };

    store
        .save_molecule(&result.id, &data)
        .map_err(|e| NucleateError::StoreUnavailable(format!("save_molecule: {e}")))?;

    // Symmetry maintenance (same semantics as `cs nucleate
    // --blocked-by`): every referenced blocker gets the reverse
    // `Blocks{target=new}` link so `compile_plan`'s bidirectional BFS
    // and the ready-frontier reducers see the edge from either side.
    // Idempotent: skip when the link is already present. NOTE the CLI
    // path holds the fleet lock around this cycle; the `StateStore`
    // trait exposes no lock, so two CONCURRENT lib-direct nucleates
    // naming the same blocker can race on the blocker's typed_links
    // (last-writer-wins) — the same read-modify-write window the tag
    // verb already tolerates on this path. The child side is safe
    // either way (its own BlockedBy was persisted atomically above),
    // and compile_plan walks both directions, so a lost symmetric edge
    // degrades scheduling visibility, never correctness of the lock
    // discipline.
    for blocker in blockers {
        let mut b = store
            .load_molecule(blocker)
            .map_err(|e| NucleateError::StoreUnavailable(format!("load blocker: {e}")))?;
        let already = b.typed_links.iter().any(|l| {
            matches!(l, cosmon_core::interaction::MoleculeLink::Blocks { target } if *target == result.id)
        });
        if !already {
            b.typed_links
                .push(cosmon_core::interaction::MoleculeLink::Blocks {
                    target: result.id.clone(),
                });
            b.updated_at = Utc::now();
            store
                .save_molecule(blocker, &b)
                .map_err(|e| NucleateError::StoreUnavailable(format!("save blocker: {e}")))?;
        }
    }

    let mol_dir = state_dir
        .join("fleets")
        .join(fleet_id.as_str())
        .join("molecules")
        .join(result.id.as_str());
    fs::create_dir_all(&mol_dir).map_err(|e| {
        NucleateError::StoreUnavailable(format!("mkdir molecule_dir {}: {e}", mol_dir.display()))
    })?;
    write_briefing(&mol_dir, formula, result)?;
    write_prompt(&mol_dir, formula, result)?;
    write_log(&mol_dir, result)?;

    // Emit canonical EventV2. Failure here is a substrate problem —
    // we surface as StoreUnavailable rather than swallowing.
    event_log::emit_one(
        state_dir.join("events.jsonl"),
        EventV2::MoleculeNucleated {
            molecule_id: result.id.clone(),
            formula_id: result.formula_id.as_str().to_owned(),
            parent_id: None,
            blocks: Vec::new(),
        },
        None,
    )
    .map_err(|e| NucleateError::StoreUnavailable(format!("emit MoleculeNucleated: {e}")))?;

    // Best-effort prompt seal — defensive, never fatal. Mirrors cs-cli.
    if let Some(seal) = try_seal_prompt(&mol_dir) {
        data.prompt_seal = Some(seal.clone());
        data.updated_at = Utc::now();
        if let Err(e) = store.save_molecule(&result.id, &data) {
            // Log as a warning; the molecule is already committed.
            eprintln!("warning: failed to stamp prompt seal on {}: {e}", result.id);
        } else {
            let _ = event_log::emit_one(
                state_dir.join("events.jsonl"),
                EventV2::PromptSealed {
                    molecule_id: result.id.clone(),
                    hash: seal.hash.clone(),
                    sealed_at: seal.sealed_at,
                    bytes: seal.briefing_bytes,
                    canonical_version: seal.canonical_version,
                },
                None,
            );
        }
    }

    Ok(mol_dir)
}

fn try_seal_prompt(mol_dir: &Path) -> Option<BriefingSeal> {
    match fs::read(mol_dir.join("prompt.md")) {
        Ok(bytes) => Some(BriefingSeal::of_text_or_bytes(0, &bytes)),
        Err(_) => None,
    }
}

fn write_briefing(
    mol_dir: &Path,
    formula: &Formula,
    result: &NucleateResult,
) -> Result<(), NucleateError> {
    let mut md = String::new();
    let _ = write!(md, "# Molecule: {}\n\n", result.id);
    let _ = write!(md, "**Formula:** {}\n\n", result.formula_id);
    if let Some(ref worker) = result.assigned_worker {
        let _ = write!(md, "**Assigned to:** {worker}\n\n");
    }
    md.push_str("## Steps\n\n");
    for (i, step) in formula.steps.iter().enumerate() {
        let _ = write!(md, "### Step {} — {}\n\n", i + 1, step.title);
        if !step.description.is_empty() {
            md.push_str(&step.description);
            md.push_str("\n\n");
        }
        if let Some(ref criteria) = step.exit_criteria {
            let _ = write!(md, "**Exit criteria:** {criteria}\n\n");
        }
    }
    fs::write(mol_dir.join("briefing.md"), md.as_bytes())
        .map_err(|e| NucleateError::StoreUnavailable(format!("write briefing.md: {e}")))
}

fn write_prompt(
    mol_dir: &Path,
    formula: &Formula,
    result: &NucleateResult,
) -> Result<(), NucleateError> {
    let mut md = String::new();
    md.push_str("---\n");
    let _ = writeln!(
        md,
        "nucleated_at: {}",
        result.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    let _ = writeln!(md, "molecule_id: {}", result.id);
    let _ = writeln!(md, "formula: {}", formula.name);
    let _ = writeln!(md, "formula_id: {}", result.formula_id);
    if !result.variables.is_empty() {
        md.push_str("variables:\n");
        let mut keys: Vec<&String> = result.variables.keys().collect();
        keys.sort();
        for k in keys {
            let v = &result.variables[k];
            if v.contains('\n') {
                let _ = writeln!(md, "  {k}: |");
                for line in v.lines() {
                    let _ = writeln!(md, "    {line}");
                }
            } else {
                let escaped = v.replace('\\', "\\\\").replace('"', "\\\"");
                let _ = writeln!(md, "  {k}: \"{escaped}\"");
            }
        }
    }
    md.push_str("---\n\n# Operator prompt\n\n");
    if result.variables.is_empty() {
        md.push_str("_(no variables bound at nucleation)_\n");
    } else {
        let mut keys: Vec<&String> = result.variables.keys().collect();
        keys.sort();
        for k in keys {
            let _ = writeln!(md, "## {k}\n\n{}\n", result.variables[k]);
        }
    }
    fs::write(mol_dir.join("prompt.md"), md.as_bytes())
        .map_err(|e| NucleateError::StoreUnavailable(format!("write prompt.md: {e}")))
}

fn write_log(mol_dir: &Path, result: &NucleateResult) -> Result<(), NucleateError> {
    let entry = format!(
        "# Log: {}\n\n- **{}** — Molecule nucleated from formula `{}`\n",
        result.id,
        result.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
        result.formula_id,
    );
    fs::write(mol_dir.join("log.md"), entry.as_bytes())
        .map_err(|e| NucleateError::StoreUnavailable(format!("write log.md: {e}")))
}

#[allow(dead_code)]
fn _unused(_: CosmonError) {} // keeps the import linked for the doc-paths

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Mutex;

    use cosmon_core::auth::Subject;
    use cosmon_core::error::CosmonError;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use tempfile::TempDir;

    use super::*;
    use crate::ops::error::is_kebab_case;
    use crate::MoleculeFilter;

    /// In-memory `StateStore` for tests — same shape as the one in
    /// `observe.rs`, kept local so each verb test stays self-contained.
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

    fn write_minimal_formula(formulas_dir: &Path, name: &str) {
        fs::create_dir_all(formulas_dir).unwrap();
        let body = format!(
            r#"
formula = "{name}"
version = 1
description = "smoke"
id_prefix = "task"

[[steps]]
id = "step-1"
title = "First"
description = "do it"
"#
        );
        fs::write(formulas_dir.join(format!("{name}.formula.toml")), body).unwrap();
    }

    #[test]
    fn nucleate_persists_and_writes_artefacts() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        write_minimal_formula(&formulas_dir, "task-work");
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

        let store = FakeStore::default();
        let view = nucleate(
            &store,
            &state_dir,
            &formulas_dir,
            &Subject::operator(),
            NucleateRequest::for_formula("task-work"),
        )
        .unwrap();

        assert_eq!(view.data.formula_id.as_str(), "task-work");
        assert_eq!(view.data.total_steps, 1);
        assert!(view.molecule_dir.join("briefing.md").exists());
        assert!(view.molecule_dir.join("prompt.md").exists());
        assert!(view.molecule_dir.join("log.md").exists());

        // The state.json equivalent: the store has the record under the
        // generated id.
        let loaded = store.load_molecule(&view.data.id).unwrap();
        assert_eq!(loaded.id, view.data.id);
        assert!(loaded.prompt_seal.is_some(), "prompt should be sealed");
    }

    #[test]
    fn nucleate_emits_molecule_nucleated_event() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        write_minimal_formula(&formulas_dir, "task-work");
        std::env::remove_var("COSMON_AUTHZ_INSTRUMENTATION_PATH");

        let store = FakeStore::default();
        let _view = nucleate(
            &store,
            &state_dir,
            &formulas_dir,
            &Subject::operator(),
            NucleateRequest::for_formula("task-work"),
        )
        .unwrap();

        let events = event_log::read_all(state_dir.join("events.jsonl")).unwrap();
        assert!(
            events
                .iter()
                .any(|env| matches!(&env.event, EventV2::MoleculeNucleated { .. })),
            "MoleculeNucleated event missing: {events:?}"
        );
    }

    #[test]
    fn nucleate_returns_formula_not_found_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&formulas_dir).unwrap();

        let store = FakeStore::default();
        let err = nucleate(
            &store,
            &state_dir,
            &formulas_dir,
            &Subject::operator(),
            NucleateRequest::for_formula("nonexistent"),
        )
        .unwrap_err();
        assert!(matches!(err, NucleateError::FormulaNotFound(_)));
        assert_eq!(err.tag(), "formula-not-found");
        assert_eq!(err.http_status(), 404);
    }

    #[test]
    fn nucleate_returns_invalid_kind_on_garbage_input() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        write_minimal_formula(&formulas_dir, "task-work");

        let store = FakeStore::default();
        let mut req = NucleateRequest::for_formula("task-work");
        req.kind = Some("not-a-kind".into());
        let err =
            nucleate(&store, &state_dir, &formulas_dir, &Subject::operator(), req).unwrap_err();
        assert!(matches!(err, NucleateError::InvalidKind(_)));
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn nucleate_returns_invalid_tag_on_bad_grammar() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        write_minimal_formula(&formulas_dir, "task-work");

        let store = FakeStore::default();
        let mut req = NucleateRequest::for_formula("task-work");
        req.tags = vec!["with space".into()];
        let err =
            nucleate(&store, &state_dir, &formulas_dir, &Subject::operator(), req).unwrap_err();
        assert!(matches!(err, NucleateError::InvalidTag(_)));
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn nucleate_returns_missing_variable_for_required_var() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        let body = r#"
formula = "needy"
version = 1
id_prefix = "task"

[vars.target]
description = "who"
required = true

[[steps]]
id = "s"
title = "S"
description = "."
"#;
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::write(formulas_dir.join("needy.formula.toml"), body).unwrap();

        let store = FakeStore::default();
        let err = nucleate(
            &store,
            &state_dir,
            &formulas_dir,
            &Subject::operator(),
            NucleateRequest::for_formula("needy"),
        )
        .unwrap_err();
        assert!(matches!(err, NucleateError::MissingVariable(ref k) if k == "target"));
    }

    #[test]
    fn nucleate_records_kind_and_tags_on_data() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        write_minimal_formula(&formulas_dir, "task-work");

        let store = FakeStore::default();
        let mut req = NucleateRequest::for_formula("task-work");
        req.kind = Some("task".into());
        req.tags = vec!["temp:warm".into(), "smoke".into()];
        let view = nucleate(&store, &state_dir, &formulas_dir, &Subject::operator(), req).unwrap();
        assert!(view.data.kind.is_some());
        assert_eq!(view.data.tags.len(), 2);
        assert!(view
            .data
            .tags
            .iter()
            .any(|t| t.key() == "temp" && t.value() == Some("warm")));
    }

    #[test]
    fn nucleate_error_tags_are_kebab_case() {
        let cases: &[NucleateError] = &[
            NucleateError::FormulaNotFound("x".into()),
            NucleateError::FormulaParse("x".into()),
            NucleateError::InvalidKind("x".into()),
            NucleateError::InvalidTag("x".into()),
            NucleateError::MissingVariable("x".into()),
            NucleateError::Domain("x".into()),
            NucleateError::StoreUnavailable("x".into()),
        ];
        for e in cases {
            assert!(
                is_kebab_case(e.tag()),
                "tag must be kebab-case: {}",
                e.tag()
            );
        }
    }

    #[test]
    fn nucleate_view_renders_to_wire_json() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        write_minimal_formula(&formulas_dir, "task-work");

        let store = FakeStore::default();
        let view = nucleate(
            &store,
            &state_dir,
            &formulas_dir,
            &Subject::operator(),
            NucleateRequest::for_formula("task-work"),
        )
        .unwrap();
        let json = NucleateJson::from_view(&view);
        assert_eq!(json.formula, "task-work");
        assert_eq!(json.status, "active");
        assert_eq!(json.total_steps, 1);
        assert!(json.id.starts_with("task-"));
    }

    // Suppress dead_code for the doc-path tag binding that keeps imports
    // tidy without exposing them as part of the API surface.
    #[allow(dead_code)]
    fn _unused_imports() -> (FleetId, FormulaId, BTreeSet<()>) {
        (
            FleetId::new("default").unwrap(),
            FormulaId::new("noop").unwrap(),
            BTreeSet::new(),
        )
    }
}
