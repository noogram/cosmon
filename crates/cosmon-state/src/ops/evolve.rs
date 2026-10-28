// SPDX-License-Identifier: AGPL-3.0-only

//! `evolve` — advance a molecule past its current formula step.
//!
//! One operation, two callers: `cs evolve` and
//! `POST /v1/molecules/:id/evolve` both call [`evolve`], so a step advanced
//! remotely leaves the molecule in the state a local advance would.
//!
//! The operation owns everything that is a property of the *molecule*:
//! the terminal refusal, the energy circuit breaker, the step transition
//! (`cosmon_core::evolve`), the acceptance-artifact guard, the persisted
//! state with its intent record, the event records, `log.md`, the
//! regenerated `briefing.md` (with the committee-posture pointer) and the
//! briefing seal.
//!
//! What it cannot own is a property of the *caller's machine*. Those effects
//! go through [`EvolveEffects`]: the shell verification command a formula
//! step may declare, the repatriation of artifacts a worker wrote to its
//! worktree, the per-step auto-commit, the inline gate step, the bootstrap
//! seal that walks the worktree, and the realized-energy capture. The CLI
//! implements all of them. A tenant-facing caller implements none of them,
//! and the defaults refuse what cannot be done safely (a step that declares
//! a shell verification is answered [`EvolveError::Unsupported`], never
//! skipped).

use std::path::Path;
use std::time::SystemTime;

use chrono::Utc;
use cosmon_core::error::CosmonError;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::event_v2::{EventV2, Seq, StuckReason};
use cosmon_core::evolve::{self, EvolveOutcome, EvolveRequest, NewState};
use cosmon_core::formula::{Formula, Step};
use cosmon_core::id::{MoleculeId, StepId};
use cosmon_core::molecule::MoleculeStatus;
use serde::Serialize;

use crate::ops::error::OpsError;
use crate::{event_log, BriefingSeal, MoleculeData, PendingStep, StateStore};

/// Why a step was not advanced.
#[derive(Debug, thiserror::Error)]
pub enum EvolveError {
    /// No molecule exists with the given ID.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),
    /// The molecule is terminal and cannot advance.
    #[error("molecule {0} is `{1}`, must be running to evolve")]
    NotRunnable(MoleculeId, MoleculeStatus),
    /// The transition itself was refused (no evidence, step out of range,
    /// unmet dependency, molecule not runnable).
    #[error("{0}")]
    Refused(#[from] evolve::EvolveError),
    /// The per-molecule step budget is spent. The molecule has been parked
    /// in `Frozen`.
    #[error("energy-exhausted (cap={cap}, remaining=0)")]
    EnergyExhausted {
        /// The immutable cap the molecule was nucleated with.
        cap: u32,
    },
    /// The step's verification failed and retries remain. Nothing was
    /// persisted; the caller can fix the cause and evolve again.
    #[error(
        "verification failed for step \"{step}\": {output}, {retries_remaining} retries remaining"
    )]
    VerificationRetry {
        /// The step whose verification failed.
        step: String,
        /// Combined output of the verification command.
        output: String,
        /// Retries still available.
        retries_remaining: u32,
    },
    /// The step's verification failed and no retry remains. The molecule has
    /// been parked in `Frozen`.
    #[error("{reason}")]
    VerificationExhausted {
        /// Why, including the command output.
        reason: String,
    },
    /// The step declares acceptance artifacts that are absent from the
    /// molecule directory. Nothing was persisted.
    #[error("step \"{step}\" declares acceptance_artifacts missing from the molecule directory {mol_dir}: {}", .missing.join(", "))]
    ArtifactsMissing {
        /// The step whose artifacts are missing.
        step: String,
        /// The canonical molecule directory the artifacts belong in.
        mol_dir: String,
        /// The declared entries that are absent, empty or out of bounds.
        missing: Vec<String>,
    },
    /// The caller cannot honour something the step declares (for example a
    /// shell verification command on a surface that must not run one).
    #[error("step \"{step}\" cannot be advanced here: {what}")]
    Unsupported {
        /// The step concerned.
        step: String,
        /// What it declares that this caller cannot do.
        what: String,
    },
    /// The state store could not be read or written.
    #[error("state store unavailable: {0}")]
    StoreUnavailable(String),
    /// A caller-side effect failed after the state advanced.
    #[error("evolve effect failed: {0}")]
    Effect(String),
}

impl EvolveError {
    fn from_cosmon(id: &MoleculeId, err: CosmonError) -> Self {
        match err {
            CosmonError::MoleculeNotFound(_) => Self::MoleculeNotFound(id.clone()),
            other => Self::StoreUnavailable(other.to_string()),
        }
    }
}

impl OpsError for EvolveError {
    fn tag(&self) -> &'static str {
        match self {
            Self::MoleculeNotFound(_) => "molecule-not-found",
            Self::NotRunnable(_, _) => "not-runnable",
            Self::Refused(evolve::EvolveError::MissingEvidence { .. }) => "missing-evidence",
            Self::Refused(_) => "step-refused",
            Self::EnergyExhausted { .. } => "energy-exhausted",
            Self::VerificationRetry { .. } => "verification-failed",
            Self::VerificationExhausted { .. } => "verification-exhausted",
            Self::ArtifactsMissing { .. } => "artifacts-missing",
            Self::Unsupported { .. } => "unsupported-step",
            Self::StoreUnavailable(_) => "store-unavailable",
            Self::Effect(_) => "effect-failed",
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::MoleculeNotFound(_) => 404,
            Self::Refused(evolve::EvolveError::MissingEvidence { .. }) => 400,
            Self::NotRunnable(_, _)
            | Self::Refused(_)
            | Self::EnergyExhausted { .. }
            | Self::VerificationRetry { .. }
            | Self::VerificationExhausted { .. }
            | Self::ArtifactsMissing { .. } => 409,
            Self::Unsupported { .. } => 501,
            Self::StoreUnavailable(_) | Self::Effect(_) => 503,
        }
    }
}

/// What a caller's step check can answer instead of "fine".
#[derive(Debug)]
pub enum StepRefusal {
    /// Verification failed, retries remain: do not persist, let the caller retry.
    Retry {
        /// Combined output of the verification command.
        output: String,
        /// Retries still available.
        retries_remaining: u32,
    },
    /// Verification failed for good: park the molecule.
    Exhausted {
        /// Why, including the command output.
        reason: String,
    },
    /// The caller cannot do what the step declares.
    Unsupported {
        /// What the step declares.
        what: String,
    },
    /// Any other refusal (for example the repository is not trusted).
    Other(String),
}

/// The effects of an advance that depend on the caller's machine.
///
/// See the module docs for why they are a port rather than part of
/// [`evolve`]. Every method has a default that does nothing, except
/// [`Self::verify_step`], whose default refuses a declared shell command.
pub trait EvolveEffects {
    /// Run the shell verification the completed step declares, if any.
    ///
    /// Called under the fleet lock, before any state mutation. The default
    /// refuses a step that declares verification criteria: a caller that does
    /// not run shell commands must say so rather than pass the step silently.
    ///
    /// # Errors
    ///
    /// A [`StepRefusal`] stops the advance before anything is persisted
    /// (except [`StepRefusal::Exhausted`], which parks the molecule).
    fn verify_step(&self, _mol: &MoleculeData, step: &Step) -> Result<(), StepRefusal> {
        match &step.verification {
            Some(v) if !v.criteria.is_empty() => Err(StepRefusal::Unsupported {
                what: "it declares a shell verification command".to_owned(),
            }),
            _ => Ok(()),
        }
    }

    /// Move acceptance artifacts the worker left outside `mol_dir` into it.
    /// Returns the entries recovered. The guard is re-evaluated afterwards,
    /// so this can only help.
    fn repatriate(&self, _mol_dir: &Path, _missing: &[String]) -> Vec<String> {
        Vec::new()
    }

    /// Append the legacy `molecule_evolved` envelope to the event log.
    fn append_legacy_event(&self, events_path: &Path, envelope: &Envelope);

    /// Run after the advance is persisted and the fleet lock released,
    /// before any event is emitted. May change `updated` (the CLI runs an
    /// inline gate step here and can collapse the molecule).
    ///
    /// # Errors
    ///
    /// The message of a failed effect.
    fn after_advance(
        &self,
        _updated: &mut MoleculeData,
        _outcome: &EvolveOutcome,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Run once `briefing.md` has been regenerated and sealed.
    ///
    /// # Errors
    ///
    /// The message of a failed effect.
    fn after_briefing(
        &self,
        _updated: &MoleculeData,
        _briefing_path: &Path,
        _seal_step: u32,
        _causal_parent: Option<Seq>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// The molecule reached `Completed`.
    fn on_completed(&self, _state_dir: &Path, _id: &MoleculeId) {}

    /// A non-fatal observation (verification warning, seal that could not
    /// be persisted). The CLI prints it to stderr; a service logs it.
    fn warn(&self, _message: &str) {}
}

/// Inputs for [`evolve`].
#[derive(Debug, Clone)]
pub struct EvolveOpRequest<'a> {
    /// Evidence documenting why the current step is complete.
    pub evidence: &'a str,
    /// The molecule's formula.
    pub formula: &'a Formula,
}

/// Result of a successful [`evolve`] call.
#[derive(Debug, Clone)]
pub struct EvolveView {
    /// The molecule as it stands after the advance.
    pub updated: MoleculeData,
    /// What the core transition reported.
    pub outcome: EvolveOutcome,
}

/// Wire shape of an [`EvolveView`] — what `cs --json evolve` prints and the
/// route returns.
#[derive(Debug, Clone, Serialize)]
pub struct EvolveJson {
    /// Molecule id.
    pub molecule: String,
    /// The step just completed.
    pub completed_step: String,
    /// Status after the advance.
    pub new_status: String,
    /// The step the molecule is now on; `None` when it completed.
    pub new_step: Option<String>,
    /// Non-fatal warnings from the transition.
    pub warnings: Vec<String>,
}

impl EvolveJson {
    /// Project a view onto its wire shape.
    #[must_use]
    pub fn from_view(view: &EvolveView) -> Self {
        Self {
            molecule: view.updated.id.as_str().to_owned(),
            completed_step: view.outcome.completed_step.id.clone(),
            new_status: view.updated.status.to_string(),
            new_step: match &view.outcome.new_state {
                NewState::Active { step_id, .. } => Some(step_id.clone()),
                _ => None,
            },
            warnings: view.outcome.warnings.clone(),
        }
    }
}

/// The evidence text as an event field: `None` when the caller recorded none,
/// so the event omits the key rather than carrying an empty string.
fn non_empty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_owned())
}

fn park_frozen(
    store: &dyn StateStore,
    state_dir: &Path,
    mut mol: MoleculeData,
    reason: StuckReason,
) -> Result<(), EvolveError> {
    let id = mol.id.clone();
    mol.status = MoleculeStatus::Frozen;
    mol.updated_at = Utc::now();
    store
        .save_molecule(&id, &mol)
        .map_err(|e| EvolveError::from_cosmon(&id, e))?;
    let _ = event_log::emit_one(
        state_dir.join("events.jsonl"),
        EventV2::MoleculeStuck {
            molecule_id: id,
            reason,
        },
        None,
    );
    Ok(())
}

/// Advance a molecule past its current step.
///
/// # Errors
///
/// See [`EvolveError`]. Every variant other than the two that park the
/// molecule ([`EvolveError::EnergyExhausted`],
/// [`EvolveError::VerificationExhausted`]) leaves the molecule's state
/// exactly as it was.
///
/// # `#[verb]` registration
///
/// Annotated as `POST /v1/molecules/:id/evolve` (principal `tenant`).
#[cosmon_thin_macro::verb(
    method = "POST",
    path = "/v1/molecules/:id/evolve",
    principal = "tenant"
)]
#[allow(clippy::too_many_lines)]
pub fn evolve(
    store: &dyn StateStore,
    state_dir: &Path,
    id: &MoleculeId,
    request: &EvolveOpRequest<'_>,
    effects: &dyn EvolveEffects,
) -> Result<EvolveView, EvolveError> {
    let formula = request.formula;
    // The path `cs done` tears down: the only place a step's acceptance
    // artifacts can durably live.
    let mol_dir = store.molecule_dir(id);
    let events_path = state_dir.join("events.jsonl");
    let store_err = |e: CosmonError| EvolveError::from_cosmon(id, e);

    // Hold the fleet lock for the load → evolve → save cycle (ADR-131
    // Decision 2) so concurrent evolves cannot clobber each other.
    let (mut updated, outcome) = {
        let _g = store.lock_fleet().map_err(store_err)?;
        let mol_data = store.load_molecule(id).map_err(store_err)?;

        // Terminal calls refuse before the energy breaker can park them.
        if matches!(
            mol_data.status,
            MoleculeStatus::Completed | MoleculeStatus::Collapsed
        ) {
            return Err(EvolveError::NotRunnable(id.clone(), mol_data.status));
        }

        // Energy circuit breaker (THESIS Part XI): an exhausted budget parks
        // the molecule so a human can bump the cap, collapse or split.
        if let Some(budget) = mol_data.energy_budget {
            if budget.is_exhausted() {
                park_frozen(store, state_dir, mol_data, StuckReason::EnergyExhausted)?;
                return Err(EvolveError::EnergyExhausted { cap: budget.cap });
            }
        }

        let outcome = evolve::evolve(
            mol_data.status,
            mol_data.current_step,
            &mol_data.completed_steps,
            formula,
            &EvolveRequest {
                evidence: request.evidence.to_owned(),
                timestamp: Utc::now(),
            },
        )?;
        for warning in &outcome.warnings {
            effects.warn(warning);
        }

        let completed_step_spec = &formula.steps[outcome.completed_step.index];
        match effects.verify_step(&mol_data, completed_step_spec) {
            Ok(()) => {}
            Err(StepRefusal::Retry {
                output,
                retries_remaining,
            }) => {
                return Err(EvolveError::VerificationRetry {
                    step: outcome.completed_step.id.clone(),
                    output,
                    retries_remaining,
                });
            }
            Err(StepRefusal::Exhausted { reason }) => {
                park_frozen(
                    store,
                    state_dir,
                    mol_data,
                    StuckReason::from(reason.clone()),
                )?;
                return Err(EvolveError::VerificationExhausted { reason });
            }
            Err(StepRefusal::Unsupported { what }) => {
                return Err(EvolveError::Unsupported {
                    step: outcome.completed_step.id.clone(),
                    what,
                });
            }
            Err(StepRefusal::Other(message)) => return Err(EvolveError::Effect(message)),
        }

        // Artifact-presence guard (HARD gate): every artifact the step
        // declared must have landed under the canonical `mol_dir` BEFORE any
        // committed state mutation. Blocking the advance is the recovery
        // window — the molecule stays on its step, so the artifact can be
        // moved and the evolve re-run.
        let mut missing =
            missing_expected_artifacts(&mol_dir, &completed_step_spec.expected_artifacts);
        if !missing.is_empty() {
            // The caller reports what it moved; the guard is then
            // re-evaluated, so a recovery can only help.
            if !effects.repatriate(&mol_dir, &missing).is_empty() {
                missing =
                    missing_expected_artifacts(&mol_dir, &completed_step_spec.expected_artifacts);
            }
        }
        if !missing.is_empty() {
            return Err(EvolveError::ArtifactsMissing {
                step: outcome.completed_step.id.clone(),
                mol_dir: mol_dir.display().to_string(),
                missing,
            });
        }

        let step_id = StepId::new(&outcome.completed_step.id)
            .map_err(|e| EvolveError::StoreUnavailable(e.to_string()))?;
        let mut updated = mol_data;
        updated.completed_steps.push(step_id);
        // The exhaustion check above guaranteed `remaining > 0`.
        if let Some(mut b) = updated.energy_budget {
            b.consume();
            updated.energy_budget = Some(b);
        }
        let now = Utc::now();
        updated.updated_at = now;
        // Forward motion and a durable work product: stamp both clocks.
        updated.last_progress_at = Some(now);
        updated.last_output_at = Some(now);
        match &outcome.new_state {
            NewState::Active { current_step, .. } => {
                updated.current_step = *current_step;
                // Promote Queued → Running on first evolve.
                updated.status = MoleculeStatus::Running;
            }
            NewState::Completed => {
                // Respect freeze_on_last_step: freeze instead of completing.
                updated.status = if updated.freeze_on_last_step {
                    MoleculeStatus::Frozen
                } else {
                    MoleculeStatus::Completed
                };
                // `current_step = total_steps` so observers read "N/N".
                updated.current_step = updated.total_steps;
            }
            _ => {}
        }

        // Intent record (godel's intent+receipt, ADR-036): a crash between
        // this save and the receipt clear below leaves `pending_step` for a
        // later replay to inspect.
        updated.pending_step = Some(PendingStep {
            target_step: updated.current_step,
            started_at: Utc::now(),
            commit_sha: None,
        });

        if updated.status == MoleculeStatus::Completed {
            effects.on_completed(state_dir, id);
        }
        store
            .save_molecule(&updated.id.clone(), &updated)
            .map_err(store_err)?;
        (updated, outcome)
    };

    effects
        .after_advance(&mut updated, &outcome)
        .map_err(EvolveError::Effect)?;

    // Legacy `molecule_evolved` record, then the EventV2 records.
    effects.append_legacy_event(
        &events_path,
        &Envelope::now(Event::MoleculeEvolved {
            molecule_id: id.clone(),
            step: outcome.completed_step.index,
            total: updated.total_steps,
        }),
    );
    let step_seq = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStepCompleted {
            molecule_id: id.clone(),
            step: outcome.completed_step.index,
            total: updated.total_steps,
            duration_ms: None,
            step_hash: None,
            evidence: non_empty(request.evidence),
        },
        None,
    )
    .ok();
    if matches!(outcome.new_state, NewState::Completed) {
        let _ = event_log::emit_one(
            &events_path,
            EventV2::MoleculeCompleted {
                molecule_id: id.clone(),
                duration_ms: None,
                reason: "all steps completed".to_owned(),
                summary: non_empty(request.evidence),
            },
            step_seq,
        );
    }

    // Append to log.md in the molecule directory.
    let log_path = mol_dir.join("log.md");
    let existing_log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let new_log = if existing_log.is_empty() {
        format!("# Evolution Log\n\n{}", outcome.log_entry)
    } else {
        format!("{existing_log}{}", outcome.log_entry)
    };
    std::fs::write(&log_path, new_log)
        .map_err(|e| EvolveError::Effect(format!("failed to write log.md: {e}")))?;

    // Write briefing.md (a final one when the molecule completed).
    let briefing_path = mol_dir.join("briefing.md");
    if let Some(ref briefing) = outcome.briefing {
        let task = cosmon_core::briefing::render_task(&updated.variables);
        std::fs::write(&briefing_path, format!("{task}{briefing}"))
            .map_err(|e| EvolveError::Effect(format!("failed to write briefing.md: {e}")))?;
    } else {
        std::fs::write(
            &briefing_path,
            "# Molecule Briefing\n\n**Status:** COMPLETED\n\nAll steps have been completed.\n",
        )
        .map_err(|e| EvolveError::Effect(format!("failed to write briefing.md: {e}")))?;
    }

    // Committee-posture delivery: the briefing just rewritten is a fresh
    // projection of the formula step and carries no adversarial contract. If
    // this molecule is a committee seat, deliver the stable pointer so the
    // seat's persona witness stays satisfied across every step advance.
    // Runs before the seal so the seal covers the delivered pointer.
    deliver_committee_posture_reference(&mol_dir, &briefing_path)
        .map_err(|e| EvolveError::Effect(e.to_string()))?;

    // Soft-contract seal of the briefing just written. Defensive: any
    // failure is reported and swallowed, seal emission never blocks an
    // advance.
    let seal_step = u32::try_from(updated.current_step).unwrap_or(u32::MAX);
    if let Some(seal) = try_seal_briefing(&briefing_path, seal_step) {
        let persisted: Result<(), CosmonError> = 'seal: {
            let _g = match store.lock_fleet() {
                Ok(g) => g,
                Err(e) => break 'seal Err(e),
            };
            let mut mol = match store.load_molecule(id) {
                Ok(m) => m,
                Err(e) => break 'seal Err(e),
            };
            mol.briefing_seals.push(seal.clone());
            mol.updated_at = Utc::now();
            store.save_molecule(id, &mol)
        };
        if let Err(e) = persisted {
            effects.warn(&format!("could not persist briefing seal: {e}"));
        } else {
            let _ = event_log::emit_one(
                &events_path,
                EventV2::BriefingSealed {
                    molecule_id: id.clone(),
                    step: seal.step,
                    hash: seal.hash.clone(),
                    sealed_at: seal.sealed_at,
                    bytes: seal.briefing_bytes,
                    canonical_version: seal.canonical_version,
                },
                step_seq,
            );
        }
    }

    effects
        .after_briefing(&updated, &briefing_path, seal_step, step_seq)
        .map_err(EvolveError::Effect)?;

    // Receipt: artifacts have landed, clear the `pending_step` intent. A
    // stale intent left by a crash is observed by a replay as a no-op marker.
    {
        let _g = store.lock_fleet().map_err(store_err)?;
        let mut mol = store.load_molecule(id).map_err(store_err)?;
        if mol.pending_step.is_some() {
            mol.pending_step = None;
            mol.updated_at = Utc::now();
            store.save_molecule(id, &mol).map_err(store_err)?;
        }
    }

    Ok(EvolveView { updated, outcome })
}

/// Return the subset of `expected` artifacts that are absent from `mol_dir`.
///
/// Each entry is a path relative to `mol_dir`. A trailing `/` (e.g.
/// `"responses/"`) marks a **directory that must exist and be non-empty** —
/// an empty `responses/` is as much a near-loss as a missing one (no proof
/// of work landed). Every other entry must resolve to an existing path
/// (file or directory).
///
/// This is the read-only, idempotent core of the artifact-presence HARD gate
/// (Lever B). It never mutates state; the caller turns
/// a non-empty result into a failed advance. See
/// [`cosmon_core::formula::Step::expected_artifacts`] for the doctrine
/// (why this blocks where the briefing-seal family swallows).
#[must_use]
pub fn missing_expected_artifacts(mol_dir: &Path, expected: &[String]) -> Vec<String> {
    unsatisfied_expected_artifacts(mol_dir, expected, None)
}

/// The acceptance-artifact presence check, shared by the `cs evolve` gate
/// (per-step, `min_mtime = None`) and the in-process / detached-local finalize
/// guard (whole-formula, `min_mtime = Some(step_start)`).
///
/// An entry is *unsatisfied* — i.e. fails to prove work — when any of the four
/// failure modes the acceptance-artifact contract names holds (Jesse #4):
///
/// * **absent** — the declared path does not exist under `mol_dir`;
/// * **empty** — a declared file exists but is zero bytes, or a declared
///   directory (`"…/"`) exists but holds no entry. A zero-byte file is as much
///   a near-loss as none: a weak model that `touch`es its deliverable without
///   writing it must not pass;
/// * **outside the molecule dir** — the entry is absolute or climbs out with
///   `..`. Such a path can never be satisfied by a real deliverable *inside*
///   the recovery window, so it is treated as unsatisfied rather than resolved
///   against something the `cs done` teardown will not preserve;
/// * **stale** — when `min_mtime` is given, the artifact (or, for a directory,
///   its freshest child) was last modified *before* the step started. A file
///   left over from a previous tackle is not proof that *this* turn did work.
///
/// The check is read-only and idempotent. Legacy steps that declare no
/// `acceptance_artifacts` yield an empty `expected` slice and thus an empty
/// result — backward-compatible.
#[must_use]
pub fn unsatisfied_expected_artifacts(
    mol_dir: &Path,
    expected: &[String],
    min_mtime: Option<SystemTime>,
) -> Vec<String> {
    expected
        .iter()
        .filter(|entry| !artifact_satisfied(mol_dir, entry, min_mtime))
        .cloned()
        .collect()
}

/// Whether a single declared `entry` is satisfied under `mol_dir`. See
/// [`unsatisfied_expected_artifacts`] for the four failure modes this rejects.
fn artifact_satisfied(mol_dir: &Path, entry: &str, min_mtime: Option<SystemTime>) -> bool {
    let rel = entry.trim_end_matches('/');
    // Containment: the declared path must resolve strictly *inside* `mol_dir`.
    // An absolute path or one climbing out with `..` can never be a deliverable
    // inside the recovery window, so it is unsatisfiable by construction.
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return false;
    }
    if Path::new(rel)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let path = mol_dir.join(rel);
    let Ok(canonical_root) = mol_dir.canonicalize() else {
        return false;
    };
    let mut cursor = canonical_root.clone();
    for component in Path::new(rel).components() {
        cursor.push(component.as_os_str());
        if std::fs::symlink_metadata(&cursor)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return false;
        }
    }
    let Ok(canonical_path) = path.canonicalize() else {
        return false;
    };
    if canonical_path == canonical_root || !canonical_path.starts_with(&canonical_root) {
        return false;
    }
    if entry.ends_with('/') {
        // Directory contract: exists, holds at least one entry, and — when a
        // step-start floor is given — at least one entry is fresher than it.
        let Ok(read) = std::fs::read_dir(&path) else {
            return false;
        };
        let mut saw_child = false;
        for child in read.flatten() {
            if child
                .file_type()
                .is_ok_and(|file_type| file_type.is_symlink())
            {
                continue;
            }
            saw_child = true;
            if mtime_at_or_after(&child.path(), min_mtime) {
                return true;
            }
        }
        // Non-empty but every child is stale (only reachable with a floor);
        // an empty directory falls through here too.
        saw_child && min_mtime.is_none()
    } else {
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => {
                min_mtime.is_none_or(|floor| meta.modified().is_ok_and(|m| m >= floor))
            }
            _ => false,
        }
    }
}

/// Whether `path`'s mtime is at or after `floor` (always true when no floor).
fn mtime_at_or_after(path: &Path, floor: Option<SystemTime>) -> bool {
    match floor {
        None => true,
        Some(floor) => {
            std::fs::metadata(path).is_ok_and(|m| m.modified().is_ok_and(|t| t >= floor))
        }
    }
}

/// Deliver the committee-posture pointer into `briefing.md`, when — and only
/// when — the molecule is a committee seat.
///
/// A committee seat carries its adversarial contract in the durable,
/// regeneration-stable
/// [`committee-posture.md`](cosmon_core::committee::COMMITTEE_POSTURE_FILE) file
/// that no lifecycle verb rewrites. This function appends the stable
/// [`committee_posture_reference`](cosmon_core::committee::committee_posture_reference)
/// pointer to whatever `briefing.md` currently holds, so the seat's persona
/// witness sees a briefing that *references* its contract — the second fact
/// [`AdversarialBriefing::from_durable_injection`](cosmon_core::committee::AdversarialBriefing::from_durable_injection)
/// tests, and the one that fails as `BriefingNotInjected`.
///
/// No-ops for ordinary molecules (no durable file → nothing to point at) and is
/// idempotent (skips when the pointer is already present), so calling it after
/// every briefing write never stacks duplicate stanzas.
///
/// # Why the name is not `reinstate`
///
/// It was, and the name was load-bearing in the wrong direction. Under that
/// name the function ran from exactly one place — the step-advance path below —
/// and *re-establishing* is precisely what it did: the contract provably
/// survived regeneration, while nothing asked whether it was there **before the
/// first regeneration**. Measured on 2026-07-28: immediately after `cs
/// nucleate` plus writing `committee-posture.md`, a seat's `briefing.md`
/// contained zero references to it, so the witness returned `injected = false`
/// for **every seat on its step 1** — the step on which the verdict contract
/// must be written, and the one a provider refusal is most likely to end on. A
/// verb named for restoration reads as complete once restoration works; a verb
/// named for delivery is unfinished anywhere a briefing is written and does not
/// call it (committee-20260728-1668 F1).
///
/// The same argument ran once more at the other end. `cs
/// complete` rewrites `briefing.md` down to a terse
/// `**Status:** COMPLETED` line, and it is the verb every seat ends with — so
/// without a third call site the pointer existed exactly while a seat was
/// running and was gone by the time anything read the seat's record, which is
/// the only time such a record is read. Measured on committee-20260728-2d37's
/// two seats on 2026-07-29: pointer present after `cs tackle`, `grep -c
/// committee-posture.md briefing.md == 0` after `cs complete`, both seats.
///
/// # The rule this function is under
///
/// **Every production write of `briefing.md` calls this immediately after** —
/// before the first step (`cs tackle`), at every advance (`cs evolve`), and
/// after the last (`cs complete`). The rule is not left to memory: the
/// `committee_posture_delivery_lint` suite reads the sources and fails on any
/// briefing write that neither calls this nor carries an inline
/// `committee-posture: exempt — <reason>` waiver on the write. `cs nucleate` is
/// one such waiver, and reading it is how you learn that a call there would be
/// a permanent no-op — nucleation mints the directory, so nothing can pre-exist
/// it.
///
/// # Errors
///
/// Returns an error only if the durable file exists (this *is* a committee
/// seat) but appending the pointer to `briefing.md` fails — a real I/O fault on
/// the seat's contract delivery, which must not pass silently.
pub fn deliver_committee_posture_reference(
    mol_dir: &Path,
    briefing_path: &Path,
) -> std::io::Result<()> {
    let posture_path = mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE);
    if !posture_path.exists() {
        // Not a committee seat — nothing to point at.
        return Ok(());
    }
    let reference = cosmon_core::committee::committee_posture_reference();
    let current = std::fs::read_to_string(briefing_path).unwrap_or_default();
    if current.contains(reference.trim()) {
        // Already delivered this step — stay idempotent.
        return Ok(());
    }
    let mut body = current;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(reference);
    // committee-posture: exempt — this write IS the delivery; calling itself
    // would recurse, and the idempotence check above already ran.
    std::fs::write(briefing_path, body).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("failed to deliver committee-posture pointer into briefing.md: {e}"),
        )
    })
}

/// Compute a [`BriefingSeal`] over `briefing.md`. Returns `None` if the
/// file cannot be read — the caller treats this as "no seal", never as
/// an error. Seal emission is a probe, not a lock.
///
/// The seal carries an immutable snapshot of the briefing **as it was at
/// this step**. cosmon regenerates `briefing.md` on every advance (and
/// `cs complete` rewrites it once more at the end), so verifying a
/// historical seal against the current file would flag cosmon's own
/// honest per-step rewrite as tampering. Snapshotting the content into
/// the molecule lets `cs verify` check each step's briefing against its
/// own epoch. Binary (non-UTF-8) briefings carry no text snapshot and
/// fall back to the live-file comparison at verify time.
#[must_use]
pub fn try_seal_briefing(briefing_path: &Path, step: u32) -> Option<BriefingSeal> {
    match std::fs::read(briefing_path) {
        Ok(bytes) => {
            let seal = BriefingSeal::of_text_or_bytes(step, &bytes);
            Some(match std::str::from_utf8(&bytes) {
                Ok(text) => seal.with_snapshot(text),
                Err(_) => seal,
            })
        }
        Err(e) => {
            eprintln!("warning: could not seal briefing.md: {e}");
            None
        }
    }
}
