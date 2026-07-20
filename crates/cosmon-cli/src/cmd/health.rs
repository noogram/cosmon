// SPDX-License-Identifier: AGPL-3.0-only

//! `cs health` — the Witness, read-only (ADR-137 Phase 1).
//!
//! Surfaces the molecule-health anomaly catalog (ADR-137 §4) the way `cs peek`
//! surfaces fleet state: a federation-wide, **zero-mutation** snapshot of what
//! is anomalous right now. This is the L1 *detect* layer — it computes a
//! [`HealthReport`] via the pure [`cosmon_core::patrol::scan`] Witness and
//! prints it. It never collapses, dones, nudges, or touches a worker — that is
//! the Deacon (P3+), gated behind the §5 no-interference guard.
//!
//! **The load-bearing discipline (ADR-137 §2).** Every signal fed to the
//! Witness is *control-plane* state — molecule `status`, `tackled_at` /
//! `last_progress_at` timestamps, the transport liveness probe (`tmux
//! has-session`), the liveness lease. **Never** a pane glyph. The shell folds
//! these facts into [`MoleculeHealthView`]s; the Witness, which has no field
//! for rendered pane text, classifies them. A worker cannot trip `cs health`
//! by *printing* the glyphs of the rule meant to police it — the be1e SEV-1
//! cure, structural.
//!
//! **Conservative P1 defaults.** Two classes need machinery that lands later:
//! A3 (auth-dead) needs the typed adapter-exit auth probe (P4) and A7
//! (ghost-merge) needs the `events.jsonl` authorizing-`Done` check (P4). Until
//! then the shell sets `auth_probe_failed = false` and `merge_authorized =
//! true` — erring toward *missing* a stall rather than the catastrophic
//! false-positive of flagging a compliant worker (ADR-137 §12 accepted cost).
//! The types and unit tests for A3/A7 already exist in `cosmon-core`; the
//! deacon wires the probes.

use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use colored::Colorize;
use cosmon_core::patrol::{
    scan, HealthFinding, HealthReport, HealthThresholds, MoleculeHealthView,
};
use cosmon_core::run_state::Liveness;
use cosmon_core::transport::TransportBackend;
use cosmon_state::{MoleculeData, MoleculeFilter};
use cosmon_transport::TmuxBackend;

use super::Context;

/// Staleness window (seconds) after which a session-dead `Running` molecule is
/// treated as a crash-zombie (A9). Mirrors the `cs patrol --propel`
/// `stale_after` default (300 s). A control-plane threshold on a timestamp —
/// not a pane signal.
///
/// `pub(crate)` so the Deacon (`cs patrol --heal`, [`super::patrol_heal`])
/// folds views with the exact same window the read-only Witness uses — the two
/// paths must never drift (the be1e lesson: a second code path is a second bug
/// surface).
pub(crate) const LEASE_STALE_SECS: i64 = 300;

/// Arguments for the `health` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Scan every `.cosmon/state/` below the configured cluster root,
    /// the way `cs peek --all` aggregates. Without it, only the current
    /// galaxy's state store is scanned.
    #[arg(long)]
    pub all: bool,

    /// Skip the tmux liveness probe (state-only mode, for tests / headless).
    /// Session liveness is reported as `unknown`, so session-dependent classes
    /// (A1/A4/A5/A9) are conservatively not flagged.
    #[arg(long)]
    pub no_tmux: bool,
}

/// Run the read-only Witness scan and print the anomaly catalog.
///
/// Exit code (ADR-137 §7): `0` when the federation is all-healthy, `1` when
/// any finding is present — CI / monitor-friendly. The non-zero exit is raised
/// via [`std::process::exit`] *after* output so `--json` consumers still see
/// the report.
///
/// Returns `anyhow::Result<()>` to match the uniform `main.rs` dispatch table
/// (every `cmd::*::run` shares the shape); the read-only scan has no fallible
/// path of its own — store/probe failures degrade to best-effort skips — so
/// the `unnecessary_wraps` lint is allowed here deliberately.
#[allow(clippy::unnecessary_wraps)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let now = Utc::now();
    let cfg = HealthThresholds::default();

    // Collect (state_dir, backend) pairs to scan. The current galaxy always;
    // every sibling galaxy too under `--all`.
    let mut views: Vec<MoleculeHealthView> = Vec::new();

    // Current galaxy — full liveness probe via its own tmux socket.
    {
        let state_dir = ctx.state_dir();
        let backend: Option<TmuxBackend> = if args.no_tmux {
            None
        } else {
            Some(TmuxBackend::new(super::tmux_socket_name(ctx)))
        };
        collect_views(ctx, &state_dir, backend.as_ref(), now, &mut views);
    }

    // Federation — sibling galaxies. Session liveness is left `Unknown`
    // (a foreign galaxy's tmux socket is not the current one), so only
    // session-independent classes (A6/A8) flag there. Honest, not silent.
    if args.all {
        for state_dir in discover_sibling_state_dirs(&ctx.state_dir()) {
            collect_views(ctx, &state_dir, None, now, &mut views);
        }
    }

    let report = scan(&views, now, &cfg);

    if ctx.json {
        print_json(&report);
    } else {
        print_plain(&report);
    }

    if !report.is_healthy() {
        std::process::exit(1);
    }
    Ok(())
}

/// Load one state store's molecules, fold each into a [`MoleculeHealthView`],
/// and append to `views`. Best-effort: a store that fails to open is skipped
/// (mirrors `cs peek --all`'s silence-on-error), never aborting the sweep.
fn collect_views(
    ctx: &Context,
    state_dir: &Path,
    backend: Option<&TmuxBackend>,
    now: chrono::DateTime<Utc>,
    views: &mut Vec<MoleculeHealthView>,
) {
    let store = ctx.store_at(state_dir);
    let Ok(molecules) = store.list_molecules(&MoleculeFilter::default()) else {
        return;
    };
    for m in &molecules {
        // Only molecules that can be anomalous in P1: skip pending/queued/
        // frozen/collapsed — they hold no slot and have no live worker to heal.
        if !is_scannable(m) {
            continue;
        }
        views.push(fold_view(m, backend, now));
    }
}

/// Which molecules are worth scanning. The Witness classes all concern a
/// molecule that is either actively occupying a slot (`Running`/`Starved`) or
/// completed-but-not-yet-harvested (`Completed`). Pending/queued/frozen and the
/// terminal `Collapsed` are inert and excluded.
///
/// `pub(crate)` so the Deacon shares this predicate verbatim (see
/// [`LEASE_STALE_SECS`]).
pub(crate) fn is_scannable(m: &MoleculeData) -> bool {
    use cosmon_core::molecule::MoleculeStatus as S;
    matches!(m.status, S::Running | S::Starved | S::Completed)
}

/// Fold one persisted molecule + its transport liveness into the pure,
/// control-plane-only view the Witness consumes. **No pane reads.**
///
/// `pub(crate)` so the Deacon (`cs patrol --heal`) classifies off the exact
/// same view the read-only `cs health` shows — one fold, two readers, zero
/// drift.
pub(crate) fn fold_view(
    m: &MoleculeData,
    backend: Option<&TmuxBackend>,
    now: chrono::DateTime<Utc>,
) -> MoleculeHealthView {
    // Transport liveness — a control-plane probe (`tmux has-session`), never
    // pane content. `Unknown` when no backend (foreign galaxy / --no-tmux).
    let session = match (backend, m.assigned_worker.as_ref()) {
        (Some(be), Some(wid)) => match be.is_alive(wid) {
            Ok(true) => Liveness::Alive,
            Ok(false) => Liveness::Dead,
            Err(_) => Liveness::Unknown,
        },
        _ => Liveness::Unknown,
    };

    // Progress proxy folded from timestamps: a molecule whose `last_progress_at`
    // never advanced past `tackled_at` has made no progress since tackle (the
    // A1 boot-stall signal) — a timestamp comparison, not a `grep '[Pasted'`.
    let events_advanced_since_tackle = match (m.last_progress_at, m.tackled_at) {
        (Some(prog), Some(tackled)) => prog > tackled,
        // No progress timestamp at all, but tackled ⇒ treat as no advance.
        (None, Some(_)) => false,
        // No tackle timestamp ⇒ cannot assert a boot-stall; default advanced.
        _ => true,
    };

    // Liveness-lease expiry (A9, ADR-116): the session is gone AND the molecule
    // has been silent past the stale window. A probe + a timestamp — both
    // control-plane.
    let last_signal = m.last_progress_at.unwrap_or(m.updated_at);
    let lease_expired = matches!(session, Liveness::Dead)
        && now.signed_duration_since(last_signal) > Duration::seconds(LEASE_STALE_SECS);

    // "Harvested" — a molecule that has been through `cs done` carries a
    // `merged_at` even when the legacy `archived` flag was never written
    // (the common case for historical state). Both mean the slot is reclaimed,
    // so the Witness must treat either as `archived`; otherwise A8
    // (completed-unharvested) false-fires on every merged molecule in history.
    let harvested = m.archived || m.merged_at.is_some();

    MoleculeHealthView {
        molecule_id: m.id.clone(),
        status: m.status,
        session,
        tackled_at: m.tackled_at,
        last_progress_at: m.last_progress_at,
        last_output_at: m.last_output_at,
        updated_at: m.updated_at,
        // P1 has no per-step timeout reader wired; the Witness falls back to the
        // 30 min default when this is `None`.
        step_timeout: None,
        events_advanced_since_tackle,
        // P4 wires the typed adapter-exit auth probe; until then, never flag A3.
        auth_probe_failed: false,
        // `Starved` status already carries A6; no separate typed event reader yet.
        rate_limited: false,
        merged_or_archived: m.merged_at.is_some() || m.archived,
        // P4 wires the events.jsonl authorizing-`Done` check; until then assume
        // every merge was authorized (never raise an integrity alarm blind).
        merge_authorized: true,
        archived: harvested,
        lease_expired,
        // P2 wires the presence + whisper piloting guard; default not-piloted.
        piloted: false,
    }
}

/// Enumerate sibling galaxies' `.cosmon/state` directories under
/// `~/galaxies/*/`, mirroring `cs peek --all`. The current galaxy's own state
/// dir is excluded (already scanned with its live socket).
fn discover_sibling_state_dirs(current: &Path) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let galaxies_root = home.join("galaxies");
    let Ok(read) = std::fs::read_dir(&galaxies_root) else {
        return Vec::new();
    };
    let current = current.canonicalize().ok();
    let mut out = Vec::new();
    for entry in read.flatten() {
        let state = entry.path().join(".cosmon").join("state");
        if !state.exists() {
            continue;
        }
        if let (Some(cur), Ok(this)) = (&current, state.canonicalize()) {
            if &this == cur {
                continue; // don't double-scan the current galaxy
            }
        }
        out.push(state);
    }
    out.sort();
    out
}

/// Emit the report as NDJSON (one JSON object per line): a header line with the
/// aggregate, then one line per finding. Agent-first per the `--json` convention.
fn print_json(report: &HealthReport) {
    let header = serde_json::json!({
        "type": "health_report",
        "timestamp": report.timestamp.to_rfc3339(),
        "scanned": report.patrol.ensemble_size,
        "findings": report.findings.len(),
        "healthy": report.is_healthy(),
    });
    println!("{header}");
    for f in &report.findings {
        if let Ok(line) = serde_json::to_string(f) {
            println!("{line}");
        }
    }
}

/// Print the human-facing catalog — the `cs peek`-style federation snapshot.
fn print_plain(report: &HealthReport) {
    if report.is_healthy() {
        println!(
            "{} — {} molecule(s) scanned, no anomalies",
            "✓ healthy".green().bold(),
            report.patrol.ensemble_size
        );
        return;
    }

    println!(
        "{} — {} finding(s) across {} molecule(s) scanned",
        "⚠ anomalies".yellow().bold(),
        report.findings.len(),
        report.patrol.ensemble_size
    );
    println!();
    for f in &report.findings {
        print_finding(f);
    }
}

/// One catalog row. The `remedy` is **advisory** in P1 (`cs health` mutates
/// nothing); it tells the operator what the deacon *would* do.
fn print_finding(f: &HealthFinding) {
    let pilot = if f.piloted {
        " [piloted — guarded]".magenta().to_string()
    } else {
        String::new()
    };
    println!(
        "  {} {}  {}{}",
        f.class.code().red().bold(),
        f.molecule_id.to_string().bold(),
        f.class.describe(),
        pilot
    );
    println!(
        "      signal: {}   remedy (advisory): {:?}",
        format!("{:?}", f.signal).dimmed(),
        f.remedy
    );
}
