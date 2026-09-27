// SPDX-License-Identifier: AGPL-3.0-only

//! `cs wait` — block until a molecule reaches a terminal (or requested) status.
//!
//! This closes the canonical `cs tackle` / `cs wait` / `cs done` trinity so
//! scripts and agents can compose the full workflow in a single line instead
//! of writing ad-hoc polling loops:
//!
//! ```sh
//! cs tackle <mol-id> && \
//!   cs wait <mol-id> && \
//!   cs done <mol-id>
//! ```
//!
//! # Distinct perimeter
//!
//! `cs wait` is **kubectl wait**, not **kubectl watch**. It is deliberately
//! different from:
//!
//! - `cs observe` — single-shot snapshot of a molecule (no polling).
//! - `cs watch` — live, unbounded fleet view with a diff-based event log.
//! - `cs wait` — bounded polling loop on a single molecule that exits when
//!   the status reaches the target set **or** the timeout elapses.
//!
//! Three verbs, three patterns: **snapshot**, **live view**, **bounded wait**.
//!
//! # After a whisper
//!
//! `cs whisper <mol> …; cs wait <mol>` waits for the worker to answer the
//! whisper, even when the molecule is already `completed`. A whisper is a
//! speech act, not a state transition (ADR-038): it leaves `state.json`
//! untouched, so the status alone would satisfy the wait in zero seconds while
//! the worker is still acting on the correction. When the most recent whisper
//! found the molecule in the very status this wait just reached, the wait
//! continues until the worker's branch `feat/<id>` has moved past the HEAD the
//! whisper recorded **and** the worker pane is no longer working — the
//! correction commit is the observable end of the turn. It stops early if the
//! branch disappears (`cs done` ran) and fails if the pane dies with the
//! branch unmoved. A whisper answered without any commit is not observable;
//! such a wait ends on `--timeout` (exit 124), naming the whisper.
//!
//! No new status is needed. Reopening the molecule on a whisper would make a
//! perturbation drive the lifecycle, which ADR-038 rules out; a `Revising`
//! state would also have to be closed by the worker, and a worker that commits
//! and stops without a second `cs complete` would hold it open forever. The
//! branch is what `cs done` merges, so it is the fact worth waiting on.
//!
//! # Metrics for the feedback loop
//!
//! `cs wait --json` enriches its response with quantitative metrics so both
//! humans and MCP clients build intuition about what their requests cost:
//! `elapsed_seconds`, `poll_count`, `transitions` are always present;
//! `energy` (input/output tokens + cost), `entropy`, and `temperature`
//! follow an **omit-if-none** discipline and are only serialized when the
//! backing data source is available. See [`cosmon_state::wait::WaitMetrics`]
//! for the exact wire format.
//!
//! # ADR-016 coherence
//!
//! Stateless (read-only), idempotent (each call is independent — calling on
//! an already-terminal molecule returns immediately with zero polls), and
//! bounded (exits on condition or timeout; never a daemon). Works on Inert,
//! Propelled, and future Autonomous molecules without assuming anything
//! about who drives the clock.

use std::time::Duration;

use colored::Colorize;
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::wait::{wait_for_status_with_metrics_probed, WaitError};

use super::whisper::WhisperRecord;
use super::Context;

/// Exit code for a timeout — matches `timeout(1)` on GNU coreutils and
/// BSD so that shell composition feels native:
/// `cs tackle M && cs wait M --timeout 30 || echo "stuck"`.
pub const EXIT_TIMEOUT: i32 = 124;

/// Arguments for the `wait` subcommand.
///
/// Defaults mirror the 80% use case: wait up to ten minutes for the
/// molecule to reach a terminal state, polling every five seconds.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to wait on. Must be an exact ID — we never want a
    /// prefix to match the wrong molecule under a long-running wait.
    pub molecule: String,

    /// Statuses to wait for, comma-separated. Defaults to the terminal set
    /// so `cs tackle M && cs wait M && cs done M` just works.
    #[arg(long, default_value = "completed,collapsed", value_delimiter = ',')]
    pub r#for: Vec<String>,

    /// Maximum seconds to wait before giving up.
    #[arg(long, default_value_t = 600)]
    pub timeout: u64,

    /// Seconds between polls. Clamped internally to the remaining budget,
    /// so setting this larger than `--timeout` still terminates on time.
    #[arg(long, default_value_t = 5)]
    pub poll_interval: u64,

    /// Suppress per-poll progress lines — only emit the final result.
    /// Implied when `--json` is set.
    #[arg(long)]
    pub quiet: bool,
}

/// Execute the `wait` command.
///
/// Exit semantics:
/// - `0` — molecule reached one of the target statuses.
/// - `1` — error (invalid input, molecule missing, store I/O).
/// - [`EXIT_TIMEOUT`] — the timeout expired with the molecule still in a
///   non-target status. Mirrors `timeout(1)` for shell composition.
///
/// # Errors
///
/// Surfaces any setup error via [`anyhow::Error`]; timeouts and missing
/// molecules are reported via structured stderr plus a process exit and
/// therefore never return `Err` from this function.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let mol_id =
        MoleculeId::new(&args.molecule).map_err(|e| anyhow::anyhow!("invalid molecule id: {e}"))?;

    let targets = parse_statuses(&args.r#for)?;
    if targets.is_empty() {
        anyhow::bail!("--for must list at least one status");
    }

    let timeout = Duration::from_secs(args.timeout);
    let poll_interval = Duration::from_secs(args.poll_interval.max(1));

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    // Pre-flight read: emit a single "waiting…" line so humans know the
    // command is alive. `--quiet` and `--json` skip this.
    if !args.quiet && !ctx.json {
        println!(
            "{} {} for {} (timeout {}s, poll {}s)",
            "Waiting on".dimmed(),
            args.molecule.bold(),
            format_targets(&targets),
            args.timeout,
            args.poll_interval.max(1),
        );
    }

    // Runtime realized-model capture (round-3 / F-01). The wait loop is the
    // one cosmon process reliably alive during a subprocess-adapter run
    // (canonical trinity: tackle → wait → done), so every poll tick probes the
    // worker's live claude/codex session log and emits `ModelObserved` at the
    // first model-bearing turn — durable even if the worker crashes before
    // `cs complete`. Best-effort and idempotent; `cs peek` stays a reader.
    let backends =
        crate::energy_probe::discover_fleet_backends(&state_dir, &super::tmux_socket_name(ctx));
    let on_poll = || crate::energy_probe::capture_realized_runtime(&state_dir, &mol_id, &backends);

    match wait_for_status_with_metrics_probed(
        &store,
        &state_dir,
        &mol_id,
        &targets,
        timeout,
        poll_interval,
        on_poll,
    ) {
        Ok(outcome) => finish(ctx, &store, &mol_id, outcome, args),
        Err(WaitError::Timeout {
            elapsed,
            last_status,
        }) => {
            if ctx.json {
                let json = serde_json::json!({
                    "error": "timeout",
                    "molecule": mol_id.as_str(),
                    "last_status": last_status.to_string(),
                    "elapsed_seconds": elapsed.as_secs_f64(),
                    "timeout_seconds": args.timeout,
                });
                eprintln!("{}", serde_json::to_string(&json).unwrap_or_default());
            } else {
                eprintln!(
                    "cs: wait timed out after {:.1}s — {} is still {}",
                    elapsed.as_secs_f64(),
                    mol_id,
                    last_status,
                );
            }
            std::process::exit(EXIT_TIMEOUT);
        }
        Err(WaitError::MoleculeNotFound(id)) => Err(anyhow::anyhow!("molecule not found: {id}")),
        Err(WaitError::Store(msg)) => Err(anyhow::anyhow!("state store error: {msg}")),
    }
}

/// Report a reached status, after holding for an unanswered whisper if one
/// applies (see [`await_whisper_answer`]). Exits [`EXIT_TIMEOUT`] when the
/// whisper phase runs out of budget.
fn finish(
    ctx: &Context,
    store: &FileStore,
    mol_id: &MoleculeId,
    mut outcome: cosmon_state::wait::WaitOutcome,
    args: &Args,
) -> anyhow::Result<()> {
    let whisper = match await_whisper_answer(ctx, store, mol_id, &mut outcome, args) {
        Ok(answered) => answered,
        Err(WhisperWaitError::Timeout(rec)) => {
            let elapsed = outcome.elapsed.as_secs_f64();
            if ctx.json {
                let json = serde_json::json!({
                    "error": "timeout",
                    "molecule": mol_id.as_str(),
                    "last_status": outcome.reached.to_string(),
                    "elapsed_seconds": elapsed,
                    "timeout_seconds": args.timeout,
                    "unanswered_whisper": rec.ts.to_rfc3339(),
                });
                eprintln!("{}", serde_json::to_string(&json).unwrap_or_default());
            } else {
                eprintln!(
                    "cs: wait timed out after {elapsed:.1}s — {mol_id} is {}, but \
                     feat/{mol_id} has not moved since the whisper of {}",
                    outcome.reached,
                    rec.ts.to_rfc3339(),
                );
            }
            std::process::exit(EXIT_TIMEOUT);
        }
        Err(WhisperWaitError::WorkerGone(rec)) => {
            return Err(anyhow::anyhow!(
                "the worker pane of {mol_id} is gone and feat/{mol_id} has not moved \
                 since the whisper of {} — the whisper was never answered",
                rec.ts.to_rfc3339()
            ));
        }
    };
    if ctx.json {
        let mut json = render_outcome_json(&outcome)?;
        if let Some(answer) = &whisper {
            json["whisper_answered"] = serde_json::json!({
                "whispered_at": answer.record.ts.to_rfc3339(),
                "branch_head": answer.head,
            });
        }
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        render_outcome_human(&outcome);
        if let Some(answer) = &whisper {
            println!(
                "  {} of {} answered: {}",
                "whisper".dimmed(),
                answer.record.ts.to_rfc3339(),
                answer.head.as_deref().map_or_else(
                    || format!("feat/{mol_id} is gone (merged or deleted)"),
                    |h| format!("feat/{mol_id} now at {}", &h[..h.len().min(8)]),
                ),
            );
        }
    }
    Ok(())
}

/// Activity of the worker pane, as far as the whisper gate needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneActivity {
    /// The pane shows the worker mid-turn (tool calls, streaming, loading).
    Working,
    /// The pane is alive and not visibly working.
    Idle,
    /// The pane no longer exists.
    Gone,
}

/// Verdict of one whisper-gate poll. Pure: see [`whisper_turn`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum WhisperTurn {
    /// The worker has answered; carries the branch HEAD (`None` = branch gone).
    Answered(Option<String>),
    /// Keep waiting.
    Pending,
    /// The pane died with the branch unmoved — no answer can arrive.
    WorkerGone,
}

/// A whisper the wait saw answered.
struct WhisperAnswer {
    record: WhisperRecord,
    head: Option<String>,
}

/// Why the whisper phase of a wait ended without an answer.
enum WhisperWaitError {
    Timeout(WhisperRecord),
    WorkerGone(WhisperRecord),
}

/// The whisper that still holds this wait, if any.
///
/// Only a whisper that found the molecule in the status the wait just reached
/// holds it: reaching that status again proves nothing about the whisper. A
/// whisper sent to a running worker is answered by the transition itself, so
/// the canonical `cs tackle M && cs wait M` is unaffected. A whisper whose
/// branch did not exist at delivery has no commit to wait for and holds
/// nothing.
fn holding_whisper(
    record: Option<WhisperRecord>,
    reached: MoleculeStatus,
) -> Option<WhisperRecord> {
    record.filter(|r| r.molecule_status == Some(reached) && r.branch_head.is_some())
}

/// Decide whether the worker has answered `record`, given the branch HEAD and
/// pane activity observed now.
///
/// The commit alone would return while the worker is still running its gates
/// after committing; the idle pane alone would return before the pasted
/// whisper is even picked up. Both together mark the end of the turn.
fn whisper_turn(record: &WhisperRecord, head_now: Option<&str>, pane: PaneActivity) -> WhisperTurn {
    let Some(head_now) = head_now else {
        return WhisperTurn::Answered(None);
    };
    let moved = record.branch_head.as_deref() != Some(head_now);
    match (moved, pane) {
        (true, PaneActivity::Working) | (false, PaneActivity::Working | PaneActivity::Idle) => {
            WhisperTurn::Pending
        }
        (true, _) => WhisperTurn::Answered(Some(head_now.to_owned())),
        (false, PaneActivity::Gone) => WhisperTurn::WorkerGone,
    }
}

/// Observe the worker pane running in tmux session `session`.
fn pane_activity(ctx: &Context, session: &str) -> PaneActivity {
    use cosmon_transport::readiness::{detect_status, SessionStatus};
    let Ok(worker) = WorkerId::new(session) else {
        return PaneActivity::Gone;
    };
    let backend = cosmon_transport::TmuxBackend::new(super::tmux_socket_name(ctx));
    match detect_status(&backend, &worker) {
        Ok(SessionStatus::Dead) => PaneActivity::Gone,
        Ok(SessionStatus::Working | SessionStatus::Loading) => PaneActivity::Working,
        // An unreadable pane is not evidence of work; the branch still has
        // to move before the wait returns.
        Ok(_) | Err(_) => PaneActivity::Idle,
    }
}

/// Second phase of `cs wait`: when a whisper holds the wait (see
/// [`holding_whisper`]), poll until the worker has answered it, within what
/// is left of `--timeout`. Updates `outcome`'s elapsed time and poll count.
fn await_whisper_answer(
    ctx: &Context,
    store: &FileStore,
    mol_id: &MoleculeId,
    outcome: &mut cosmon_state::wait::WaitOutcome,
    args: &Args,
) -> Result<Option<WhisperAnswer>, WhisperWaitError> {
    let record =
        super::whisper::last_whisper_record(&store.molecule_dir(mol_id).join("whispers.jsonl"));
    let Some(record) = holding_whisper(record, outcome.reached) else {
        return Ok(None);
    };
    if !args.quiet && !ctx.json {
        println!(
            "{} whisper of {} — waiting for the worker's answer on feat/{mol_id}",
            "Holding for".dimmed(),
            record.ts.to_rfc3339(),
        );
    }
    let timeout = Duration::from_secs(args.timeout);
    let poll_interval = Duration::from_secs(args.poll_interval.max(1));
    // Same resolution as `cs whisper`: the pane the whisper was pasted into.
    let session = outcome
        .molecule
        .session_name
        .clone()
        .unwrap_or_else(|| mol_id.to_string());
    let started = std::time::Instant::now();
    let already = outcome.elapsed;
    loop {
        let head = super::whisper::worker_branch_head(mol_id);
        let verdict = whisper_turn(&record, head.as_deref(), pane_activity(ctx, &session));
        outcome.metrics.poll_count += 1;
        outcome.elapsed = already + started.elapsed();
        match verdict {
            WhisperTurn::Answered(head) => return Ok(Some(WhisperAnswer { record, head })),
            WhisperTurn::WorkerGone => return Err(WhisperWaitError::WorkerGone(record)),
            WhisperTurn::Pending => {}
        }
        let Some(remaining) = timeout
            .checked_sub(outcome.elapsed)
            .filter(|d| !d.is_zero())
        else {
            return Err(WhisperWaitError::Timeout(record));
        };
        std::thread::sleep(poll_interval.min(remaining));
    }
}

/// Render a successful wait outcome as the canonical JSON body emitted
/// by `cs wait --json`. Extracted as a free function to keep
/// [`run`] under the `clippy::too-many-lines` limit and to share the
/// exact wire format with sibling callers.
///
/// Optional metric fields (`energy`, `entropy`, `temperature`) follow
/// an **omit-if-none** discipline — absent probes mean the key is not
/// present in the response.
fn render_outcome_json(
    outcome: &cosmon_state::wait::WaitOutcome,
) -> anyhow::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert(
        "molecule".to_owned(),
        serde_json::Value::String(outcome.molecule.id.as_str().to_owned()),
    );
    map.insert(
        "status".to_owned(),
        serde_json::Value::String(outcome.reached.to_string()),
    );
    map.insert(
        "reached".to_owned(),
        serde_json::Value::String(outcome.reached.to_string()),
    );
    map.insert(
        "elapsed_seconds".to_owned(),
        serde_json::json!(outcome.elapsed.as_secs_f64()),
    );
    map.insert(
        "current_step".to_owned(),
        serde_json::json!(outcome.molecule.current_step),
    );
    map.insert(
        "total_steps".to_owned(),
        serde_json::json!(outcome.molecule.total_steps),
    );
    map.insert(
        "poll_count".to_owned(),
        serde_json::json!(outcome.metrics.poll_count),
    );
    map.insert(
        "transitions".to_owned(),
        serde_json::json!(outcome.metrics.transitions),
    );
    if let Some(energy) = &outcome.metrics.energy {
        map.insert("energy".to_owned(), serde_json::to_value(energy)?);
    }
    if let Some(entropy) = &outcome.metrics.entropy {
        map.insert("entropy".to_owned(), serde_json::to_value(entropy)?);
    }
    if let Some(temperature) = outcome.metrics.temperature {
        map.insert("temperature".to_owned(), serde_json::json!(temperature));
    }
    Ok(serde_json::Value::Object(map))
}

/// Print the operator-facing human summary for a successful wait. This
/// is the fast-glance feedback channel: status + wall-clock +
/// quantitative metrics (polls, transitions, and — when available —
/// the token/cost footprint so humans start building cost intuition).
fn render_outcome_human(outcome: &cosmon_state::wait::WaitOutcome) {
    println!(
        "{} {} reached {} in {:.1}s ({} polls, {} transitions)",
        outcome.reached.emoji(),
        outcome.molecule.id,
        colorize_status(outcome.reached),
        outcome.elapsed.as_secs_f64(),
        outcome.metrics.poll_count,
        outcome.metrics.transitions,
    );
    if let Some(energy) = &outcome.metrics.energy {
        // Quick operator feedback loop: surface the cost of the
        // request in-line so humans start building intuition.
        println!(
            "  {} {} in / {} out tokens — ${:.4}",
            "energy".dimmed(),
            energy.input_tokens,
            energy.output_tokens,
            energy.cost_usd,
        );
    }
}

/// Parse a list of status strings, preserving order and de-duplicating.
/// Unknown statuses fail fast — we'd rather error than silently wait
/// forever on a typo.
fn parse_statuses(raw: &[String]) -> anyhow::Result<Vec<MoleculeStatus>> {
    let mut out = Vec::with_capacity(raw.len());
    for s in raw {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: MoleculeStatus = trimmed
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid status `{trimmed}`: {e}"))?;
        if !out.contains(&parsed) {
            out.push(parsed);
        }
    }
    Ok(out)
}

/// Render a target status set for the human-readable header line.
fn format_targets(targets: &[MoleculeStatus]) -> String {
    targets
        .iter()
        .map(|s| format!("{}{}", s.emoji(), s))
        .collect::<Vec<_>>()
        .join("|")
}

/// Colorize a status for human output — matches the palette used by
/// `cs observe` so operators see a consistent vocabulary.
fn colorize_status(status: MoleculeStatus) -> String {
    let s = status.to_string();
    match status {
        MoleculeStatus::Pending => s.cyan().to_string(),
        MoleculeStatus::Queued => s.blue().to_string(),
        MoleculeStatus::Running => s.green().to_string(),
        MoleculeStatus::Frozen => s.yellow().to_string(),
        MoleculeStatus::Starved => s.magenta().to_string(),
        MoleculeStatus::Completed => s.bold().green().to_string(),
        MoleculeStatus::Collapsed => s.red().to_string(),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_statuses_defaults() {
        let parsed = parse_statuses(&["completed".to_owned(), "collapsed".to_owned()]).unwrap();
        assert_eq!(
            parsed,
            vec![MoleculeStatus::Completed, MoleculeStatus::Collapsed]
        );
    }

    #[test]
    fn test_parse_statuses_dedups() {
        let parsed = parse_statuses(&[
            "running".to_owned(),
            "running".to_owned(),
            "completed".to_owned(),
        ])
        .unwrap();
        assert_eq!(
            parsed,
            vec![MoleculeStatus::Running, MoleculeStatus::Completed]
        );
    }

    #[test]
    fn test_parse_statuses_rejects_typo() {
        let err = parse_statuses(&["compleet".to_owned()]).unwrap_err();
        assert!(err.to_string().contains("invalid status"));
    }

    #[test]
    fn test_parse_statuses_skips_blank_entries() {
        // Empty string survives `value_delimiter` on a whitespace-only --for.
        let parsed = parse_statuses(&[String::new(), "completed".to_owned()]).unwrap();
        assert_eq!(parsed, vec![MoleculeStatus::Completed]);
    }

    fn record(status: Option<MoleculeStatus>, head: Option<&str>) -> WhisperRecord {
        WhisperRecord {
            ts: chrono::Utc::now(),
            molecule_status: status,
            branch_head: head.map(str::to_owned),
        }
    }

    #[test]
    fn a_whisper_to_a_completed_molecule_holds_a_wait_that_reaches_completed() {
        let rec = record(Some(MoleculeStatus::Completed), Some("aaa"));
        assert_eq!(
            holding_whisper(Some(rec.clone()), MoleculeStatus::Completed),
            Some(rec)
        );
    }

    #[test]
    fn a_whisper_to_a_running_worker_does_not_hold_the_trinity() {
        // `cs tackle M && cs wait M && cs done M` with a mid-flight whisper:
        // the transition to completed is itself the answer.
        let rec = record(Some(MoleculeStatus::Running), Some("aaa"));
        assert_eq!(holding_whisper(Some(rec), MoleculeStatus::Completed), None);
        assert_eq!(holding_whisper(None, MoleculeStatus::Completed), None);
    }

    #[test]
    fn a_whisper_without_a_recorded_branch_or_status_holds_nothing() {
        let no_head = record(Some(MoleculeStatus::Completed), None);
        assert_eq!(
            holding_whisper(Some(no_head), MoleculeStatus::Completed),
            None
        );
        let legacy = record(None, Some("aaa"));
        assert_eq!(
            holding_whisper(Some(legacy), MoleculeStatus::Completed),
            None
        );
    }

    #[test]
    fn the_turn_ends_on_a_new_commit_with_the_pane_idle() {
        let rec = record(Some(MoleculeStatus::Completed), Some("aaa"));
        assert_eq!(
            whisper_turn(&rec, Some("bbb"), PaneActivity::Idle),
            WhisperTurn::Answered(Some("bbb".to_owned()))
        );
        assert_eq!(
            whisper_turn(&rec, Some("bbb"), PaneActivity::Gone),
            WhisperTurn::Answered(Some("bbb".to_owned()))
        );
    }

    #[test]
    fn an_idle_pane_without_a_commit_is_not_an_answer() {
        // Right after the paste the pane may still look idle: returning then
        // is the zero-second wait this gate exists to prevent.
        let rec = record(Some(MoleculeStatus::Completed), Some("aaa"));
        assert_eq!(
            whisper_turn(&rec, Some("aaa"), PaneActivity::Idle),
            WhisperTurn::Pending
        );
    }

    #[test]
    fn a_commit_while_the_worker_is_still_working_is_not_the_end_of_the_turn() {
        let rec = record(Some(MoleculeStatus::Completed), Some("aaa"));
        assert_eq!(
            whisper_turn(&rec, Some("bbb"), PaneActivity::Working),
            WhisperTurn::Pending
        );
    }

    #[test]
    fn a_dead_pane_with_an_unmoved_branch_can_never_answer() {
        let rec = record(Some(MoleculeStatus::Completed), Some("aaa"));
        assert_eq!(
            whisper_turn(&rec, Some("aaa"), PaneActivity::Gone),
            WhisperTurn::WorkerGone
        );
    }

    #[test]
    fn a_deleted_branch_ends_the_wait() {
        // `cs done` merged and deleted feat/<id>: nothing left to wait on.
        let rec = record(Some(MoleculeStatus::Completed), Some("aaa"));
        assert_eq!(
            whisper_turn(&rec, None, PaneActivity::Gone),
            WhisperTurn::Answered(None)
        );
    }
}
