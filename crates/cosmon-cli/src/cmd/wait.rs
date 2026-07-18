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
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::wait::{wait_for_status_with_metrics_probed, WaitError};

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
        Ok(outcome) => {
            if ctx.json {
                let json = render_outcome_json(&outcome)?;
                println!("{}", serde_json::to_string_pretty(&json)?);
            } else {
                render_outcome_human(&outcome);
            }
            Ok(())
        }
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
}
