// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded, polling wait for a molecule to reach a target status set.
//!
//! This is the shared kernel behind both `cs wait` and the `cosmon_wait` MCP
//! tool — it closes the canonical `cs tackle` / `cs wait` / `cs done` trinity
//! so scripts and agents can compose the full workflow in a single line
//! instead of writing ad-hoc polling loops.
//!
//! # Regime alignment (ADR-016)
//!
//! `wait_for_status` is **stateless** (read-only — it never mutates the store),
//! **idempotent** (each call is independent; calling on an already-terminal
//! molecule returns immediately with no wasted polls), and **bounded** (it
//! exits as soon as either the condition is met or the timeout elapses — it
//! is never a daemon). It therefore works in every regime (Inert, Propelled,
//! future Autonomous) without assuming anything about who is driving the
//! clock.
//!
//! # Distinct perimeter
//!
//! This helper implements `kubectl wait` semantics, not `kubectl watch`. It
//! is deliberately different from the snapshot view (`cs observe`) and the
//! live fleet stream (`cs watch`): three distinct verbs for three distinct
//! patterns — **snapshot**, **live view**, **bounded wait**.
//!
//! # Metrics (feedback loop)
//!
//! Every [`WaitOutcome`] carries a [`WaitMetrics`] bundle so operators (human
//! and AI via MCP) build intuition about what their requests cost and how
//! the work actually went. Two fields are always populated (`poll_count`,
//! `transitions` — measured directly by the wait loop). The remaining
//! fields — `energy`, `entropy`, `temperature` — are optional and degrade
//! gracefully to `None` when the backing data source is absent. The policy
//! is **omit-if-none**: callers should skip missing keys in their JSON
//! output rather than emit `null` placeholders.
//!
//! # Metric coupling (THESIS Part XVIII)
//!
//! `cs wait` is the first — but deliberately not the last — verb to expose
//! this bundle. The governing principle: a read-only, stateless surface that
//! does not radiate its observations is hoarding. The same bundle must flow through every read-only sibling that
//! shares wait's discipline, starting with [`cs observe`]. To keep the
//! cognitive SNR ceiling (Shannon, ~7 decorrelated fields), the bundle is
//! **frozen at five scalars**: `poll_count`, `transitions`, `energy`,
//! `entropy`, `temperature`. Do not widen it without a successor ADR.
//!
//! [`cs observe`]: https://example.invalid

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

use cosmon_core::energy::EnergyRecord;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use serde::Serialize;

use crate::{MoleculeData, StateStore};

/// Aggregated token / cost consumption observed for a molecule.
///
/// Populated from the energy JSONL log (see [`FileEnergyTracker`]). The
/// field names match the canonical wire format expected by `cs wait --json`
/// and `cosmon_wait`.
///
/// [`FileEnergyTracker`]: crate::file_energy_tracker::FileEnergyTracker
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EnergyMetrics {
    /// Sum of all input tokens recorded for the molecule.
    pub input_tokens: u64,
    /// Sum of all output tokens recorded for the molecule.
    pub output_tokens: u64,
    /// Monetary cost in USD — floating because fractional cents are common.
    pub cost_usd: f64,
}

/// Shannon-style information content for a molecule's traffic, in bits.
///
/// This is the information-theoretic counterpart of [`EnergyMetrics`]
/// (THESIS Part XI/XII). Currently always `None` — reserved for a future
/// claudion-side computation so we don't tighten the wire format later.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EntropyMetrics {
    /// Estimated input entropy in bits.
    pub input_bits: f64,
    /// Estimated output entropy in bits.
    pub output_bits: f64,
}

/// Quantitative metrics returned alongside a [`WaitOutcome`].
///
/// `poll_count` and `transitions` are always available — they are observed
/// directly by the wait loop. The remaining fields are optional and follow
/// the **omit-if-none** discipline: when the backing data source is missing
/// (no energy log, no entropy probe, no temperature sample) the field stays
/// `None` and callers skip it in their serialized output.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WaitMetrics {
    /// How many times the store was polled before the terminal status
    /// was reached. Includes the successful poll, so a molecule that was
    /// already terminal on entry reports `1`.
    pub poll_count: u32,
    /// Count of observed status changes between consecutive polls. A
    /// molecule that flipped `pending → running → completed` during the
    /// wait reports `2`. Non-decreasing state machines should stay near
    /// the number of legitimate transitions — unhealthy molecules flapping
    /// between states will show inflated counts.
    pub transitions: u32,
    /// Energy (tokens + cost) aggregated for this molecule. Present when
    /// an energy JSONL log exists at `{state_dir}/log/energy.jsonl` and
    /// contains at least one record matching the molecule. `None`
    /// otherwise — never emitted as a placeholder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy: Option<EnergyMetrics>,
    /// Information-theoretic bits for the molecule's traffic. Currently
    /// always `None` — wire-format slot reserved for a future claudion
    /// computation (THESIS Part XI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entropy: Option<EntropyMetrics>,
    /// Last observed sampling temperature, if any sampler exposed one.
    /// Currently always `None` — no data source tracks per-molecule
    /// temperature samples yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// Outcome of a successful [`wait_for_status`] call — the molecule reached
/// one of the target statuses before the timeout expired.
#[derive(Debug, Clone)]
pub struct WaitOutcome {
    /// Full molecule snapshot at the moment the condition was met. Returned
    /// so callers don't need a follow-up `observe` to see the final state.
    pub molecule: MoleculeData,
    /// The status that satisfied the condition — always an element of the
    /// `target` slice passed to [`wait_for_status`].
    pub reached: MoleculeStatus,
    /// How long the wait took from the first store read. Zero if the
    /// molecule was already in the target set on the first poll.
    pub elapsed: Duration,
    /// Quantitative observations collected during the wait. Always
    /// populated; individual optional fields may be `None`.
    pub metrics: WaitMetrics,
}

/// Failure modes of [`wait_for_status`].
#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    /// The molecule does not exist in the store. Distinct from a timeout —
    /// polling cannot recover from a missing molecule, so we fail fast.
    #[error("molecule not found: {0}")]
    MoleculeNotFound(MoleculeId),

    /// The backing store returned an I/O-level error. Propagated verbatim
    /// so callers can surface it to the operator.
    #[error("state store error: {0}")]
    Store(String),

    /// The timeout elapsed before the molecule reached a target status.
    /// `last_status` is the most recent observation, useful for log lines
    /// and non-zero exit codes.
    #[error("timed out after {elapsed:?} — molecule is still `{last_status}`")]
    Timeout {
        /// How long the caller waited before giving up.
        elapsed: Duration,
        /// The molecule's status at the final poll.
        last_status: MoleculeStatus,
    },
}

/// Block until `id` reaches one of the statuses in `target`, or fail after
/// `timeout`.
///
/// # Semantics
///
/// - **Immediate return.** The first action is always a store read; if the
///   molecule is already in `target` the function returns without sleeping.
/// - **Polling discipline.** Between reads the thread sleeps for at most
///   `poll_interval`, clamped to the remaining time so we never overshoot
///   the budget.
/// - **No mutation.** Every operation is read-only. Safe to call from any
///   regime (Inert, Propelled, Autonomous) and from multiple processes at
///   once.
/// - **Empty target set** is a programmer error — the loop can never exit,
///   so we short-circuit to [`WaitError::Timeout`] on the first poll.
/// - **Metrics.** The returned [`WaitOutcome`] always has `poll_count` and
///   `transitions` populated by the wait loop itself; `energy`, `entropy`
///   and `temperature` stay `None` unless the caller layers on
///   [`wait_for_status_with_metrics`] (which enriches from the energy log).
///
/// # Errors
///
/// - [`WaitError::MoleculeNotFound`] — the molecule is absent from the
///   store. This is not retried: a missing molecule will not appear by
///   polling.
/// - [`WaitError::Store`] — the backing store surfaced an I/O error on the
///   initial read. We propagate verbatim so the operator can diagnose.
/// - [`WaitError::Timeout`] — `timeout` elapsed with the molecule still
///   outside `target`.
///
/// # Example
///
/// ```no_run
/// use std::time::Duration;
/// use cosmon_core::id::MoleculeId;
/// use cosmon_core::molecule::MoleculeStatus;
/// use cosmon_state::wait::wait_for_status;
/// # fn demo(store: &dyn cosmon_state::StateStore) -> Result<(), Box<dyn std::error::Error>> {
/// let id = MoleculeId::new("task-20260409-abcd")?;
/// let outcome = wait_for_status(
///     store,
///     &id,
///     &[MoleculeStatus::Completed, MoleculeStatus::Collapsed],
///     Duration::from_secs(600),
///     Duration::from_secs(5),
/// )?;
/// println!(
///     "reached {} in {:?} after {} polls ({} transitions)",
///     outcome.reached,
///     outcome.elapsed,
///     outcome.metrics.poll_count,
///     outcome.metrics.transitions,
/// );
/// # Ok(()) }
/// ```
pub fn wait_for_status(
    store: &dyn StateStore,
    id: &MoleculeId,
    target: &[MoleculeStatus],
    timeout: Duration,
    poll_interval: Duration,
) -> Result<WaitOutcome, WaitError> {
    wait_for_status_with_sleep(
        store,
        id,
        target,
        timeout,
        poll_interval,
        std::thread::sleep,
        || {},
    )
}

/// Wait for `id`, then enrich the outcome with metrics pulled from the
/// energy JSONL log at `{state_dir}/log/energy.jsonl`.
///
/// This is the CLI- and MCP-facing variant: it preserves every semantic
/// of [`wait_for_status`] (same errors, same exit conditions, same
/// `poll_count` / `transitions` bookkeeping) and then — only if the
/// wait succeeded — attempts a best-effort read of the energy log to
/// populate `metrics.energy`. A missing log file leaves `energy = None`;
/// corrupt lines are silently skipped so a single bad record cannot
/// poison the whole wait.
///
/// Entropy and temperature stay `None` here; they are reserved for
/// future probes that know how to compute them.
///
/// # Errors
///
/// Identical to [`wait_for_status`] — the enrichment step is infallible
/// from the caller's perspective (any I/O problem in the energy log is
/// absorbed into a `None` metric).
pub fn wait_for_status_with_metrics(
    store: &dyn StateStore,
    state_dir: &Path,
    id: &MoleculeId,
    target: &[MoleculeStatus],
    timeout: Duration,
    poll_interval: Duration,
) -> Result<WaitOutcome, WaitError> {
    wait_for_status_with_metrics_probed(store, state_dir, id, target, timeout, poll_interval, || {})
}

/// [`wait_for_status_with_metrics`] with a **per-poll runtime probe** —
/// the seam the realized-model capture rides (delib-20260718-c70e / D4,
/// round-3 F-01).
///
/// `on_poll` is invoked once per poll iteration, *while the worker is still
/// running*. The canonical pilot cycle is `cs tackle → cs wait → cs done`, so
/// the wait loop is the one cosmon process reliably alive during a
/// subprocess-adapter run (claude/codex in tmux): a probe here can read the
/// worker's live session log and emit `ModelObserved` at the **first**
/// model-bearing turn — durable on `events.jsonl` even if the worker later
/// crashes before `cs complete`. The probe must be best-effort and cheap-ish
/// (it runs every `poll_interval`); it takes no arguments and returns nothing
/// so the wait loop stays I/O-free with respect to what the probe does.
///
/// # Errors
///
/// Identical to [`wait_for_status`].
#[allow(clippy::too_many_arguments)]
pub fn wait_for_status_with_metrics_probed<F: FnMut()>(
    store: &dyn StateStore,
    state_dir: &Path,
    id: &MoleculeId,
    target: &[MoleculeStatus],
    timeout: Duration,
    poll_interval: Duration,
    on_poll: F,
) -> Result<WaitOutcome, WaitError> {
    let mut outcome = wait_for_status_with_sleep(
        store,
        id,
        target,
        timeout,
        poll_interval,
        std::thread::sleep,
        on_poll,
    )?;
    outcome.metrics.energy = collect_molecule_energy(state_dir, id);
    Ok(outcome)
}

/// Build a single-snapshot coupling report for `id`.
///
/// This is the shared kernel behind the metrics bundle now exposed by both
/// `cs wait` and `cs observe` — the two read-only, stateless, regime-agnostic
/// verbs. They must return the same shape so operators (human and AI) learn
/// a single vocabulary: see THESIS Part XVIII.
///
/// Because a snapshot is a zero-poll observation, the bookkeeping fields
/// are hard-coded:
///
/// - `poll_count = 1` — one read of the store, no looping.
/// - `transitions = 0` — a single observation cannot witness a transition.
///
/// The optional fields follow the same **omit-if-none** discipline as
/// [`wait_for_status_with_metrics`]:
///
/// - `energy` — aggregated from `{state_dir}/log/energy.jsonl` if present,
///   otherwise `None`.
/// - `entropy`, `temperature` — always `None` until a probe exists.
///
/// The shape is deliberately frozen at five scalar fields to stay under the
/// Shannon cognitive-SNR ceiling (~7 decorrelated fields). New metrics must
/// land in a successor ADR, not as a silent widening.
#[must_use]
pub fn coupling_report_snapshot(state_dir: &Path, id: &MoleculeId) -> WaitMetrics {
    WaitMetrics {
        poll_count: 1,
        transitions: 0,
        energy: collect_molecule_energy(state_dir, id),
        entropy: None,
        temperature: None,
    }
}

/// Aggregate per-molecule energy from `{state_dir}/log/energy.jsonl`.
///
/// Returns `None` when the log file is absent or when no record matches
/// the given molecule — this is the signal the caller uses to omit the
/// `energy` field from its JSON output (omit-if-none discipline).
///
/// The function is deliberately tolerant of malformed lines: if a record
/// fails to parse it is skipped rather than returned as an error, so a
/// single corrupted write from a crashing worker cannot block a wait.
#[must_use]
pub fn collect_molecule_energy(state_dir: &Path, molecule: &MoleculeId) -> Option<EnergyMetrics> {
    let log_path = state_dir.join("log/energy.jsonl");
    if !log_path.exists() {
        return None;
    }
    let file = fs::File::open(&log_path).ok()?;
    let reader = BufReader::new(file);

    let mut metrics = EnergyMetrics::default();
    let mut matched = false;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<EnergyRecord>(trimmed) else {
            continue;
        };
        if &record.molecule != molecule {
            continue;
        }
        metrics.input_tokens = metrics
            .input_tokens
            .saturating_add(record.input_tokens.get());
        metrics.output_tokens = metrics
            .output_tokens
            .saturating_add(record.output_tokens.get());
        metrics.cost_usd += record.cost.get();
        matched = true;
    }

    if matched {
        Some(metrics)
    } else {
        None
    }
}

/// Assemble a [`WaitOutcome`] from a confirmed in-target molecule read.
///
/// Centralises the `poll_count` / `transitions` bookkeeping so the two
/// success exits of [`wait_for_status_with_sleep`] (already-stable terminal
/// and confirmed-fresh-transition) build byte-identical outcomes.
fn make_outcome(
    mol: MoleculeData,
    start: &Instant,
    poll_count: u32,
    transitions: u32,
) -> WaitOutcome {
    WaitOutcome {
        reached: mol.status,
        elapsed: start.elapsed(),
        metrics: WaitMetrics {
            poll_count,
            transitions,
            ..WaitMetrics::default()
        },
        molecule: mol,
    }
}

/// Internal variant that lets tests inject a fake sleep — avoids real time
/// in unit tests while keeping the public API sleep-free. `on_poll` is the
/// per-iteration runtime probe (see [`wait_for_status_with_metrics_probed`]);
/// it fires after every store read, including the final in-target one.
pub(crate) fn wait_for_status_with_sleep<F, P>(
    store: &dyn StateStore,
    id: &MoleculeId,
    target: &[MoleculeStatus],
    timeout: Duration,
    poll_interval: Duration,
    mut sleep: F,
    mut on_poll: P,
) -> Result<WaitOutcome, WaitError>
where
    F: FnMut(Duration),
    P: FnMut(),
{
    let start = Instant::now();
    let mut poll_count: u32 = 0;
    let mut transitions: u32 = 0;
    let mut previous_status: Option<MoleculeStatus> = None;

    loop {
        let mol = load_molecule(store, id)?;
        poll_count = poll_count.saturating_add(1);
        on_poll();
        // The status observed on the *previous* iteration — captured before
        // we overwrite `previous_status`, so the confirmation gate below can
        // tell a fresh transition into the target set from a status that was
        // already in-target on the prior poll.
        let prior = previous_status;
        if let Some(prev) = prior {
            if prev != mol.status {
                transitions = transitions.saturating_add(1);
            }
        }
        previous_status = Some(mol.status);

        if target.contains(&mol.status) {
            // Confirmation re-read (task-20260606-21d4, DoD c). `cs wait`
            // must return ONLY on a *real* terminal observation, never on a
            // transient one. A molecule that was observed in a non-target
            // status earlier in this wait, and now reads in-target for the
            // first time, is a *fresh transition* — we re-read it once,
            // immediately (no sleep), and only declare success if it is
            // *still* in target. A genuine terminal status is absorbing
            // (`Completed` / `Collapsed` never leave the set), so the
            // confirmation is free for every real molecule; it only filters
            // out a target status that was observed for a single poll and
            // then contradicted (e.g. a racing writer, a non-atomic
            // multi-write transition, or a `--for` that targets a
            // recoverable status like `frozen` that flickered back to
            // `running`). A molecule already in-target on the very first
            // poll (`prior == None`) has been stable since before the wait
            // began and returns with no confirmation, so the already-terminal
            // fast path keeps its zero-extra-read, zero-sleep behaviour.
            let prior_in_target = prior.is_some_and(|p| target.contains(&p));
            if prior.is_none() || prior_in_target {
                return Ok(make_outcome(mol, &start, poll_count, transitions));
            }
            let confirm = load_molecule(store, id)?;
            poll_count = poll_count.saturating_add(1);
            if confirm.status != mol.status {
                transitions = transitions.saturating_add(1);
            }
            previous_status = Some(confirm.status);
            if target.contains(&confirm.status) {
                return Ok(make_outcome(confirm, &start, poll_count, transitions));
            }
            // Transient: the in-target observation did not survive the
            // confirmation re-read. Fall through to the timeout/sleep path
            // and keep polling rather than returning a false terminal.
        }

        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return Err(WaitError::Timeout {
                elapsed,
                last_status: mol.status,
            });
        }

        // Sleep at most the remaining budget so we never overshoot the
        // caller's deadline. Empty target set trips `elapsed >= timeout`
        // on the next iteration once the budget drains.
        let remaining = timeout.saturating_sub(elapsed);
        let step = poll_interval.min(remaining);
        if step.is_zero() {
            return Err(WaitError::Timeout {
                elapsed,
                last_status: mol.status,
            });
        }
        sleep(step);
    }
}

/// Load a molecule, mapping `CosmonError::MoleculeNotFound` onto a distinct
/// `WaitError` variant so callers can surface it as a structured error
/// (non-zero exit, CLI message, MCP `invalid_params`) rather than a timeout.
fn load_molecule(store: &dyn StateStore, id: &MoleculeId) -> Result<MoleculeData, WaitError> {
    use cosmon_core::error::CosmonError;
    store.load_molecule(id).map_err(|e| match e {
        CosmonError::MoleculeNotFound(_) => WaitError::MoleculeNotFound(id.clone()),
        other => WaitError::Store(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use chrono::Utc;
    use cosmon_core::energy::{TokenCost, TokenCount};
    use cosmon_core::error::CosmonError;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId, StepId, WorkerId};
    use tempfile::TempDir;

    use super::*;
    use crate::{Fleet, MoleculeFilter};

    /// In-memory store whose `load_molecule` cycles through a scripted list
    /// of statuses. First call returns the first entry, second call the
    /// second, and so on; once exhausted the last entry is repeated.
    struct ScriptedStore {
        id: MoleculeId,
        script: Mutex<Vec<MoleculeStatus>>,
        index: Mutex<usize>,
    }

    impl ScriptedStore {
        fn new(id: &str, script: Vec<MoleculeStatus>) -> Self {
            assert!(
                !script.is_empty(),
                "script must contain at least one status"
            );
            Self {
                id: MoleculeId::new(id).unwrap(),
                script: Mutex::new(script),
                index: Mutex::new(0),
            }
        }

        fn make_mol(&self, status: MoleculeStatus) -> MoleculeData {
            MoleculeData {
                id: self.id.clone(),
                fleet_id: FleetId::new("default").unwrap(),
                formula_id: FormulaId::new("task-work").unwrap(),
                status,
                variables: HashMap::new(),
                assigned_worker: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                total_steps: 2,
                current_step: 1,
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
                tags: std::collections::BTreeSet::new(),
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
                process: None,
                energy_budget: None,
                stuck_at: None,
                tackled_by: None,
                tackled_at: None,
            }
        }
    }

    impl StateStore for ScriptedStore {
        fn load_fleet(&self) -> Result<Fleet, CosmonError> {
            Ok(Fleet::default())
        }
        fn save_fleet(&self, _: &Fleet) -> Result<(), CosmonError> {
            Ok(())
        }
        fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
            if id != &self.id {
                return Err(CosmonError::MoleculeNotFound(id.clone()));
            }
            let script = self.script.lock().unwrap();
            let mut idx = self.index.lock().unwrap();
            let i = (*idx).min(script.len() - 1);
            *idx += 1;
            Ok(self.make_mol(script[i]))
        }
        fn save_molecule(&self, _id: &MoleculeId, _data: &MoleculeData) -> Result<(), CosmonError> {
            Ok(())
        }
        fn list_molecules(
            &self,
            _filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, CosmonError> {
            Ok(Vec::new())
        }
    }

    /// A store that always reports the molecule as missing.
    struct EmptyStore;

    impl StateStore for EmptyStore {
        fn load_fleet(&self) -> Result<Fleet, CosmonError> {
            Ok(Fleet::default())
        }
        fn save_fleet(&self, _: &Fleet) -> Result<(), CosmonError> {
            Ok(())
        }
        fn load_molecule(&self, id: &MoleculeId) -> Result<MoleculeData, CosmonError> {
            Err(CosmonError::MoleculeNotFound(id.clone()))
        }
        fn save_molecule(&self, _id: &MoleculeId, _data: &MoleculeData) -> Result<(), CosmonError> {
            Ok(())
        }
        fn list_molecules(
            &self,
            _filter: &MoleculeFilter,
        ) -> Result<Vec<MoleculeData>, CosmonError> {
            Ok(Vec::new())
        }
    }

    /// Helper: write an `EnergyRecord` as a line in the tracker's JSONL log.
    fn append_energy_record(state_dir: &Path, record: &EnergyRecord) {
        use std::io::Write as _;
        let log_path = state_dir.join("log/energy.jsonl");
        fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap();
        let json = serde_json::to_string(record).unwrap();
        writeln!(file, "{json}").unwrap();
    }

    fn energy_record(molecule: &MoleculeId, input: u64, output: u64, cost: f64) -> EnergyRecord {
        EnergyRecord {
            timestamp: Utc::now(),
            worker: WorkerId::new("topaz").unwrap(),
            molecule: molecule.clone(),
            step: StepId::new("step-1").unwrap(),
            model: "claude-opus-4-6".to_owned(),
            input_tokens: TokenCount::new(input),
            output_tokens: TokenCount::new(output),
            cost: TokenCost::new(cost),
        }
    }

    #[test]
    fn test_immediate_return_when_already_terminal() {
        let store = ScriptedStore::new("task-20260409-imm1", vec![MoleculeStatus::Completed]);
        let sleep_calls = RefCell::new(0usize);
        let outcome = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed, MoleculeStatus::Collapsed],
            Duration::from_secs(60),
            Duration::from_secs(5),
            |_| *sleep_calls.borrow_mut() += 1,
            || (),
        )
        .expect("should return immediately");
        assert_eq!(outcome.reached, MoleculeStatus::Completed);
        assert_eq!(
            *sleep_calls.borrow(),
            0,
            "already-terminal molecules must not sleep"
        );
        assert_eq!(outcome.metrics.poll_count, 1);
        assert_eq!(outcome.metrics.transitions, 0);
    }

    #[test]
    fn test_returns_after_status_transitions() {
        let store = ScriptedStore::new(
            "task-20260409-trn1",
            vec![
                MoleculeStatus::Running,
                MoleculeStatus::Running,
                MoleculeStatus::Completed,
            ],
        );
        let sleep_calls = RefCell::new(0usize);
        let outcome = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed],
            Duration::from_secs(60),
            Duration::from_millis(1),
            |_| *sleep_calls.borrow_mut() += 1,
            || (),
        )
        .expect("should reach completed");
        assert_eq!(outcome.reached, MoleculeStatus::Completed);
        assert_eq!(
            *sleep_calls.borrow(),
            2,
            "two polls before the terminal one"
        );
        assert_eq!(
            outcome.metrics.poll_count, 4,
            "three polls + one confirmation re-read of the fresh terminal transition"
        );
        assert_eq!(
            outcome.metrics.transitions, 1,
            "Running→Completed is one transition; the confirmation re-read sees the same status"
        );
    }

    /// Round-3 / F-01: the per-poll runtime probe fires on EVERY store poll,
    /// including while the molecule is still Running — this is the seam the
    /// realized-model capture rides so `ModelObserved` lands during the run,
    /// not at teardown.
    #[test]
    fn test_on_poll_probe_fires_on_every_poll() {
        let store = ScriptedStore::new(
            "task-20260718-prb1",
            vec![
                MoleculeStatus::Running,
                MoleculeStatus::Running,
                MoleculeStatus::Completed,
            ],
        );
        let probe_calls = RefCell::new(0usize);
        let outcome = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed],
            Duration::from_secs(60),
            Duration::from_millis(1),
            |_| (),
            || *probe_calls.borrow_mut() += 1,
        )
        .expect("should reach completed");
        assert_eq!(outcome.reached, MoleculeStatus::Completed);
        assert!(
            *probe_calls.borrow() >= 2,
            "the probe must fire on the Running polls, while the worker is live \
             (got {} calls)",
            *probe_calls.borrow()
        );
    }

    #[test]
    fn test_transitions_counts_non_decreasing_changes() {
        // pending → queued → running → running → completed is three transitions.
        let store = ScriptedStore::new(
            "task-20260409-tcn1",
            vec![
                MoleculeStatus::Pending,
                MoleculeStatus::Queued,
                MoleculeStatus::Running,
                MoleculeStatus::Running,
                MoleculeStatus::Completed,
            ],
        );
        let outcome = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed],
            Duration::from_secs(60),
            Duration::from_millis(1),
            |_| (),
            || (),
        )
        .expect("should reach completed");
        assert_eq!(
            outcome.metrics.poll_count, 6,
            "five polls + one confirmation re-read of the fresh terminal transition"
        );
        assert_eq!(outcome.metrics.transitions, 3);
    }

    #[test]
    fn test_transient_terminal_observation_does_not_return_early() {
        // task-20260606-21d4 (DoD c): a status that reads in-target for a
        // single poll and is then contradicted (a flicker) must NOT satisfy
        // the wait. The molecule reads Completed once, flips back to Running,
        // and only later settles on Completed for good. `cs wait` must return
        // on the *settled* terminal, never on the transient one.
        let store = ScriptedStore::new(
            "task-20260606-flk1",
            vec![
                MoleculeStatus::Running,
                MoleculeStatus::Completed, // transient — contradicted on re-read
                MoleculeStatus::Running,
                MoleculeStatus::Completed, // settled — survives the re-read
            ],
        );
        let sleep_calls = RefCell::new(0usize);
        let outcome = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed],
            Duration::from_secs(60),
            Duration::from_millis(1),
            |_| *sleep_calls.borrow_mut() += 1,
            || (),
        )
        .expect("should reach the settled completed");
        assert_eq!(outcome.reached, MoleculeStatus::Completed);
        assert!(
            *sleep_calls.borrow() >= 1,
            "the transient Completed must be rejected by the confirmation re-read, \
             forcing at least one more poll cycle (sleep) before the settled terminal"
        );
    }

    #[test]
    fn test_times_out_when_condition_never_met() {
        // Script is infinite Running via the "last status sticks" rule.
        let store = ScriptedStore::new("task-20260409-tmo1", vec![MoleculeStatus::Running]);
        let err = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed],
            Duration::from_millis(5),
            Duration::from_millis(1),
            std::thread::sleep,
            || (),
        )
        .expect_err("should time out");
        match err {
            WaitError::Timeout { last_status, .. } => {
                assert_eq!(last_status, MoleculeStatus::Running);
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn test_missing_molecule_errors_immediately() {
        let id = MoleculeId::new("task-20260409-mis1").unwrap();
        let err = wait_for_status_with_sleep(
            &EmptyStore,
            &id,
            &[MoleculeStatus::Completed],
            Duration::from_secs(60),
            Duration::from_secs(1),
            |_| panic!("must not sleep when molecule is missing"),
            || (),
        )
        .expect_err("missing molecule should fail fast");
        match err {
            WaitError::MoleculeNotFound(got) => assert_eq!(got, id),
            other => panic!("expected MoleculeNotFound, got {other:?}"),
        }
    }

    #[test]
    fn test_poll_interval_is_clamped_to_remaining_budget() {
        // Remaining budget is 5ms, poll interval is 1s → clamped to ≤5ms.
        let store = ScriptedStore::new("task-20260409-clp1", vec![MoleculeStatus::Running]);
        let observed_sleep = RefCell::new(Vec::<Duration>::new());
        let err = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed],
            Duration::from_millis(5),
            Duration::from_secs(1),
            |d| observed_sleep.borrow_mut().push(d),
            || (),
        )
        .expect_err("should still time out");
        assert!(matches!(err, WaitError::Timeout { .. }));
        for d in observed_sleep.borrow().iter() {
            assert!(
                *d <= Duration::from_millis(5),
                "sleep {d:?} exceeded remaining budget"
            );
        }
    }

    #[test]
    fn test_multiple_target_statuses_either_matches() {
        // Collapsed should satisfy a wait on {Completed, Collapsed}.
        let store = ScriptedStore::new(
            "task-20260409-mul1",
            vec![MoleculeStatus::Running, MoleculeStatus::Collapsed],
        );
        let outcome = wait_for_status_with_sleep(
            &store,
            &store.id.clone(),
            &[MoleculeStatus::Completed, MoleculeStatus::Collapsed],
            Duration::from_secs(60),
            Duration::from_millis(1),
            |_| (),
            || (),
        )
        .expect("collapsed should satisfy");
        assert_eq!(outcome.reached, MoleculeStatus::Collapsed);
    }

    #[test]
    fn test_energy_surface_present_when_log_exists() {
        let tmp = TempDir::new().unwrap();
        let mol_id = MoleculeId::new("task-20260409-eng1").unwrap();
        // Two records for this molecule, one for another molecule (must be
        // filtered out). The summed result should be 1800 / 700 / 0.0135.
        append_energy_record(tmp.path(), &energy_record(&mol_id, 1000, 400, 0.0080));
        append_energy_record(tmp.path(), &energy_record(&mol_id, 800, 300, 0.0055));
        append_energy_record(
            tmp.path(),
            &energy_record(
                &MoleculeId::new("task-20260409-oth1").unwrap(),
                999,
                999,
                1.0,
            ),
        );

        let metrics = collect_molecule_energy(tmp.path(), &mol_id)
            .expect("energy metrics should surface when the log exists");
        assert_eq!(metrics.input_tokens, 1800);
        assert_eq!(metrics.output_tokens, 700);
        assert!((metrics.cost_usd - 0.0135).abs() < 1e-9);
    }

    #[test]
    fn test_energy_omitted_when_log_absent() {
        // Fresh tempdir — no log/energy.jsonl file at all.
        let tmp = TempDir::new().unwrap();
        let mol_id = MoleculeId::new("task-20260409-eng2").unwrap();
        assert!(
            collect_molecule_energy(tmp.path(), &mol_id).is_none(),
            "absent log must degrade to None so callers can omit the field"
        );
    }

    #[test]
    fn test_coupling_report_snapshot_shape_is_frozen() {
        // The coupling report must always be exactly five scalar fields so
        // humans and MCP clients see one vocabulary regardless of which
        // read-only verb they called. Widen it only via a successor ADR.
        let tmp = TempDir::new().unwrap();
        let mol_id = MoleculeId::new("task-20260409-cpl1").unwrap();
        let report = coupling_report_snapshot(tmp.path(), &mol_id);
        assert_eq!(report.poll_count, 1, "snapshot is a single observation");
        assert_eq!(
            report.transitions, 0,
            "a single observation cannot transition"
        );
        assert!(report.energy.is_none(), "no log on disk → omit-if-none");
        assert!(report.entropy.is_none());
        assert!(report.temperature.is_none());
    }

    #[test]
    fn test_coupling_report_matches_wait_energy_aggregation() {
        // The Part XVIII invariant: the snapshot bundle and the wait bundle
        // must agree on the energy aggregation for the same molecule on the
        // same log, so operators get one answer regardless of which verb
        // they called. Anything else is a contract violation.
        let tmp = TempDir::new().unwrap();
        let mol_id = MoleculeId::new("task-20260409-cpl2").unwrap();
        append_energy_record(tmp.path(), &energy_record(&mol_id, 1234, 567, 0.0321));
        append_energy_record(tmp.path(), &energy_record(&mol_id, 100, 50, 0.0010));
        let snapshot = coupling_report_snapshot(tmp.path(), &mol_id);
        let aggregated = collect_molecule_energy(tmp.path(), &mol_id).unwrap();
        assert_eq!(
            snapshot.energy.as_ref(),
            Some(&aggregated),
            "snapshot and direct aggregation must be bit-identical"
        );
    }

    #[test]
    fn test_energy_none_when_log_has_no_matching_record() {
        let tmp = TempDir::new().unwrap();
        let mol_id = MoleculeId::new("task-20260409-eng3").unwrap();
        // Log file exists, but only contains records for a different molecule.
        append_energy_record(
            tmp.path(),
            &energy_record(
                &MoleculeId::new("task-20260409-oth3").unwrap(),
                123,
                456,
                0.01,
            ),
        );
        assert!(
            collect_molecule_energy(tmp.path(), &mol_id).is_none(),
            "no matching records → omit rather than emit zeros"
        );
    }
}
