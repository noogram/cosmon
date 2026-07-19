// SPDX-License-Identifier: AGPL-3.0-only

//! Replay `events.jsonl` → `state.json` cache rebuild.
//!
//! `events.jsonl` is the **source of truth** for cognitive history (append-only,
//! durable, seal-carrying). `state.json` is a **derivable cache** — the live
//! hot view the CLI reads on every invocation. When the cache is missing,
//! stale, or corrupt, [`project_molecules_from_events`] reconstructs every
//! molecule's state by folding the event stream.
//!
//! # Why rebuild exists
//!
//! The residence distinction:
//! *cross only `events.jsonl` across a residence boundary; rebuild `state.json`
//! on the receiving end*. This avoids the two-clock paradox (Hawking's
//! chronology protection) while preserving the BLAKE3 seals. `cs reconcile`
//! uses this module to materialize the cache from the narration channel.
//!
//! # Determinism
//!
//! The projection is purely functional over the event sequence: replaying the
//! same envelopes twice produces byte-identical [`MoleculeData`] (modulo
//! timestamps, which are embedded in the events themselves). Fields that no
//! event carries (e.g. `variables`, `tags`, `session_name`) materialise to
//! their default values — the rebuild is a disaster-recovery primitive, not
//! a perfect clone of the hot cache. See synthesis `surprise #1` and `R4`
//! in the delib for the full cache-vs-truth argument.
//!
//! # What we can project
//!
//! | Field | Source event |
//! |-------|-------------|
//! | `id`, `formula_id`, `created_at` | [`MoleculeNucleated`] |
//! | `typed_links` (parent, blocks) | [`MoleculeNucleated`] |
//! | `status` | [`MoleculeStatusChanged`], [`MoleculeCompleted`], [`MoleculeCollapsed`], [`MoleculeStuck`] |
//! | `current_step`, `total_steps`, `completed_steps` | [`MoleculeStepCompleted`] |
//! | `prompt_seal` | [`PromptSealed`] |
//! | `briefing_seals` | [`BriefingSealed`] |
//! | `bootstrap_seals` | [`BootstrapSealed`] |
//! | `merged_at` | [`MergeCompleted`] |
//! | `collapse_reason` | [`MoleculeCollapsed`] |
//! | `updated_at` | last event timestamp |
//!
//! Anything else (`variables`, `tags`, `project_id`, `kind`, `session_name`,
//! `assigned_worker`, `originating_branch`, `expires_at`, …) materialises to
//! the serde default. Operators who need the full hot cache contents should
//! rely on the synchronous `cs evolve` write path; the rebuild is a safety net.
//!
//! [`MoleculeNucleated`]: cosmon_core::event_v2::EventV2::MoleculeNucleated
//! [`MoleculeStatusChanged`]: cosmon_core::event_v2::EventV2::MoleculeStatusChanged
//! [`MoleculeCompleted`]: cosmon_core::event_v2::EventV2::MoleculeCompleted
//! [`MoleculeCollapsed`]: cosmon_core::event_v2::EventV2::MoleculeCollapsed
//! [`MoleculeStuck`]: cosmon_core::event_v2::EventV2::MoleculeStuck
//! [`MoleculeStepCompleted`]: cosmon_core::event_v2::EventV2::MoleculeStepCompleted
//! [`PromptSealed`]: cosmon_core::event_v2::EventV2::PromptSealed
//! [`BriefingSealed`]: cosmon_core::event_v2::EventV2::BriefingSealed
//! [`BootstrapSealed`]: cosmon_core::event_v2::EventV2::BootstrapSealed
//! [`MergeCompleted`]: cosmon_core::event_v2::EventV2::MergeCompleted

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_core::error::CosmonError;
use cosmon_core::event_v2::{Envelope, EventV2, StuckReason};
use cosmon_core::id::{FleetId, FormulaId, MoleculeId, StepId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;

use crate::briefing_seal::BriefingSeal;
use crate::event_log::read_all;
use crate::MoleculeData;

/// Outcome of a single-molecule rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildOutcome {
    /// The cache was absent; a fresh file was written from events.
    CreatedFromEvents,
    /// The cache was corrupt (non-UTF-8 or invalid JSON); the bad file
    /// was archived as `state.json.broken` and a fresh file was written.
    RecoveredFromCorruption,
    /// The cache was valid and consistent with the event log — no write.
    UpToDate,
    /// No molecule directory was found to seed the rebuild (empty galaxy,
    /// or molecule never nucleated in this event log).
    NoEventsForMolecule,
}

/// Fold a slice of `events.jsonl` envelopes into a projection per molecule.
///
/// The returned map keys are molecule IDs; values are the cache image that
/// would be written to `state.json`. Envelopes without a molecule association
/// (worker lifecycle, energy ticks) are ignored — they belong to the fleet
/// channel, not to any single molecule's projection.
///
/// The projection is idempotent: re-running it on the same input yields the
/// same output. This is the contract `cs reconcile` relies on when it asks
/// "is my cache consistent with the log?".
#[must_use]
pub fn project_molecules_from_events(envelopes: &[Envelope]) -> HashMap<MoleculeId, MoleculeData> {
    let mut out: HashMap<MoleculeId, MoleculeData> = HashMap::new();
    for env in envelopes {
        apply_event(env, &mut out);
    }
    out
}

/// Rebuild `state.json` for a single molecule from a global `events.jsonl`.
///
/// Reads the events file (if present), filters to envelopes that reference
/// `molecule_id`, projects them, and writes the result to `state_path`. If
/// `state_path` already exists and is parseable, the rebuild is a strict
/// no-op — the on-disk cache is the source of truth for the hot view and
/// MUST NOT be overwritten by the projection.
///
/// **Read-only on valid caches (architectural invariant).** The event-stream
/// projection is intentionally lossy: events do not carry `variables`,
/// `kind`, `project_id`, `session_name`, `tags`, or the formula's named
/// `StepId`s (event payloads use positional `step-N` ids only). If the
/// caller could overwrite a valid cache from the projection, every legacy
/// or schema-drifted molecule would lose its proof-of-work prompt data on
/// the next `cs reconcile`. That is the data-loss bug this guards against:
/// projecting over a valid cache drops fields the event log cannot replay.
/// The contract is therefore strict:
///
/// 1. `Missing` cache → write a fresh projection.
/// 2. `Corrupt` cache → archive as `state.json.broken`, write a fresh projection.
/// 3. `Valid` cache → return [`RebuildOutcome::UpToDate`], no I/O.
///
/// Detection of cache↔events drift on a valid cache is a separate concern —
/// see `cs verify` for the audit path. `cs reconcile` stays a pure
/// surface-projection command (docs/architectural-invariants.md §8 — write/
/// read asymmetry).
///
/// The caller provides both paths so tests and production code can share the
/// same routine — this module does no walk-up discovery.
///
/// # Errors
///
/// Returns [`CosmonError::StateStore`] if reading the event log or writing
/// the cache fails. A missing events file is **not** an error: if the log
/// does not exist, the rebuild returns [`RebuildOutcome::NoEventsForMolecule`]
/// so the caller can decide what to do (typically: leave the cache alone).
pub fn rebuild_molecule_state(
    events_path: &Path,
    molecule_id: &MoleculeId,
    state_path: &Path,
) -> Result<RebuildOutcome, CosmonError> {
    let outcome = classify_cache(state_path);

    match outcome {
        CacheState::Valid(_data) => {
            // INVARIANT — never overwrite a parseable cache from the
            // event projection. See the function docs above for the
            // task-20260523-5bd6 (voix) data-loss postmortem that
            // pinned this rule. Drift detection is `cs verify`'s job;
            // rebuild only resurrects missing or corrupt files.
            Ok(RebuildOutcome::UpToDate)
        }
        CacheState::Missing => {
            if !events_path.exists() {
                return Ok(RebuildOutcome::NoEventsForMolecule);
            }
            let envelopes = load_envelopes(events_path)?;
            if let Some(projected) = project_single(molecule_id, &envelopes) {
                write_state(state_path, &projected)?;
                Ok(RebuildOutcome::CreatedFromEvents)
            } else {
                Ok(RebuildOutcome::NoEventsForMolecule)
            }
        }
        CacheState::Corrupt => {
            // Read the corrupt bytes *before* archiving so we can salvage the
            // operator-set fields the event log cannot project (`variables`,
            // `tags`, `assigned_worker`, …). A cache is "corrupt" when the
            // strict `MoleculeData` deserialize fails — but that often means a
            // single field drifted while the rest of the JSON is intact. A
            // lenient `Value` parse recovers the survivors, so a transient
            // corruption next to a *running* molecule no longer wipes its
            // variables and worker assignment (the data-loss reported by
            // `delib-20260509-39ad`). See [`salvage_non_projectable`].
            let corrupt_bytes = std::fs::read(state_path).ok();
            archive_corrupt(state_path)?;
            if !events_path.exists() {
                return Ok(RebuildOutcome::NoEventsForMolecule);
            }
            let envelopes = load_envelopes(events_path)?;
            if let Some(mut projected) = project_single(molecule_id, &envelopes) {
                if let Some(bytes) = &corrupt_bytes {
                    salvage_non_projectable(bytes, &mut projected);
                }
                write_state(state_path, &projected)?;
                Ok(RebuildOutcome::RecoveredFromCorruption)
            } else {
                Ok(RebuildOutcome::NoEventsForMolecule)
            }
        }
    }
}

/// Discover which molecules exist on disk under `fleets_root` and rebuild
/// any whose `state.json` is missing or corrupt.
///
/// This is the "sweep the galaxy" entry point used by `cs reconcile`.
/// Up-to-date caches are left alone (so a round-trip through reconcile is
/// a no-op on a healthy galaxy). Molecules whose directory exists but
/// whose events never appeared in the log yield [`RebuildOutcome::NoEventsForMolecule`]
/// and are left alone — a rebuild without a source would be a silent lie.
///
/// Returns the list of (id, outcome) pairs in molecule-id order so `cs
/// reconcile` can print a deterministic report.
///
/// # Errors
///
/// Returns [`CosmonError::StateStore`] on filesystem failures. Individual
/// molecule failures short-circuit; the caller can retry after fixing the
/// cause.
pub fn rebuild_all_missing(
    events_path: &Path,
    fleets_root: &Path,
) -> Result<Vec<(MoleculeId, RebuildOutcome)>, CosmonError> {
    let mut results = Vec::new();
    if !fleets_root.is_dir() {
        return Ok(results);
    }
    let mut molecule_dirs: Vec<(MoleculeId, PathBuf)> = Vec::new();
    for fleet_entry in std::fs::read_dir(fleets_root)?.flatten() {
        let mols_dir = fleet_entry.path().join("molecules");
        if !mols_dir.is_dir() {
            continue;
        }
        for mol_entry in std::fs::read_dir(&mols_dir)?.flatten() {
            if !mol_entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = mol_entry.file_name().to_str().map(String::from) else {
                continue;
            };
            let Ok(id) = MoleculeId::new(name) else {
                continue;
            };
            molecule_dirs.push((id, mol_entry.path()));
        }
    }
    molecule_dirs.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    for (id, dir) in molecule_dirs {
        let state_path = dir.join("state.json");
        let outcome = rebuild_molecule_state(events_path, &id, &state_path)?;
        results.push((id, outcome));
    }
    Ok(results)
}

enum CacheState {
    Valid(Box<MoleculeData>),
    Missing,
    Corrupt,
}

fn classify_cache(state_path: &Path) -> CacheState {
    if !state_path.exists() {
        return CacheState::Missing;
    }
    let Ok(bytes) = std::fs::read(state_path) else {
        return CacheState::Corrupt;
    };
    match serde_json::from_slice::<MoleculeData>(&bytes) {
        Ok(data) => CacheState::Valid(Box::new(data)),
        Err(_) => CacheState::Corrupt,
    }
}

fn archive_corrupt(state_path: &Path) -> Result<(), CosmonError> {
    let broken = state_path.with_extension("json.broken");
    std::fs::rename(state_path, &broken).map_err(|e| CosmonError::StateStore {
        reason: format!(
            "failed to archive corrupt cache to {}: {e}",
            broken.display()
        ),
    })
}

fn load_envelopes(events_path: &Path) -> Result<Vec<Envelope>, CosmonError> {
    read_all(events_path).map_err(|e| CosmonError::StateStore {
        reason: format!("failed to read events log {}: {e}", events_path.display()),
    })
}

fn write_state(state_path: &Path, data: &MoleculeData) -> Result<(), CosmonError> {
    if let Some(parent) = state_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(data)?;
    let tmp = state_path.with_extension("json.tmp");
    std::fs::write(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, state_path)?;
    Ok(())
}

fn project_single(id: &MoleculeId, envelopes: &[Envelope]) -> Option<MoleculeData> {
    let mut projection = project_molecules_from_events(envelopes);
    projection.remove(id)
}

/// Best-effort recovery of the operator-set fields that no event carries.
///
/// The event log can project `status`, step counters, seals, and typed links,
/// but **not** `variables`, `tags`, `assigned_worker`, `assigned_role`,
/// `kind`, `class`, `session_name`, `originating_branch`, `project_id`,
/// `expires_at`, or `expiry_policy` (see the module table). When a `state.json`
/// is classified `Corrupt`, the strict `MoleculeData` deserialize failed — but
/// the JSON is frequently *mostly* intact (one field drifted, a trailing
/// truncation, a type that no longer matches). A lenient `serde_json::Value`
/// parse recovers whatever survived, and we graft those fields onto the
/// freshly projected molecule so a corrupt cache next to a live worker does
/// not silently lose its variables and worker assignment.
///
/// Every field is copied only when (a) the lenient parse yields a value of the
/// right shape and (b) it is non-empty — so an absent or unreadable field
/// leaves the projection's default in place. The function never fails: an
/// unparseable blob simply salvages nothing, matching the pre-salvage
/// behaviour.
fn salvage_non_projectable(corrupt_bytes: &[u8], projected: &mut MoleculeData) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(corrupt_bytes) else {
        return;
    };
    let Some(obj) = value.as_object() else {
        return;
    };

    // Helper: deserialize a named field into the target type, if present.
    // The target type is inferred from the assignment site, so no extra
    // imports are needed.
    macro_rules! salvage {
        ($key:literal => $target:expr) => {
            if let Some(v) = obj.get($key) {
                if let Ok(parsed) = serde_json::from_value(v.clone()) {
                    $target = parsed;
                }
            }
        };
        ($key:literal => $target:expr, non_empty: $is_empty:expr) => {
            if let Some(v) = obj.get($key) {
                if let Ok(parsed) = serde_json::from_value(v.clone()) {
                    if !$is_empty(&parsed) {
                        $target = parsed;
                    }
                }
            }
        };
    }

    salvage!("variables" => projected.variables, non_empty: |m: &std::collections::HashMap<String, String>| m.is_empty());
    salvage!("tags" => projected.tags, non_empty: |t: &std::collections::BTreeSet<_>| t.is_empty());
    salvage!("assigned_worker" => projected.assigned_worker, non_empty: |o: &Option<_>| o.is_none());
    salvage!("assigned_role" => projected.assigned_role, non_empty: |o: &Option<_>| o.is_none());
    salvage!("kind" => projected.kind, non_empty: |o: &Option<_>| o.is_none());
    salvage!("class" => projected.class);
    salvage!("session_name" => projected.session_name, non_empty: |o: &Option<_>| o.is_none());
    salvage!("originating_branch" => projected.originating_branch, non_empty: |o: &Option<_>| o.is_none());
    salvage!("project_id" => projected.project_id, non_empty: |o: &Option<_>| o.is_none());
    salvage!("expires_at" => projected.expires_at, non_empty: |o: &Option<_>| o.is_none());
    salvage!("expiry_policy" => projected.expiry_policy, non_empty: |o: &Option<_>| o.is_none());
}

/// "Consistent" here means: the status, step counter, seals, and `merged_at`
/// derivable from events match what the cache claims. Fields the event log
/// cannot know (variables, tags, `session_name`, …) are ignored — otherwise
/// every healthy cache would look stale on every reconcile.
///
/// Reserved for future `cs verify` audit work; the rebuild path itself
/// no longer consults this — see [`rebuild_molecule_state`] for the
/// read-only-on-valid-cache invariant.
#[allow(dead_code)]
fn is_consistent(cache: &MoleculeData, projected: Option<&MoleculeData>) -> bool {
    let Some(p) = projected else {
        return true;
    };
    cache.status == p.status
        && cache.current_step == p.current_step
        && cache.total_steps == p.total_steps
        && cache.completed_steps == p.completed_steps
        && cache.merged_at == p.merged_at
        && cache.prompt_seal == p.prompt_seal
        && cache.briefing_seals == p.briefing_seals
        && cache.bootstrap_seals == p.bootstrap_seals
}

#[allow(clippy::too_many_lines)]
fn apply_event(env: &Envelope, out: &mut HashMap<MoleculeId, MoleculeData>) {
    match &env.event {
        EventV2::MoleculeNucleated {
            molecule_id,
            formula_id,
            parent_id,
            blocks,
        } => {
            let mut data = empty_molecule_data(molecule_id.clone(), formula_id, env.timestamp);
            if let Some(parent) = parent_id {
                data.typed_links
                    .push(MoleculeLink::DecayedFrom { id: parent.clone() });
            }
            for target in blocks {
                data.typed_links.push(MoleculeLink::Blocks {
                    target: target.clone(),
                });
            }
            out.entry(molecule_id.clone()).or_insert(data);
        }
        EventV2::MoleculeStatusChanged {
            molecule_id, to, ..
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                if let Some(status) = parse_status(to) {
                    data.status = status;
                }
                data.updated_at = env.timestamp;
            }
        }
        EventV2::MoleculeStepCompleted {
            molecule_id,
            step,
            total,
            ..
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                data.total_steps = *total;
                let index = step.saturating_add(1);
                if index > data.current_step {
                    data.current_step = index;
                }
                let step_id = StepId::new(format!("step-{step}"))
                    .unwrap_or_else(|_| StepId::new("step").expect("non-empty"));
                if !data.completed_steps.contains(&step_id) {
                    data.completed_steps.push(step_id);
                }
                data.updated_at = env.timestamp;
            }
        }
        EventV2::MoleculeCompleted {
            molecule_id,
            reason,
            ..
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                data.status = MoleculeStatus::Completed;
                data.updated_at = env.timestamp;
                if data.collapse_reason.is_none() && !reason.is_empty() {
                    // Not a collapse — leave collapse_reason untouched.
                    let _ = reason;
                }
            }
        }
        EventV2::MoleculeCollapsed {
            molecule_id,
            reason,
            kind,
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                data.status = MoleculeStatus::Collapsed;
                data.collapse_reason = Some(reason.clone());
                if data.collapse_reason_kind.is_none() {
                    data.collapse_reason_kind.clone_from(kind);
                }
                data.updated_at = env.timestamp;
            }
        }
        EventV2::MoleculeStuck {
            molecule_id,
            reason,
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                // Stuck does not map to a first-class status — it stays
                // `Running` in the cache. Record the reason so the verifier
                // has a trace.
                let _ = reason;
                let _: &StuckReason = reason;
                data.updated_at = env.timestamp;
            }
        }
        EventV2::PromptSealed {
            molecule_id,
            hash,
            sealed_at,
            bytes,
            canonical_version,
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                data.prompt_seal = Some(BriefingSeal {
                    step: 0,
                    hash: hash.clone(),
                    sealed_at: *sealed_at,
                    briefing_bytes: *bytes,
                    canonical_version: *canonical_version,
                });
            }
        }
        EventV2::BriefingSealed {
            molecule_id,
            step,
            hash,
            sealed_at,
            bytes,
            canonical_version,
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                data.briefing_seals.push(BriefingSeal {
                    step: *step,
                    hash: hash.clone(),
                    sealed_at: *sealed_at,
                    briefing_bytes: *bytes,
                    canonical_version: *canonical_version,
                });
            }
        }
        EventV2::BootstrapSealed {
            molecule_id,
            step,
            hash,
            sealed_at,
            bytes,
            canonical_version,
        } => {
            if let Some(data) = out.get_mut(molecule_id) {
                data.bootstrap_seals.push(BriefingSeal {
                    step: *step,
                    hash: hash.clone(),
                    sealed_at: *sealed_at,
                    briefing_bytes: *bytes,
                    canonical_version: *canonical_version,
                });
            }
        }
        EventV2::MergeCompleted { molecule, .. } => {
            if let Some(data) = out.get_mut(molecule) {
                data.merged_at = Some(env.timestamp);
                data.updated_at = env.timestamp;
            }
        }
        EventV2::DecaySpliced { parent, children } => {
            if let Some(data) = out.get_mut(parent) {
                for child in children {
                    data.typed_links
                        .push(MoleculeLink::DecayProduct { id: child.clone() });
                }
                data.updated_at = env.timestamp;
            }
        }
        // Everything else (worker lifecycle, energy ticks, gate/native
        // telemetry, expiry, resurrection, harvest) does not project into
        // `MoleculeData`. `EventV2` is `#[non_exhaustive]`, so the
        // wildcard covers today's irrelevant variants AND any future
        // variant that lands without an explicit arm — silent by default
        // keeps the rebuild safe during ADR-era churn.
        _ => {}
    }
}

fn parse_status(s: &str) -> Option<MoleculeStatus> {
    serde_json::from_str::<MoleculeStatus>(&format!("\"{s}\"")).ok()
}

fn empty_molecule_data(
    id: MoleculeId,
    formula_id: &str,
    created_at: DateTime<Utc>,
) -> MoleculeData {
    let fleet_id = FleetId::new("default").expect("non-empty");
    let formula = FormulaId::new(formula_id)
        .unwrap_or_else(|_| FormulaId::new("unknown").expect("non-empty"));
    MoleculeData {
        id,
        fleet_id,
        formula_id: formula,
        status: MoleculeStatus::Pending,
        variables: HashMap::new(),
        assigned_worker: None,
        created_at,
        updated_at: created_at,
        total_steps: 0,
        current_step: 0,
        completed_steps: Vec::new(),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventLogWriter;
    use tempfile::tempdir;

    fn mid(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    fn write_log(path: &Path, events: Vec<EventV2>) {
        let mut w = EventLogWriter::open(path).unwrap();
        for ev in events {
            w.emit(ev, None).unwrap();
        }
        w.sync().unwrap();
    }

    #[test]
    fn project_reconstructs_formula_and_status_for_nucleated_molecule() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let id = mid("task-20260420-0001");
        write_log(
            &path,
            vec![
                EventV2::MoleculeNucleated {
                    molecule_id: id.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                EventV2::MoleculeStatusChanged {
                    molecule_id: id.clone(),
                    from: "pending".into(),
                    to: "running".into(),
                },
            ],
        );

        let envs = read_all(&path).unwrap();
        let projection = project_molecules_from_events(&envs);
        let data = projection.get(&id).expect("molecule projected");
        assert_eq!(data.formula_id.as_str(), "task-work");
        assert_eq!(data.status, MoleculeStatus::Running);
    }

    #[test]
    fn project_counts_completed_steps_without_duplicates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let id = mid("task-20260420-0002");
        write_log(
            &path,
            vec![
                EventV2::MoleculeNucleated {
                    molecule_id: id.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                EventV2::MoleculeStepCompleted {
                    molecule_id: id.clone(),
                    step: 0,
                    total: 2,
                    duration_ms: None,
                    step_hash: None,
                },
                EventV2::MoleculeStepCompleted {
                    molecule_id: id.clone(),
                    step: 0, // duplicate — replay tolerance
                    total: 2,
                    duration_ms: None,
                    step_hash: None,
                },
                EventV2::MoleculeStepCompleted {
                    molecule_id: id.clone(),
                    step: 1,
                    total: 2,
                    duration_ms: None,
                    step_hash: None,
                },
            ],
        );
        let envs = read_all(&path).unwrap();
        let projection = project_molecules_from_events(&envs);
        let data = projection.get(&id).unwrap();
        assert_eq!(data.current_step, 2);
        assert_eq!(data.total_steps, 2);
        assert_eq!(data.completed_steps.len(), 2);
    }

    #[test]
    fn project_captures_seal_events_into_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let id = mid("task-20260420-0003");
        let now = Utc::now();
        write_log(
            &path,
            vec![
                EventV2::MoleculeNucleated {
                    molecule_id: id.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                EventV2::PromptSealed {
                    molecule_id: id.clone(),
                    hash: "deadbeef".into(),
                    sealed_at: now,
                    bytes: 42,
                    canonical_version: 0,
                },
                EventV2::BriefingSealed {
                    molecule_id: id.clone(),
                    step: 0,
                    hash: "cafe".into(),
                    sealed_at: now,
                    bytes: 99,
                    canonical_version: 1,
                },
            ],
        );
        let envs = read_all(&path).unwrap();
        let data = project_molecules_from_events(&envs)
            .remove(&id)
            .expect("molecule present");
        let seal = data.prompt_seal.expect("prompt seal captured");
        assert_eq!(seal.hash, "deadbeef");
        assert_eq!(seal.briefing_bytes, 42);
        assert_eq!(data.briefing_seals.len(), 1);
        assert_eq!(data.briefing_seals[0].hash, "cafe");
    }

    #[test]
    fn projection_is_deterministic_across_reads() {
        // Same events.jsonl ⇒ identical cache JSON on two rebuilds.
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let id = mid("task-20260420-0004");
        write_log(
            &path,
            vec![
                EventV2::MoleculeNucleated {
                    molecule_id: id.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                EventV2::MoleculeStepCompleted {
                    molecule_id: id.clone(),
                    step: 0,
                    total: 1,
                    duration_ms: None,
                    step_hash: None,
                },
                EventV2::MoleculeCompleted {
                    molecule_id: id.clone(),
                    duration_ms: None,
                    reason: "ok".into(),
                },
            ],
        );
        let envs = read_all(&path).unwrap();
        let a = project_molecules_from_events(&envs).remove(&id).unwrap();
        let b = project_molecules_from_events(&envs).remove(&id).unwrap();
        let ja = serde_json::to_string_pretty(&a).unwrap();
        let jb = serde_json::to_string_pretty(&b).unwrap();
        assert_eq!(ja, jb, "two replays must produce identical bytes");
    }

    #[test]
    fn rebuild_creates_cache_when_missing() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id = mid("task-20260420-0005");
        write_log(
            &events_path,
            vec![EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            }],
        );
        let mol_dir = dir.path().join("mol");
        std::fs::create_dir_all(&mol_dir).unwrap();
        let state_path = mol_dir.join("state.json");
        assert!(!state_path.exists());

        let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
        assert_eq!(outcome, RebuildOutcome::CreatedFromEvents);
        assert!(state_path.exists(), "state.json materialized from events");
        let bytes = std::fs::read(&state_path).unwrap();
        let data: MoleculeData = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(data.id, id);
        assert_eq!(data.formula_id.as_str(), "task-work");
    }

    #[test]
    fn rebuild_archives_corrupt_cache() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id = mid("task-20260420-0006");
        write_log(
            &events_path,
            vec![EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            }],
        );
        let mol_dir = dir.path().join("mol");
        std::fs::create_dir_all(&mol_dir).unwrap();
        let state_path = mol_dir.join("state.json");
        std::fs::write(&state_path, b"not json {{{{").unwrap();

        let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
        assert_eq!(outcome, RebuildOutcome::RecoveredFromCorruption);
        let broken = state_path.with_extension("json.broken");
        assert!(broken.exists(), "corrupt cache archived as .broken");
        let data: MoleculeData =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(data.id, id);
    }

    /// Recovering a *corrupt* cache must not silently wipe the operator-set
    /// fields the event log cannot project (`variables`, `tags`,
    /// `session_name`, …). A cache is "corrupt" the moment a single field
    /// drifts — here `total_steps` is poisoned to a string — but the rest of
    /// the JSON is intact, so a lenient salvage recovers the survivors. This
    /// is the anti-regression for that class of data loss: a transient
    /// corruption next to a running molecule keeps its variables and
    /// worker assignment.
    #[test]
    fn rebuild_salvages_non_projectable_fields_from_corrupt_cache() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id = mid("task-20260509-d0d0");
        write_log(
            &events_path,
            vec![
                EventV2::MoleculeNucleated {
                    molecule_id: id.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                EventV2::MoleculeStatusChanged {
                    molecule_id: id.clone(),
                    from: "pending".into(),
                    to: "running".into(),
                },
            ],
        );

        // Build a *healthy* cache from events, then enrich it with the
        // operator fields no event carries.
        let envs = read_all(&events_path).unwrap();
        let mut healthy = project_molecules_from_events(&envs).remove(&id).unwrap();
        healthy
            .variables
            .insert("topic".into(), "fix-the-bug".into());
        healthy
            .variables
            .insert("surface_path".into(), "STATUS.md".into());
        healthy.session_name = Some("fix-bug-d0d0".into());
        healthy.originating_branch = Some("feat/task-20260509-d0d0".into());

        // Poison one field's type so the strict `MoleculeData` deserialize
        // fails (→ classified Corrupt) while the rest stays salvageable.
        let mut value = serde_json::to_value(&healthy).unwrap();
        value["total_steps"] = serde_json::Value::String("not-a-number".into());
        let corrupt = serde_json::to_string_pretty(&value).unwrap();
        assert!(
            serde_json::from_str::<MoleculeData>(&corrupt).is_err(),
            "the poisoned cache must fail strict deserialize"
        );

        let mol_dir = dir.path().join("mol");
        std::fs::create_dir_all(&mol_dir).unwrap();
        let state_path = mol_dir.join("state.json");
        std::fs::write(&state_path, &corrupt).unwrap();

        let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
        assert_eq!(outcome, RebuildOutcome::RecoveredFromCorruption);

        let data: MoleculeData =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        // Operator fields salvaged from the corrupt blob.
        assert_eq!(
            data.variables.get("topic"),
            Some(&"fix-the-bug".to_owned()),
            "variables must survive corrupt-cache recovery"
        );
        assert_eq!(
            data.variables.get("surface_path"),
            Some(&"STATUS.md".to_owned())
        );
        assert_eq!(data.session_name.as_deref(), Some("fix-bug-d0d0"));
        assert_eq!(
            data.originating_branch.as_deref(),
            Some("feat/task-20260509-d0d0")
        );
        // Status is re-projected from events — the running molecule stays running.
        assert_eq!(data.status, MoleculeStatus::Running);
    }

    /// When the corrupt blob is total garbage (not even valid JSON), salvage
    /// recovers nothing — matching the pre-salvage behaviour. The molecule is
    /// still rebuilt from events; only the non-projectable fields default.
    #[test]
    fn rebuild_salvage_is_noop_on_unparseable_corrupt_cache() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id = mid("task-20260509-d0d1");
        write_log(
            &events_path,
            vec![EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            }],
        );
        let mol_dir = dir.path().join("mol");
        std::fs::create_dir_all(&mol_dir).unwrap();
        let state_path = mol_dir.join("state.json");
        std::fs::write(&state_path, b"\x00\x01 not json at all {{{").unwrap();

        let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
        assert_eq!(outcome, RebuildOutcome::RecoveredFromCorruption);
        let data: MoleculeData =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(data.id, id);
        assert!(data.variables.is_empty());
        assert!(data.session_name.is_none());
    }

    #[test]
    fn rebuild_is_noop_when_cache_matches_log() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id = mid("task-20260420-0007");
        write_log(
            &events_path,
            vec![EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            }],
        );
        let mol_dir = dir.path().join("mol");
        std::fs::create_dir_all(&mol_dir).unwrap();
        let state_path = mol_dir.join("state.json");

        // First rebuild creates the cache from events.
        rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
        let mtime_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();

        // Second rebuild should be a no-op — same outcome, no rewrite.
        let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
        assert_eq!(outcome, RebuildOutcome::UpToDate);
        let mtime_after = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "no rewrite on consistent cache");
    }

    #[test]
    fn rebuild_all_missing_sweeps_fleet_tree() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id_a = mid("task-20260420-000a");
        let id_b = mid("task-20260420-000b");
        write_log(
            &events_path,
            vec![
                EventV2::MoleculeNucleated {
                    molecule_id: id_a.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                EventV2::MoleculeNucleated {
                    molecule_id: id_b.clone(),
                    formula_id: "deep-think".into(),
                    parent_id: None,
                    blocks: vec![],
                },
            ],
        );
        let fleets_root = dir.path().join("fleets");
        let mol_a = fleets_root
            .join("default")
            .join("molecules")
            .join(id_a.as_str());
        let mol_b = fleets_root
            .join("default")
            .join("molecules")
            .join(id_b.as_str());
        std::fs::create_dir_all(&mol_a).unwrap();
        std::fs::create_dir_all(&mol_b).unwrap();

        let results = rebuild_all_missing(&events_path, &fleets_root).unwrap();
        assert_eq!(results.len(), 2, "both molecules discovered");
        for (_, outcome) in &results {
            assert_eq!(*outcome, RebuildOutcome::CreatedFromEvents);
        }
        assert!(mol_a.join("state.json").exists());
        assert!(mol_b.join("state.json").exists());
    }

    /// **Regression test for cache-vs-projection data loss.**
    ///
    /// A `Valid` cache that disagrees with the event projection on
    /// fields the projection cannot reconstruct (variables, kind,
    /// `project_id`, `session_name`, formula's named `StepId`s) MUST NOT be
    /// overwritten. The bug previously sat in [`rebuild_molecule_state`]:
    /// when `is_consistent` returned `false` because the cache carried
    /// named `completed_steps = [implement, verify]` while events
    /// emitted positional `[step-0, step-1]`, the function silently
    /// wrote the lossy projection on top of the cache, stripping every
    /// operator-visible field. the voicelab galaxy lost variables from 37
    /// molecules this way before the bug was caught.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn rebuild_never_overwrites_valid_cache_with_lossy_projection() {
        use std::collections::{BTreeSet, HashMap};

        use chrono::Utc;
        use cosmon_core::id::{FleetId, FormulaId, ProjectId};
        use cosmon_core::kind::MoleculeKind;
        use cosmon_core::molecule::MoleculeStatus;

        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id = mid("task-20260419-vox1");

        // Events disagree with the cache on completed_steps shape AND
        // carry only the bare structural facts — the projection's
        // variables/kind/project_id/session_name are empty.
        write_log(
            &events_path,
            vec![
                EventV2::MoleculeNucleated {
                    molecule_id: id.clone(),
                    formula_id: "task-work".into(),
                    parent_id: None,
                    blocks: vec![],
                },
                EventV2::MoleculeStepCompleted {
                    molecule_id: id.clone(),
                    step: 0,
                    total: 2,
                    duration_ms: None,
                    step_hash: None,
                },
            ],
        );

        // Hand-craft a cache that holds operator-visible payload (variables,
        // kind, project_id, session_name) and a named StepId in
        // completed_steps — the voix shape.
        let mut variables = HashMap::new();
        variables.insert("topic".into(), "voix bootstrap".into());
        variables.insert("detail".into(), "long-form prose payload".into());
        let cache = MoleculeData {
            id: id.clone(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Running,
            variables,
            assigned_worker: None,
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
            kind: Some(MoleculeKind::Task),
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: Some(ProjectId::new("voix-9999").unwrap()),
            assigned_role: None,
            session_name: Some("voix-task-vox1".into()),
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
        };

        let mol_dir = dir.path().join("mol");
        std::fs::create_dir_all(&mol_dir).unwrap();
        let state_path = mol_dir.join("state.json");
        std::fs::write(&state_path, serde_json::to_string_pretty(&cache).unwrap()).unwrap();
        let bytes_before = std::fs::read(&state_path).unwrap();
        let mtime_before = std::fs::metadata(&state_path).unwrap().modified().unwrap();

        let outcome = rebuild_molecule_state(&events_path, &id, &state_path).unwrap();
        assert_eq!(
            outcome,
            RebuildOutcome::UpToDate,
            "valid cache must report UpToDate even when projection disagrees"
        );

        // The file on disk must be byte-identical — no silent rewrite.
        let bytes_after = std::fs::read(&state_path).unwrap();
        assert_eq!(
            bytes_before, bytes_after,
            "valid cache must not be touched by rebuild (voix data-loss regression)"
        );
        let mtime_after = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "mtime must not advance");

        // And, doubly so: the operator-visible fields must still be there.
        let after: MoleculeData =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(
            after.variables.get("topic").map(String::as_str),
            Some("voix bootstrap")
        );
        assert_eq!(after.kind, Some(MoleculeKind::Task));
        assert_eq!(
            after.project_id.as_ref().map(ProjectId::as_str),
            Some("voix-9999")
        );
        assert_eq!(after.session_name.as_deref(), Some("voix-task-vox1"));
        assert_eq!(
            after.completed_steps,
            vec![StepId::new("implement").unwrap()]
        );
    }

    /// `rebuild_all_missing` must respect the same read-only-on-valid
    /// contract: walking the fleet tree never touches `state.json` files
    /// that parse cleanly, even when their content disagrees with the
    /// event projection.
    #[test]
    fn rebuild_all_missing_does_not_touch_valid_caches() {
        use std::collections::{BTreeSet, HashMap};

        use chrono::Utc;
        use cosmon_core::id::{FleetId, FormulaId};
        use cosmon_core::molecule::MoleculeStatus;

        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let id = mid("task-20260419-vox2");
        write_log(
            &events_path,
            vec![EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: None,
                blocks: vec![],
            }],
        );

        let fleets_root = dir.path().join("fleets");
        let mol_dir = fleets_root
            .join("default")
            .join("molecules")
            .join(id.as_str());
        std::fs::create_dir_all(&mol_dir).unwrap();
        let state_path = mol_dir.join("state.json");

        let mut variables = HashMap::new();
        variables.insert("topic".into(), "preserved".into());
        let cache = MoleculeData {
            id: id.clone(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Running,
            variables,
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 2,
            current_step: 0,
            completed_steps: Vec::new(),
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
        };
        std::fs::write(&state_path, serde_json::to_string_pretty(&cache).unwrap()).unwrap();
        let bytes_before = std::fs::read(&state_path).unwrap();

        let results = rebuild_all_missing(&events_path, &fleets_root).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, RebuildOutcome::UpToDate);
        let bytes_after = std::fs::read(&state_path).unwrap();
        assert_eq!(
            bytes_before, bytes_after,
            "sweep must not write any valid state.json"
        );
    }

    #[test]
    fn projection_captures_parent_and_blocks_links() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let parent = mid("task-20260420-00p0");
        let id = mid("task-20260420-00c0");
        let child = mid("task-20260420-00c1");
        write_log(
            &path,
            vec![EventV2::MoleculeNucleated {
                molecule_id: id.clone(),
                formula_id: "task-work".into(),
                parent_id: Some(parent.clone()),
                blocks: vec![child.clone()],
            }],
        );
        let envs = read_all(&path).unwrap();
        let data = project_molecules_from_events(&envs).remove(&id).unwrap();
        assert_eq!(data.blocks(), vec![&child]);
        let decayed = data
            .typed_links
            .iter()
            .any(|l| matches!(l, MoleculeLink::DecayedFrom { id } if *id == parent));
        assert!(decayed, "parent link recorded as DecayedFrom");
    }
}
