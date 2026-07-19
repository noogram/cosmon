// SPDX-License-Identifier: AGPL-3.0-only

//! Stable callsites for the Worker-Spawn Port event family (ADR-097).
//!
//! Five free functions, one per [`EventV2`] variant, that adapters call
//! to emit the IFBDD trail for the Worker-Spawn perimeter. Today the
//! only adapter is the claude-tmux path in
//! `cosmon_transport::claude`; a future adapter (`aider`, an API
//! adapter, …) will call the same helpers without forcing a change to
//! the adapter's own surface — that is the callsite-stability
//! discipline.
//!
//! Each helper is best-effort but **not silent**. The hot path must
//! not fail because telemetry is unhappy; a write or serialise error
//! is logged-but-non-fatal:
//!
//! - A sidecar `events.error.jsonl` next to the canonical
//!   `events.jsonl` records the failed envelope so a retrospective
//!   audit still has the bytes (ENOSPC on the canonical log must not
//!   erase the WS-* lineage);
//! - An in-memory error counter ([`emit_error_count`]) is incremented
//!   so a cat-test can assert "the sidecar landed *and* the counter
//!   ticked";
//! - The first failure of the process writes one line to stderr so an
//!   operator watching logs sees the symptom loud without it spamming
//!   every subsequent call.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use cosmon_core::event_v2::{
    AdapterHandleState, AdapterProbeKind, AdapterProbeResult, AdapterSelectionSource, EventV2,
    LoopOwnershipTag, ModelSelectionSource, PerturbationChannel,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::model_realization::ModelObservationSource;
use cosmon_core::spawn_seam::LoopOwnership;

use crate::event_log::{emit_one, resolve_events_log_path};

/// Process-wide counter of Worker-Spawn Port emissions that the
/// canonical `events.jsonl` path refused.
///
/// Incremented every time [`write_event`] catches an `emit_one` error
/// (ENOSPC, ENOENT, permission denied, …). The hot path stays
/// non-blocking; the counter is the audit handle for "how many WS-*
/// envelopes did we lose this process?" without parsing logs.
///
/// Visible to tests via [`emit_error_count`].
static EMIT_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);

/// One-shot guard for the loud `tracing::error!` on the first emit
/// failure — keeps subsequent failures silent so the log does not
/// flood when the underlying disk is full or read-only.
static FIRST_EMIT_ERROR_LOGGED: AtomicBool = AtomicBool::new(false);

/// Snapshot the process-wide emit-error counter.
///
/// Test-facing: paired with the sidecar file at
/// `<state_dir>/events.error.jsonl`, the counter answers "did the
/// canonical write fail?" without requiring the test to parse the
/// sidecar.
#[must_use]
pub fn emit_error_count() -> u64 {
    EMIT_ERROR_COUNT.load(Ordering::Relaxed)
}

/// **Test-only** — reset the in-memory counter + first-error guard so
/// a regression test can pin a clean baseline before driving an
/// induced ENOSPC.
///
/// Marked `#[doc(hidden)]` rather than `cfg(test)` so integration
/// tests in sibling crates (notably
/// `cosmon-transport/tests/cross_adapter_*`) can call it without
/// growing a feature flag.
#[doc(hidden)]
pub fn reset_emit_error_counters_for_tests() {
    EMIT_ERROR_COUNT.store(0, Ordering::Relaxed);
    FIRST_EMIT_ERROR_LOGGED.store(false, Ordering::Relaxed);
}

/// Emit an [`EventV2::WorkerSpawnAttempted`] (ADR-097 / WS-1).
///
/// Called by the adapter spawn path immediately *before* the
/// underlying backend spawn call. Records the spawn intent so an
/// audit query of the form
/// `jq -c 'select(.type == "worker_spawn_attempted")'` can answer
/// "did the adapter even try?" without parsing tmux output or
/// scraping logs.
///
/// `pre_existing_worker` is `Some` only when the adapter detected a
/// tmux session collision under the target name before spawning;
/// `None` in the normal path.
///
/// The hot path must not fail because telemetry is unhappy: write
/// errors are silently swallowed.
///
/// # Example — karpathy cat-test (§14 badge)
///
/// After this PR lands and one `cs tackle … --tag temp:hot` cycle
/// runs, the operator can read the IFBDD trail without any
/// cosmon-specific tool — just `cat` and `jq`:
///
/// ```text
/// // The shell-level cat-test:
/// //   cat .cosmon/state/fleets/default/molecules/<mol_id>/events.jsonl \
/// //     | jq -c 'select(.type | startswith("worker_spawn") or
/// //                            startswith("adapter_"))'
/// //
/// // returns the lineage (Attempted → Probed → BriefingConsumed →
/// // Reconciled) emitted by the four wired emit-sites in claude.rs.
/// ```
///
/// The query relies on the snake-case `type` discriminator that the
/// `EventV2` serde tag attribute writes. The schema invariant is
/// covered by
/// `cosmon-core/tests/event_v2_worker_spawn_roundtrip.rs::worker_spawn_port_variants_use_snake_case_type_discriminator`.
// The argument list mirrors the WS-1 field shape (ADR-097): bundling
// into a struct would defeat the callsite-stability discipline the
// briefing names — adapters call the free function by name, and the
// number of parameters reflects the audit-relevant fields the
// variant carries.
#[allow(clippy::too_many_arguments)]
pub fn emit_worker_spawn_attempted(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    worktree_path: &str,
    invocation_uuid: &str,
    pid: u32,
    pre_existing_worker: Option<&WorkerId>,
) {
    let event = EventV2::WorkerSpawnAttempted {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        worktree_path: worktree_path.to_owned(),
        invocation_uuid: invocation_uuid.to_owned(),
        pid,
        pre_existing_worker: pre_existing_worker.cloned(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::AdapterLivenessProbed`] (ADR-097 / WS-2).
///
/// Called by the adapter on every liveness check (`check_alive`,
/// pane-signature inspection, future API handshake). Carries both
/// the probe kind (what was watched) and the probe result (alive
/// with evidence / stuck with reason), so a downstream silence-
/// detection patrol can act on absences without reparsing the
/// signal.
#[allow(clippy::too_many_arguments)]
pub fn emit_adapter_liveness_probed(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    probe_kind: AdapterProbeKind,
    probe_result: AdapterProbeResult,
    elapsed_since_last_advance_ms: u64,
) {
    let event = EventV2::AdapterLivenessProbed {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        probe_kind,
        probe_result,
        elapsed_since_last_advance_ms,
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::AdapterPaneSignatureChecked`] (ADR-097 / WS-3).
///
/// Called at the propulsion / whisper perturbation gate (ADR-038)
/// before sending bytes to a worker pane. The variant exists in
/// C2; emit-sites in `readiness.rs` and the perturbation gates are
/// wired in C3 (PR-2). The helper is published now so C3 can wire
/// it without a schema change.
#[allow(clippy::too_many_arguments)]
pub fn emit_adapter_pane_signature_checked(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    registered_signature: &[String],
    observed_command: &str,
    matched: bool,
    channel: PerturbationChannel,
) {
    let event = EventV2::AdapterPaneSignatureChecked {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        registered_signature: registered_signature.to_vec(),
        observed_command: observed_command.to_owned(),
        matched,
        channel,
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::AdapterBriefingConsumed`] (ADR-097 / WS-4).
///
/// Called by the spawn path when the adapter reads `briefing.md` to
/// build the worker's initial prompt. `briefing_seal_observed` is
/// the hash the adapter computed over the bytes it actually read;
/// `briefing_seal_recorded` is the seal previously written to
/// `MoleculeData::briefing_seals` for the current step. Disagreement
/// between the two is WS-4's silent-failure mode.
#[allow(clippy::too_many_arguments)]
pub fn emit_adapter_briefing_consumed(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    briefing_path: &str,
    briefing_seal_observed: &str,
    briefing_seal_recorded: &str,
    bytes_read: u64,
    consumed_at: DateTime<Utc>,
) {
    let event = EventV2::AdapterBriefingConsumed {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        briefing_path: briefing_path.to_owned(),
        briefing_seal_observed: briefing_seal_observed.to_owned(),
        briefing_seal_recorded: briefing_seal_recorded.to_owned(),
        bytes_read,
        consumed_at,
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::AdapterHandleReconciled`] (ADR-097 / WS-5).
///
/// Called at every adapter-side teardown (`kill_session`, harvest,
/// patrol cleanup). `gap_ms` is computed by the caller as the
/// signed millisecond delta between `underlying_exit_observed_at`
/// and `handle_released_at` — positive when the handle outlived
/// the process, negative when the handle was released before the
/// exit was observed, `0` when `underlying_exit_observed_at` is
/// `None`.
#[allow(clippy::too_many_arguments)]
pub fn emit_adapter_handle_reconciled(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    handle_state: AdapterHandleState,
    underlying_exit_observed_at: Option<DateTime<Utc>>,
    handle_released_at: DateTime<Utc>,
    gap_ms: i64,
) {
    let event = EventV2::AdapterHandleReconciled {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        handle_state,
        underlying_exit_observed_at,
        handle_released_at,
        gap_ms,
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::AdapterSelected`] (ADR-097 / C6, ADR-103).
///
/// Called by `cs tackle` (transactional core) at every invocation —
/// before the adapter dispatch table is consulted — to record
/// *which* adapter was chosen, *where* the choice came from
/// ([`AdapterSelectionSource`]), and which [`LoopOwnership`] axis
/// applies. The triple is the substrate for the cat-test (§14
/// badge): a `jq` query over `events.jsonl` answers "did `--adapter
/// aider` actually route through?" and "did the loop run external
/// or in-process?" without correlating against the operator's shell
/// history.
///
/// `role_hint` is the forensic-only role-of-origin propagated by a
/// driver (the academy-shim's `--role researcher` becomes
/// `role_hint: Some("researcher")` here). `None` for direct operator
/// invocations.
///
/// `loop_ownership` is the per-Adapter axis returned by
/// [`cosmon_core::spawn_seam::validate_adapter_name`]; it travels
/// from the validator through the dispatch site so the event log
/// carries the byte sequence that traversed validation rather than
/// re-deriving it from a string allowlist (ADR-103 §Decision).
///
/// The hot path must not fail because telemetry is unhappy: write
/// errors are silently swallowed (same discipline as the other four
/// Worker-Spawn helpers).
pub fn emit_adapter_selected(
    state_dir: &Path,
    mol_id: &MoleculeId,
    adapter_name: &str,
    selection_source: AdapterSelectionSource,
    role_hint: Option<&str>,
    loop_ownership: LoopOwnership,
) {
    let event = EventV2::AdapterSelected {
        mol_id: mol_id.clone(),
        adapter_name: adapter_name.to_owned(),
        selected_at: Utc::now(),
        selection_source,
        role_hint: role_hint.map(ToOwned::to_owned),
        loop_ownership: LoopOwnershipTag::from(loop_ownership),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::ModelSelected`] (delib-20260704-b476 / C2).
///
/// Called by `cs tackle` (transactional core) right after the per-molecule
/// **model** pin is resolved — the model sibling of [`emit_adapter_selected`],
/// co-minted with the spawn and *before* the availability probe runs, so the
/// attribution is ex-ante and deterministic. Records *which* model was pinned
/// (`None` when nothing pinned one and the adapter's own default applies) and
/// *where* the choice came from ([`ModelSelectionSource`]).
///
/// This promotes the old `model-selection.json` sidecar attribution onto the
/// wire: a `jq` fold over `events.jsonl` answers "which model ran for this
/// molecule, and why?" — and the future ceiling guard counts strong
/// dispatches as a projection over the log rather than a mutable counter file.
///
/// The hot path must not fail because telemetry is unhappy: write errors are
/// silently swallowed (same trace-not-lock discipline as the other
/// Worker-Spawn helpers).
pub fn emit_model_selected(
    state_dir: &Path,
    mol_id: &MoleculeId,
    adapter_name: &str,
    model: Option<&str>,
    selection_source: ModelSelectionSource,
) {
    let event = EventV2::ModelSelected {
        mol_id: mol_id.clone(),
        adapter_name: adapter_name.to_owned(),
        model: model.map(ToOwned::to_owned),
        selection_source,
        selected_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::ModelObserved`] (delib-20260718-c70e / realized-model).
///
/// The ex-post empirical sibling of [`emit_model_selected`]: `ModelSelected`
/// records the model *intention* (the pin, ex-ante); this records the
/// *realization* — the concrete id an adapter reported running, read from its
/// fiable side-channel (`cosmon_core::model_realization`).
///
/// `model` is a **bare `&str`**, never optional: this helper is called *only*
/// when a concrete id was observed, so silence is expressed by not calling it.
/// That makes the honesty invariant structural — there is no `ModelObserved`
/// line that means "ran but unknown". Callers emit on the first observation and
/// re-emit only on change (the fold reconstructs the trajectory from the
/// ordered events); a caller that would re-emit an unchanged id should skip it.
///
/// `worker_id` is likewise **mandatory** (round-3 / F-02): every new
/// observation is scoped to the worker that produced it, so an emitter that
/// cannot resolve its worker must not emit at all — an unscoped line would be
/// ambiguous forever and the fold treats such legacy lines fail-closed. The
/// `Option` on the wire exists only for deserializing pre-F-02 lines.
///
/// The hot path must not fail because telemetry is unhappy: write errors are
/// swallowed (same trace-not-lock discipline as the other Worker-Spawn helpers).
pub fn emit_model_observed(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    model: &str,
    observed_source: ModelObservationSource,
) {
    let event = EventV2::ModelObserved {
        mol_id: mol_id.clone(),
        worker_id: Some(worker_id.clone()),
        adapter_name: adapter_name.to_owned(),
        model: model.to_owned(),
        observed_source,
        observed_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Emit the **newly-observed tail** of a realized-model trajectory for one
/// dispatch — the first-observation + on-change cadence (delib-20260718-c70e /
/// D4), scoped to `(mol_id, worker_id, adapter_name)` so a re-tackle's
/// observations never dedup against a prior attempt's (F-02).
///
/// Reads back the [`EventV2::ModelObserved`] already on the wire for exactly
/// this `(mol, worker, adapter)` scope, computes the suffix of `observed` not
/// yet recorded, and emits one event per new id. Idempotent: replaying the same
/// trajectory emits nothing. Every id in `observed` is a non-empty
/// [`ModelId`], so a blank realization can never reach the log.
///
/// Best-effort: an unreadable log is treated as "nothing recorded yet" (so a
/// first observation is still emitted), matching the trace-not-lock discipline.
///
/// **Atomic under concurrency** (round-4 / COND-1): the read-back + emit pair
/// runs under an exclusive advisory `flock(2)` on a sidecar lock file next to
/// `events.jsonl`, so two concurrent emitters (e.g. two `cs wait` processes
/// polling the same molecule) serialize — the second sees the first's lines
/// during its read-back and emits nothing. Without the lock the read-then-write
/// is only sequentially idempotent and D4's "re-emit only on change" can be
/// violated by duplicate identical lines on the journal. Lock failure degrades
/// to the unlocked (sequentially-idempotent) behavior rather than losing the
/// observation.
pub fn emit_new_model_observations(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    observed: &[cosmon_core::model_realization::ModelId],
    observed_source: ModelObservationSource,
) {
    if observed.is_empty() {
        return;
    }
    let _guard = ObservationEmitLock::acquire(state_dir);
    let recorded = recorded_model_observations(state_dir, mol_id, worker_id, adapter_name);
    for model in newly_observed(&recorded, observed) {
        emit_model_observed(
            state_dir,
            mol_id,
            worker_id,
            adapter_name,
            model.as_str(),
            observed_source,
        );
    }
}

/// RAII guard making the dedup read-back + emission in
/// [`emit_new_model_observations`] atomic across processes (round-4 / COND-1).
///
/// Holds `flock(LOCK_EX)` on `model_observed.lock` next to `events.jsonl` for
/// the guard's lifetime. This is a *different* file from the append lock the
/// event log itself takes per line, and it is always acquired first, so the
/// lock order is total and deadlock-free. Best-effort: `acquire` returning
/// `None` (unwritable dir, flock failure) means callers proceed unlocked —
/// telemetry must never block the hot path.
struct ObservationEmitLock {
    file: std::fs::File,
}

impl ObservationEmitLock {
    fn acquire(state_dir: &Path) -> Option<Self> {
        let log_path = resolve_events_log_path(state_dir);
        let dir = log_path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("model_observed.lock"))
            .ok()?;
        fs2::FileExt::lock_exclusive(&file).ok()?;
        Some(Self { file })
    }
}

impl Drop for ObservationEmitLock {
    fn drop(&mut self) {
        // Best-effort unlock — the kernel releases on FD close regardless.
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// The realized models already on the wire for exactly `(mol_id, worker_id,
/// adapter_name)`, in append order — folded from the matching
/// [`EventV2::ModelObserved`] events. **Fail-closed** (round-3 / F-02): a
/// legacy observation carrying no `worker_id` is ambiguous and matches **no**
/// requested worker — it must never suppress (dedup away) a properly-scoped
/// new observation, nor be counted as this attempt's prefix. Any read error
/// yields an empty list (best-effort), so a first observation is emitted.
#[must_use]
fn recorded_model_observations(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
) -> Vec<cosmon_core::model_realization::ModelId> {
    let log_path = resolve_events_log_path(state_dir);
    let Ok(envelopes) = crate::event_log::read_all(&log_path) else {
        return Vec::new();
    };
    envelopes
        .into_iter()
        .filter_map(|env| match env.event {
            EventV2::ModelObserved {
                mol_id: ref m,
                worker_id: Some(ref w),
                adapter_name: ref a,
                ref model,
                ..
            } if m == mol_id && a == adapter_name && w == worker_id => {
                cosmon_core::model_realization::ModelId::new(model)
            }
            _ => None,
        })
        .collect()
}

/// The suffix of `observed` not yet present in `recorded` — the models to emit.
///
/// The common case is monotonic growth: `recorded` is a prefix of `observed`
/// (the same trajectory, fewer turns seen last time), so the new tail is
/// `observed[recorded.len()..]`. When the sequences diverge (they should not,
/// given the collapse-consecutive parse), nothing is emitted — silence is safer
/// than a fabricated re-observation.
fn newly_observed<'a>(
    recorded: &[cosmon_core::model_realization::ModelId],
    observed: &'a [cosmon_core::model_realization::ModelId],
) -> &'a [cosmon_core::model_realization::ModelId] {
    if recorded.is_empty() {
        return observed;
    }
    if observed.len() > recorded.len() && observed[..recorded.len()] == *recorded {
        return &observed[recorded.len()..];
    }
    &[]
}

/// Emit an [`EventV2::ModelCeilingHit`] (delib-20260704-b476 / C4).
///
/// Called by `cs tackle` when the fail-closed per-galaxy model-dispatch
/// ceiling refuses a *strong* pin — the (K+1)th strong dispatch inside the
/// rolling window. `action` records what cosmon did
/// ([`CeilingAction::Downgraded`](cosmon_core::event_v2::CeilingAction::Downgraded)
/// — dropped to the safe floor and spawned economical; or
/// [`CeilingAction::Aborted`](cosmon_core::event_v2::CeilingAction::Aborted)
/// — refused the spawn). This is the loud, typed receipt carnot's safety
/// property demands: a strong dispatch over budget can never cross the ceiling
/// silently.
///
/// The hot path must not fail because telemetry is unhappy: write errors are
/// silently swallowed (same trace-not-lock discipline as the other
/// Worker-Spawn helpers).
#[allow(clippy::too_many_arguments)]
pub fn emit_model_ceiling_hit(
    state_dir: &Path,
    mol_id: &MoleculeId,
    adapter_name: &str,
    model: &str,
    strong_count: u32,
    cap: u32,
    window_hours: u32,
    action: cosmon_core::event_v2::CeilingAction,
) {
    let event = EventV2::ModelCeilingHit {
        mol_id: mol_id.clone(),
        adapter_name: adapter_name.to_owned(),
        model: model.to_owned(),
        strong_count,
        cap,
        window_hours,
        action,
        hit_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::SF7BinaryVersionMismatch`].
///
/// Called by a subprocess Adapter (codex today; future subprocess CLI
/// siblings later) when the constructor's three-pillar version-pin
/// check refuses to build the Adapter — the on-PATH binary's
/// `<binary> --version` output did not match the pin declared in
/// `.cosmon/adapters/<adapter>.toml`.
///
/// Forensic-only: the event records the mismatch so a later audit
/// query of the form
/// `jq 'select(.type == "sf7_binary_version_mismatch")'` can
/// attribute drift between expected and actual binary version without
/// re-running `<binary> --version` by hand. The Adapter constructor
/// returns the typed error on the same call site.
///
/// The hot path must not fail because telemetry is unhappy: write
/// errors are silently swallowed (same `trace-not-lock` discipline as
/// the other Worker-Spawn helpers).
pub fn emit_sf7_binary_version_mismatch(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    binary_name: &str,
    expected_version_range: &str,
    actual_version: &str,
) {
    let event = EventV2::SF7BinaryVersionMismatch {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        binary_name: binary_name.to_owned(),
        expected_version_range: expected_version_range.to_owned(),
        actual_version: actual_version.to_owned(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::WorkerSpawnFailed`] (ADR-097 / WS-1') —
/// terminal partner for `WorkerSpawnAttempted` when the backend
/// `spawn_worker` call returned an error.
///
/// Called by `spawn_*_session` (claude, aider, future siblings) from
/// the error branch *before* propagating the spawn error to the
/// caller. The pair `WorkerSpawnAttempted` → `WorkerSpawnFailed`
/// satisfies the TLA+ invariant `I1 — ws1_implies_ws5` for the
/// "never alive" path: a
/// WS-1 with no WS-5 used to look indistinguishable from a live but
/// unprobed worker; with this variant the trail is unambiguous.
///
/// The hot path must not fail because telemetry is unhappy: write
/// errors degrade to the sidecar log and the in-memory counter (see
/// `write_event`).
pub fn emit_worker_spawn_failed(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    reason: &str,
) {
    let event = EventV2::WorkerSpawnFailed {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        reason: reason.to_owned(),
        failed_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Emit an [`EventV2::WorkerSpawnRolledBack`] (ADR-097 / WS-1'') —
/// terminal partner for `WorkerSpawnAttempted` when `cs tackle`'s
/// post-lock RMW race detector rolled the partial spawn back.
///
/// Called by `cs tackle` from `crates/cosmon-cli/src/cmd/tackle.rs`
/// in the rollback path that follows the post-lock read-back, *before*
/// the `WorkerId` is removed from the fleet, so the telemetry context
/// (`mol_id`, `worker_id`, `adapter_name`) is still available. The
/// variant makes the TLA+ invariant
/// `I3 — no_rollback_without_terminal_event` hold: every Dead worker
/// has a terminal event on the wire.
///
/// The hot path must not fail because telemetry is unhappy: write
/// errors degrade to the sidecar log and the in-memory counter (see
/// `write_event`).
pub fn emit_worker_spawn_rolled_back(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    adapter_name: &str,
    reason: &str,
) {
    let event = EventV2::WorkerSpawnRolledBack {
        mol_id: mol_id.clone(),
        worker_id: worker_id.clone(),
        adapter_name: adapter_name.to_owned(),
        reason: reason.to_owned(),
        rolled_back_at: Utc::now(),
    };
    write_event(state_dir, event);
}

/// Shared write path — resolves `events.jsonl` under `state_dir`,
/// creates parent directories if needed, and appends one envelope.
///
/// On failure of the canonical append (ENOSPC, ENOENT, permission
/// denied, IO error, serialise error), the helper degrades to a
/// sidecar log `events.error.jsonl` next to the canonical file —
/// best-effort, never failing — bumps the process-wide error counter
/// ([`emit_error_count`]), and writes exactly one stderr line on the
/// first failure. The hot path stays non-blocking but loud.
fn write_event(state_dir: &Path, event: EventV2) {
    let path = resolve_events_log_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Try the canonical write first. Clone the event up-front so the
    // sidecar fallback can serialise it independently — emit_one moves
    // its argument.
    let event_for_sidecar = event.clone();
    let canonical_path = path.clone();
    if let Err(err) = emit_one(path, event, None) {
        EMIT_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
        // Loud-but-once on the first failure of the process so an
        // operator watching the harness sees the symptom without the
        // log flooding.
        if !FIRST_EMIT_ERROR_LOGGED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "cosmon-state::events::worker_spawn: write to {} failed ({err}); \
                 degrading to sidecar events.error.jsonl",
                canonical_path.display(),
            );
        }
        // Best-effort sidecar append. The sidecar lives next to
        // events.jsonl so a `cat <state_dir>/events.error.jsonl`
        // surfaces the lost envelopes during retrospective audit. If
        // the sidecar write also fails, swallow — the counter is
        // still the audit-trail of last resort.
        let sidecar = state_dir.join("events.error.jsonl");
        if let Ok(line) = serde_json::to_string(&event_for_sidecar) {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&sidecar)
            {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::Envelope;
    use std::fs;
    use tempfile::tempdir;

    fn mol() -> MoleculeId {
        MoleculeId::new("task-20260517-0b46").unwrap()
    }

    fn wkr() -> WorkerId {
        WorkerId::new("polecat-1234").unwrap()
    }

    fn read_envelopes(state_dir: &Path) -> Vec<Envelope> {
        let path = resolve_events_log_path(state_dir);
        let raw = fs::read_to_string(&path).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| Envelope::from_line(l).expect("envelope must parse"))
            .collect()
    }

    /// WS-1: a spawn-attempted emission is reachable from
    /// `events.jsonl` via the canonical `select(.event == ...)`
    /// audit query, with no other variant interleaved.
    #[test]
    fn emit_worker_spawn_attempted_lands_in_events_jsonl() {
        let dir = tempdir().unwrap();
        emit_worker_spawn_attempted(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            "/tmp/wt",
            "abcdef",
            42,
            None,
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::WorkerSpawnAttempted {
            mol_id,
            worker_id,
            adapter_name,
            invocation_uuid,
            pid,
            pre_existing_worker,
            ..
        } = &envelopes[0].event
        else {
            panic!(
                "expected WorkerSpawnAttempted, got {:?}",
                envelopes[0].event
            );
        };
        assert_eq!(mol_id, &mol());
        assert_eq!(worker_id, &wkr());
        assert_eq!(adapter_name, "claude");
        assert_eq!(invocation_uuid, "abcdef");
        assert_eq!(*pid, 42);
        assert!(pre_existing_worker.is_none());
    }

    /// WS-2: a liveness probe with `Alive { evidence }` round-trips
    /// through serde and lands on disk.
    #[test]
    fn emit_adapter_liveness_probed_records_alive_verdict() {
        let dir = tempdir().unwrap();
        emit_adapter_liveness_probed(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            AdapterProbeKind::PaneSignature,
            AdapterProbeResult::Alive {
                evidence: "pane fg=claude".to_owned(),
            },
            0,
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        assert!(matches!(
            envelopes[0].event,
            EventV2::AdapterLivenessProbed {
                probe_result: AdapterProbeResult::Alive { .. },
                ..
            }
        ));
    }

    /// WS-3: a pane-signature check serializes the `channel`
    /// discriminator so the audit can attribute mismatches to
    /// propulsion vs whisper.
    #[test]
    fn emit_adapter_pane_signature_checked_records_channel() {
        let dir = tempdir().unwrap();
        emit_adapter_pane_signature_checked(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            &["claude".to_owned()],
            "claude",
            true,
            PerturbationChannel::Propulsion,
        );
        let envelopes = read_envelopes(dir.path());
        let EventV2::AdapterPaneSignatureChecked {
            matched, channel, ..
        } = &envelopes[0].event
        else {
            panic!("expected AdapterPaneSignatureChecked");
        };
        assert!(*matched);
        assert_eq!(*channel, PerturbationChannel::Propulsion);
    }

    /// WS-4: a briefing-consumed emission records the observed
    /// and recorded seals separately so a post-hoc audit can detect
    /// shadow-contract drift.
    #[test]
    fn emit_adapter_briefing_consumed_records_both_seals() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        emit_adapter_briefing_consumed(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            "briefing.md",
            "aaaa",
            "bbbb",
            1234,
            now,
        );
        let envelopes = read_envelopes(dir.path());
        let EventV2::AdapterBriefingConsumed {
            briefing_seal_observed,
            briefing_seal_recorded,
            bytes_read,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected AdapterBriefingConsumed");
        };
        assert_eq!(briefing_seal_observed, "aaaa");
        assert_eq!(briefing_seal_recorded, "bbbb");
        assert_eq!(*bytes_read, 1234);
    }

    /// WS-5: a handle-reconciled emission records the `gap_ms` and
    /// state discriminator so the audit can classify orphan vs clean
    /// release.
    #[test]
    fn emit_adapter_handle_reconciled_records_state_and_gap() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        emit_adapter_handle_reconciled(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            AdapterHandleState::ReleasedClean,
            Some(now),
            now,
            0,
        );
        let envelopes = read_envelopes(dir.path());
        let EventV2::AdapterHandleReconciled {
            handle_state,
            gap_ms,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected AdapterHandleReconciled");
        };
        assert_eq!(*handle_state, AdapterHandleState::ReleasedClean);
        assert_eq!(*gap_ms, 0);
    }

    /// The four wired-in-C2 helpers together produce the lineage
    /// (Attempted → Probed → `BriefingConsumed` → Reconciled) that the
    /// karpathy cat-test (§14 badge) checks for on every tackle/done
    /// cycle.
    #[test]
    fn cat_test_lineage_attempted_probed_consumed_reconciled() {
        let dir = tempdir().unwrap();
        let now = Utc::now();
        emit_worker_spawn_attempted(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            "/tmp/wt",
            "uuid-1",
            1,
            None,
        );
        emit_adapter_briefing_consumed(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            "briefing.md",
            "h",
            "h",
            0,
            now,
        );
        emit_adapter_liveness_probed(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            AdapterProbeKind::PaneSignature,
            AdapterProbeResult::Alive {
                evidence: "ok".to_owned(),
            },
            0,
        );
        emit_adapter_handle_reconciled(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            AdapterHandleState::ReleasedClean,
            Some(now),
            now,
            0,
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 4);
        let tags: Vec<&str> = envelopes
            .iter()
            .map(|e| match &e.event {
                EventV2::WorkerSpawnAttempted { .. } => "spawn",
                EventV2::AdapterBriefingConsumed { .. } => "briefing",
                EventV2::AdapterLivenessProbed { .. } => "probe",
                EventV2::AdapterHandleReconciled { .. } => "reconcile",
                _ => "other",
            })
            .collect();
        assert_eq!(tags, vec!["spawn", "briefing", "probe", "reconcile"]);
    }

    /// C6: an `AdapterSelected` emission round-trips through serde
    /// with the `Cli { flag }` source variant and a `role_hint`
    /// (the academy-shim's `--role researcher` propagation path).
    #[test]
    fn emit_adapter_selected_records_cli_source_and_role_hint() {
        let dir = tempdir().unwrap();
        emit_adapter_selected(
            dir.path(),
            &mol(),
            "aider",
            AdapterSelectionSource::Cli {
                flag: "aider".to_owned(),
            },
            Some("researcher"),
            LoopOwnership::External,
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::AdapterSelected {
            adapter_name,
            selection_source,
            role_hint,
            loop_ownership,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected AdapterSelected, got {:?}", envelopes[0].event);
        };
        assert_eq!(adapter_name, "aider");
        assert_eq!(role_hint.as_deref(), Some("researcher"));
        assert!(matches!(
            selection_source,
            AdapterSelectionSource::Cli { flag } if flag == "aider"
        ));
        assert_eq!(loop_ownership.as_str(), "external");
    }

    /// C6: an `AdapterSelected` emission with no flag and no config
    /// falls into the `Default { fallback_reason }` source variant.
    /// The omitted `role_hint` serialises as absent (`skip_serializing_if`).
    #[test]
    fn emit_adapter_selected_records_default_source_without_role_hint() {
        let dir = tempdir().unwrap();
        emit_adapter_selected(
            dir.path(),
            &mol(),
            "claude",
            AdapterSelectionSource::Default {
                fallback_reason: "no [adapters] config; using built-in 'claude'".to_owned(),
            },
            None,
            LoopOwnership::External,
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::AdapterSelected {
            adapter_name,
            selection_source,
            role_hint,
            loop_ownership,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected AdapterSelected");
        };
        assert_eq!(adapter_name, "claude");
        assert!(role_hint.is_none());
        assert!(matches!(
            selection_source,
            AdapterSelectionSource::Default { .. }
        ));
        assert_eq!(loop_ownership.as_str(), "external");
    }

    /// ADR-103: an in-process Direct-API adapter (`openai` /
    /// `anthropic`) carries `loop_ownership = "cosmon"` on the
    /// `AdapterSelected` event. Pins the cat-test invariant that an
    /// `openai` selection that wrote `"external"` to the wire is a
    /// silent routing bug.
    #[test]
    fn emit_adapter_selected_in_process_carries_cosmon_loop_ownership() {
        let dir = tempdir().unwrap();
        emit_adapter_selected(
            dir.path(),
            &mol(),
            "openai",
            AdapterSelectionSource::Cli {
                flag: "openai".to_owned(),
            },
            None,
            LoopOwnership::Cosmon,
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::AdapterSelected {
            adapter_name,
            loop_ownership,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected AdapterSelected");
        };
        assert_eq!(adapter_name, "openai");
        assert_eq!(loop_ownership.as_str(), "cosmon");
    }

    /// C2 (delib-20260704-b476): a `ModelSelected` emission round-trips
    /// through serde with the `Flag { flag }` source variant carrying the
    /// pinned model id — the operator's `--model` in-the-moment choice.
    #[test]
    fn emit_model_selected_records_flag_source_and_model() {
        let dir = tempdir().unwrap();
        emit_model_selected(
            dir.path(),
            &mol(),
            "claude",
            Some("claude-opus-4-8"),
            ModelSelectionSource::Flag {
                flag: "claude-opus-4-8".to_owned(),
            },
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::ModelSelected {
            adapter_name,
            model,
            selection_source,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected ModelSelected, got {:?}", envelopes[0].event);
        };
        assert_eq!(adapter_name, "claude");
        assert_eq!(model.as_deref(), Some("claude-opus-4-8"));
        assert!(matches!(
            selection_source,
            ModelSelectionSource::Flag { flag } if flag == "claude-opus-4-8"
        ));
    }

    /// realized-model (delib-20260718-c70e): a `ModelObserved` emission
    /// round-trips through serde carrying the concrete realized id and its
    /// per-adapter provenance. The `model` is a bare string — the event exists
    /// only because a real id was observed.
    #[test]
    fn emit_model_observed_records_realized_model_and_source() {
        let dir = tempdir().unwrap();
        emit_model_observed(
            dir.path(),
            &mol(),
            &WorkerId::new("worker-1").unwrap(),
            "claude",
            "claude-sonnet-5",
            ModelObservationSource::ClaudeStreamJson,
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::ModelObserved {
            adapter_name,
            model,
            observed_source,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected ModelObserved, got {:?}", envelopes[0].event);
        };
        assert_eq!(adapter_name, "claude");
        assert_eq!(model, "claude-sonnet-5");
        assert_eq!(*observed_source, ModelObservationSource::ClaudeStreamJson);
    }

    /// C2: the floor path — nothing pinned a model, so `model` is `None`
    /// (serialised absent via `skip_serializing_if`) and the source is the
    /// `Default { fallback_reason }` floor. The safe default: silence never
    /// names a strong model.
    #[test]
    fn emit_model_selected_floor_omits_model_and_records_default_source() {
        let dir = tempdir().unwrap();
        emit_model_selected(
            dir.path(),
            &mol(),
            "claude",
            None,
            ModelSelectionSource::Default {
                fallback_reason: "no pin; adapter default applies".to_owned(),
            },
        );
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::ModelSelected {
            adapter_name,
            model,
            selection_source,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected ModelSelected");
        };
        assert_eq!(adapter_name, "claude");
        assert!(model.is_none());
        assert!(matches!(
            selection_source,
            ModelSelectionSource::Default { .. }
        ));
        // The `None` model must not appear on the wire (skip_serializing_if).
        let line = std::fs::read_to_string(resolve_events_log_path(dir.path())).unwrap();
        assert!(
            !line.contains("\"model\""),
            "None model should be omitted from the wire, got: {line}"
        );
        assert!(line.contains("\"type\":\"model_selected\""));
    }

    /// Negative test (galileo §2.1): if the adapter never calls
    /// `emit_worker_spawn_attempted`, the audit query
    /// `select(.event == "WorkerSpawnAttempted")` returns empty —
    /// exactly the silent-failure mode WS-1 names.
    #[test]
    fn missing_spawn_attempt_yields_empty_audit_match() {
        let dir = tempdir().unwrap();
        // Only emit the probe; deliberately skip the spawn attempt.
        emit_adapter_liveness_probed(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            AdapterProbeKind::PaneSignature,
            AdapterProbeResult::Alive {
                evidence: "ok".to_owned(),
            },
            0,
        );
        let envelopes = read_envelopes(dir.path());
        let any_spawn = envelopes
            .iter()
            .any(|e| matches!(e.event, EventV2::WorkerSpawnAttempted { .. }));
        assert!(
            !any_spawn,
            "missing spawn-attempt must surface as empty audit match (WS-1 detection)"
        );
    }

    /// WS-1' — `WorkerSpawnFailed` round-trips
    /// through serde and lands on disk with the reason preserved.
    #[test]
    fn emit_worker_spawn_failed_records_reason() {
        let dir = tempdir().unwrap();
        emit_worker_spawn_failed(dir.path(), &mol(), &wkr(), "claude", "tmux not on PATH");
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::WorkerSpawnFailed {
            mol_id,
            worker_id,
            adapter_name,
            reason,
            ..
        } = &envelopes[0].event
        else {
            panic!("expected WorkerSpawnFailed, got {:?}", envelopes[0].event);
        };
        assert_eq!(mol_id, &mol());
        assert_eq!(worker_id, &wkr());
        assert_eq!(adapter_name, "claude");
        assert_eq!(reason, "tmux not on PATH");
    }

    /// WS-1'' — `WorkerSpawnRolledBack`
    /// round-trips through serde and lands on disk with the
    /// observed-status reason preserved.
    #[test]
    fn emit_worker_spawn_rolled_back_records_observed_status() {
        let dir = tempdir().unwrap();
        emit_worker_spawn_rolled_back(dir.path(), &mol(), &wkr(), "claude", "pending");
        let envelopes = read_envelopes(dir.path());
        assert_eq!(envelopes.len(), 1);
        let EventV2::WorkerSpawnRolledBack {
            mol_id,
            worker_id,
            adapter_name,
            reason,
            ..
        } = &envelopes[0].event
        else {
            panic!(
                "expected WorkerSpawnRolledBack, got {:?}",
                envelopes[0].event
            );
        };
        assert_eq!(mol_id, &mol());
        assert_eq!(worker_id, &wkr());
        assert_eq!(adapter_name, "claude");
        assert_eq!(reason, "pending");
    }

    /// **Sidecar fallback on canonical-log write failure** — when the
    /// canonical `events.jsonl` cannot be written (we simulate the failure by
    /// making the path a directory rather than a file, so `OpenOptions`
    /// returns an `IsADirectory` error on append), the helper degrades
    /// to a sidecar `events.error.jsonl` next to the canonical path
    /// and the in-memory counter ticks.
    ///
    /// Note: we cannot use `/dev/full` directly because we need to
    /// trigger the failure path of `emit_one` (which goes through
    /// `OpenOptions::open`), not just a write error. A directory at
    /// `events.jsonl` is sufficient — `OpenOptions::append(true)` on
    /// a directory returns `Err(EISDIR)` deterministically across
    /// Linux + macOS, exercising the same fallback the ENOSPC path
    /// would.
    #[test]
    fn write_event_degrades_to_sidecar_and_increments_counter_on_io_error() {
        let dir = tempdir().unwrap();
        // Make the canonical events.jsonl path unwritable by placing
        // a directory there.
        let canonical = dir.path().join("events.jsonl");
        std::fs::create_dir_all(&canonical).expect("create events.jsonl as a dir");

        // Snapshot the counter, run the helper, snapshot again.
        reset_emit_error_counters_for_tests();
        let before = emit_error_count();
        emit_worker_spawn_attempted(
            dir.path(),
            &mol(),
            &wkr(),
            "claude",
            "/wt",
            "uuid-enospc",
            13,
            None,
        );
        let after = emit_error_count();

        assert!(
            after > before,
            "ENOSPC-equivalent must bump the emit-error counter; before={before} after={after}"
        );

        // Sidecar must exist and contain a serialised envelope.
        let sidecar = dir.path().join("events.error.jsonl");
        let body = fs::read_to_string(&sidecar)
            .expect("sidecar events.error.jsonl must exist when canonical write fails");
        assert!(
            body.contains("worker_spawn_attempted"),
            "sidecar must carry the lost WS-1 envelope; observed body={body:?}"
        );
    }

    /// Round-4 / COND-1: two emitters racing on the same observation must
    /// yield exactly ONE `ModelObserved` line. The empirical failure this
    /// falsifies: two concurrent `cs wait` pollers each did the (unlocked)
    /// read-back before either wrote, and the journal ended up with two
    /// identical lines — sequentially idempotent, but violating D4's
    /// "re-emit only on change" cadence under concurrency. The
    /// `ObservationEmitLock` serializes read-back + emit, so the loser of the
    /// race sees the winner's line and stays silent.
    #[test]
    fn concurrent_emitters_yield_a_single_observation() {
        use std::sync::{Arc, Barrier};

        let dir = tempdir().unwrap();
        let mol_id = mol();
        let worker_id = wkr();
        let observed =
            vec![cosmon_core::model_realization::ModelId::new("claude-opus-4-8").unwrap()];

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let state_dir = dir.path().to_path_buf();
                let mol_id = mol_id.clone();
                let worker_id = worker_id.clone();
                let observed = observed.clone();
                std::thread::spawn(move || {
                    // Maximize overlap: both emitters release together.
                    barrier.wait();
                    emit_new_model_observations(
                        &state_dir,
                        &mol_id,
                        &worker_id,
                        "claude",
                        &observed,
                        ModelObservationSource::ClaudeStreamJson,
                    );
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let n_observed = read_envelopes(dir.path())
            .into_iter()
            .filter(|env| matches!(env.event, EventV2::ModelObserved { .. }))
            .count();
        assert_eq!(
            n_observed, 1,
            "concurrent emitters must serialize to exactly one ModelObserved line"
        );
    }
}
