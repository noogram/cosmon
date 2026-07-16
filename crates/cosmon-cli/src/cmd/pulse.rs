// SPDX-License-Identifier: AGPL-3.0-only

//! `cs pulse` — runtime-vitality reading (ADR-138 Phase 1 surface).
//!
//! The I/O shell that reads disk, computes a [`Pulse`], and prints it.
//! The pure predicate lives in `cosmon_core::vitality::vitality()`.
//!
//! ## What it measures
//!
//! Progress signal P = completions (step + molecule) in a trailing window W,
//! counted directly from `events.jsonl`. The event log is the pre-integrated
//! derivative of fleet progress (C2 from `delib-20260626-9825`): no stored
//! previous Φ, no new state store.
//!
//! ## Scheduler heartbeat source (P1.5)
//!
//! The scheduler voyant reads the **launchd-scheduler's own heartbeat log**:
//! `~/.cosmon/scheduler.state.json.events.jsonl` (path derivable from the
//! default scheduler state file `~/.cosmon/scheduler.state.json` by appending
//! `.events.jsonl`). `cosmon-scheduler tick` appends a `scheduler.ticked`
//! event to that file on every successful tick, so the file's mtime and the
//! timestamp of its last record both reflect real scheduler activity.
//!
//! If the log is **absent** → scheduler is considered dead (RED).
//! If the log is **stale** (> τ) → scheduler voyant is RED.
//! If the log is **fresh** (≤ τ) → scheduler voyant is GREEN/AMBER.
//!
//! Override the path with `--sched-log <path>` or `COSMON_SCHED_LOG` (for
//! testability and non-standard launchd configurations).
//!
//! ## Output
//!
//! Human: one line — `<glyph> <headline>` followed by a voyant strip.
//! `--json`: one NDJSON line matching the `cosmon.pulse/v1` schema.
//!
//! ## ADR-068 UX↔CLI parity
//!
//! `cs pulse` is the CLI surface; a `v`-key tab in `cs peek` is the P2
//! descent. Both consume the same `Pulse` struct. This command produces the
//! canonical reading; the peek tab will be added in P2.
//!
//! ## Patrol heartbeat voyants (drainage, propel, heal)
//!
//! Three voyants (drainage, propel, heal) key on `patrols.<name>.last_fired_at`
//! inside `~/.cosmon/scheduler.state.json` — the authoritative per-patrol fire
//! time updated by the scheduler on **every** fire regardless of what the
//! command produces. This eliminates the false-red class where a patrol fires,
//! writes nothing to its log (e.g., a detached command that produces no output),
//! and the previous log-mtime probe shows RED even though the patrol is healthy.
//!
//! Map: drainage → `galaxy-drainage`, propel → `cosmon-fleet-propel`,
//! heal → `cosmon-fleet-heal`.
//!
//! GREEN = `last_fired_at` within 2× the patrol's interval; RED = stale or key
//! absent (anti-silence: a patrol that never fired = RED).
//!
//! Override the scheduler state path with `--sched-state` or `COSMON_SCHED_STATE`
//! for testability and non-standard setups.
//!
//! ## Anti-silence guarantee
//!
//! The shell infers subsystem states from staleness: if a subsystem's last
//! tick is older than its TTL, its voyant is set to `Red` — missing data IS
//! the alarm, never silence.

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use colored::Colorize;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::vitality::{vitality, Pulse, PulseState, VitalityInputs, VoyantState};
use cosmon_state::MoleculeFilter;
use serde_json::Value;

use super::Context;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum token debit (tokens) to declare spinning.
/// A single stray retry token should not trigger the spinning alarm.
const B_MIN_TOKENS: u64 = 100;

/// Conservative monthly token cap for the Phase-1 fuel proxy.
/// Full wiring (ADR-138 `FuelBudget`) is P1.5.
const FUEL_CAP_TOKENS: u64 = 10_000_000;

/// Drainage patrol cadence in seconds (matches `interval_seconds=600` in
/// `~/.config/cosmon/patrols.toml`). The voyant threshold is 2× this value,
/// so a patrol that finds nothing to drain (ALL GALAXIES DRAINED) still
/// shows GREEN because the *log mtime* proves the patrol fired, not the
/// absence of dispatch events.
const DRAIN_PATROL_INTERVAL_SECS: f64 = 600.0;

/// Propel patrol cadence in seconds (`cosmon-fleet-propel`, 180s interval).
const PROPEL_PATROL_INTERVAL_SECS: f64 = 180.0;

/// Heal patrol cadence in seconds (`cosmon-fleet-heal`, 600s interval).
const HEAL_PATROL_INTERVAL_SECS: f64 = 600.0;

// ---------------------------------------------------------------------------
// Args
// ---------------------------------------------------------------------------

/// Arguments for `cs pulse`.
#[derive(clap::Args)]
pub struct Args {
    /// Observation window — accepts `<N>d` / `<N>h` / `<N>m` / `<N>s`.
    /// Defaults to 5 minutes (`5m`).
    #[arg(long, default_value = "5m")]
    pub window: String,

    /// Scheduler-dead threshold — age beyond which `H_sched` triggers RED.
    /// Defaults to 10 minutes (`10m`).
    #[arg(long, default_value = "10m")]
    pub sched_tau: String,

    /// Path to the launchd-scheduler heartbeat log.
    ///
    /// Defaults to `~/.cosmon/scheduler.state.json.events.jsonl` (derived
    /// from the scheduler state file by appending `.events.jsonl`).
    ///
    /// Override with `COSMON_SCHED_LOG` env var or this flag for testability
    /// or non-standard launchd setups.
    #[arg(long, env = "COSMON_SCHED_LOG")]
    pub sched_log: Option<std::path::PathBuf>,

    /// Path to `scheduler.state.json`.
    ///
    /// The drainage, propel, and heal voyants read `patrols.<name>.last_fired_at`
    /// from this file — the authoritative per-patrol fire time written by the
    /// scheduler on every dispatch, regardless of what the patrol command does.
    ///
    /// Defaults to `~/.cosmon/scheduler.state.json`.
    /// Override with `COSMON_SCHED_STATE` env var or this flag for testability.
    #[arg(long, env = "COSMON_SCHED_STATE")]
    pub sched_state: Option<std::path::PathBuf>,

    /// Emit the aggregate as a single `cosmon.pulse/v1` NDJSON line.
    /// The global `--json` flag also enables this.
    #[arg(long)]
    pub json: bool,

    /// Emit `SwiftBar`/`BitBar` formatted output for the macOS menubar plugin.
    ///
    /// First line = menubar face (colored dot + headline + `SwiftBar` params).
    /// Below `---`: the six voyant lines, fuel%, scanned, separator, action items.
    /// Consumed by `menubar/cosmon-pulse.10s.sh` which execs `cs pulse --swiftbar`.
    /// See ADR-068: `cs pulse --swiftbar` is the UI surface for `cs pulse`.
    #[arg(long)]
    pub swiftbar: bool,
}

// ---------------------------------------------------------------------------
// run()
// ---------------------------------------------------------------------------

/// Execute `cs pulse`.
///
/// # Errors
///
/// Returns I/O errors from reading `events.jsonl` or writing to stdout.
/// A missing `events.jsonl` is treated as zero events (not an error).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.state_dir();
    let window_secs = parse_duration_secs(&args.window)?;
    let sched_tau_secs = parse_duration_secs(&args.sched_tau)?;
    let now = Utc::now();
    let window_duration = duration_from_secs(window_secs);
    let cutoff = now - window_duration;

    // --- Read events.jsonl for progress + energy signals ---
    let events_path = state_dir.join("events.jsonl");
    let (progress_count, fuel_debit, scanned) = scan_events(&events_path, cutoff, now);

    // --- Read molecule counts from state store ---
    let store = ctx.store();
    let molecules = store
        .list_molecules(&MoleculeFilter::default())
        .unwrap_or_default();

    let live_work = molecules
        .iter()
        .filter(|m| matches!(m.status, MoleculeStatus::Running | MoleculeStatus::Pending))
        .count() as u64;

    let starved_count = molecules
        .iter()
        .filter(|m| matches!(m.status, MoleculeStatus::Starved))
        .count() as u64;

    // --- Live worker count: verified tmux sessions on the fleet socket(s) ---
    //
    // The workers voyant must report *live worker processes*, not the count
    // of active/pending molecules (`live_work`). A completed-but-never-
    // harvested molecule sits in `Running` status with no worker attached, so
    // `live_work` inflates far past the real worker count (the "88 shown /
    // 2 alive" menubar lie). Counting non-dead tmux panes that parse as a
    // `WorkerId` gives the number an operator means by "how many workers".
    let project_socket = super::tmux_socket_name(ctx);
    let live_workers = count_live_workers(&state_dir, &project_socket);

    // --- Scheduler age from the real launchd-scheduler heartbeat log ---
    let sched_log_path = args
        .sched_log
        .clone()
        .unwrap_or_else(default_sched_log_path);
    let sched_age_secs = read_sched_log_age_secs(&sched_log_path, now);

    // --- Fuel percentage proxy ---
    let fuel_pct = derive_fuel_pct(&events_path, now);

    // --- Subsystem voyant inference ---
    //
    // Drainage/propel/heal: read patrols.<name>.last_fired_at from
    // scheduler.state.json — the authoritative fire timestamp updated on every
    // scheduler dispatch regardless of what the patrol command produces.
    // GREEN = last_fired_at within 2× interval; RED = stale or key absent.
    let subsystem_scheduler = classify_age(sched_age_secs, sched_tau_secs);

    let sched_state_path = args
        .sched_state
        .clone()
        .unwrap_or_else(default_sched_state_path);

    // Drainage: galaxy-drainage patrol, 600s interval.
    let drain_age_secs =
        read_scheduler_state_patrol_age_secs(&sched_state_path, "galaxy-drainage", now);
    let subsystem_drainage = patrol_voyant(drain_age_secs, DRAIN_PATROL_INTERVAL_SECS);

    // Propel: cosmon-fleet-propel patrol, 180s interval.
    let propel_age_secs =
        read_scheduler_state_patrol_age_secs(&sched_state_path, "cosmon-fleet-propel", now);
    let subsystem_propel = patrol_voyant(propel_age_secs, PROPEL_PATROL_INTERVAL_SECS);

    // Heal: cosmon-fleet-heal patrol, 600s interval.
    let heal_age_secs =
        read_scheduler_state_patrol_age_secs(&sched_state_path, "cosmon-fleet-heal", now);
    let subsystem_heal = patrol_voyant(heal_age_secs, HEAL_PATROL_INTERVAL_SECS);
    let subsystem_fuel = if fuel_pct >= 1.0 {
        VoyantState::Red
    } else if fuel_pct >= 0.8 {
        VoyantState::Amber
    } else {
        VoyantState::Green
    };
    // Workers voyant shares its source with `workers_count`: live worker
    // sessions, not active-molecule status. Off when no worker is alive.
    let subsystem_workers = if live_workers == 0 {
        VoyantState::Off
    } else {
        VoyantState::Green
    };

    // --- Pure vitality computation ---
    let inputs = VitalityInputs {
        now,
        progress_count,
        fuel_debit,
        live_work,
        live_workers,
        sched_age_secs,
        starved_count,
        window_secs,
        sched_tau_secs,
        b_min_tokens: B_MIN_TOKENS,
        fuel_pct,
        scanned,
        subsystem_scheduler,
        subsystem_drainage,
        subsystem_propel,
        subsystem_heal,
        subsystem_fuel,
        subsystem_workers,
    };

    let pulse = vitality(&inputs);
    let want_json = ctx.json || args.json;
    let mut out = std::io::stdout().lock();

    if args.swiftbar {
        print_swiftbar(&mut out, &pulse)?;
    } else if want_json {
        print_json(&mut out, &pulse)?;
    } else {
        print_human(&mut out, &pulse, window_secs)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count live worker processes across every fleet socket.
///
/// "Live worker" = a tmux session on a fleet socket that owns at least one
/// non-dead pane and whose name parses as a `WorkerId`. This is the same
/// liveness predicate `cs peek` and `cs purge` use (`TransportBackend::
/// list_sessions`, which filters `pane_dead` carcasses and foreign sessions),
/// so the pulse count agrees with what the operator sees in the portal.
///
/// Why not count `Running`/`Pending` molecules? Because completed-but-never-
/// harvested molecules linger in `Running` status with no worker attached —
/// the inflation this function exists to avoid. Worker ids are deduped across
/// sockets (the project-socket fallback can overlap a named fleet socket).
///
/// Best-effort: a socket with no tmux server, or any enumeration error, simply
/// contributes zero — missing data never crashes the reading.
fn count_live_workers(state_dir: &Path, project_socket: &str) -> u64 {
    use cosmon_core::transport::TransportBackend;
    use std::collections::BTreeSet;

    let backends = crate::energy_probe::discover_fleet_backends(state_dir, project_socket);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for backend in &backends {
        if let Ok(sessions) = backend.list_sessions() {
            for s in sessions {
                seen.insert(s.worker_id.to_string());
            }
        }
    }
    seen.len() as u64
}

/// Returns the voyant state for a patrol that fires at `interval_secs` cadence.
///
/// `last_fired_age_secs` is the age of the patrol log file's mtime — how many
/// seconds ago the patrol last ran. The staleness threshold is `2 × interval`:
///
/// - age ≤ interval → `Green` (recently fired, healthy)
/// - age in (interval, 2×interval] → `Amber` (one missed cycle)
/// - age > 2×interval → `Red` (dead or disabled)
/// - age = `f64::MAX` (log absent / never ran) → `Red` (anti-silence)
///
/// All three patrol-backed voyants (drainage, propel, heal) must use this
/// helper so the threshold rule is a single source of truth.
fn patrol_voyant(last_fired_age_secs: f64, interval_secs: f64) -> VoyantState {
    classify_age(last_fired_age_secs, interval_secs * 2.0)
}

/// Classify a subsystem by the age of its last heartbeat.
fn classify_age(age_secs: f64, tau_secs: f64) -> VoyantState {
    if age_secs > tau_secs {
        VoyantState::Red
    } else if age_secs > tau_secs * 0.5 {
        VoyantState::Amber
    } else {
        VoyantState::Green
    }
}

/// Elapsed seconds from `ts` to `now`, saturated to zero.
#[allow(clippy::cast_precision_loss)]
fn secs_since(now: DateTime<Utc>, ts: DateTime<Utc>) -> f64 {
    now.signed_duration_since(ts).num_seconds().max(0) as f64
}

/// Convert a fractional seconds value to a `chrono::Duration`.
#[allow(clippy::cast_possible_truncation)]
fn duration_from_secs(secs: f64) -> Duration {
    Duration::seconds(secs as i64)
}

// ---------------------------------------------------------------------------
// Event scanning (I/O)
// ---------------------------------------------------------------------------

/// Scan `events.jsonl` for progress and fuel signals in `[cutoff, now]`.
///
/// Returns `(progress_count, fuel_debit, scanned)`.
/// Progress events: `step_completed`, `molecule_completed`,
/// `molecule_evolved` (V1 kind), `molecule_step_completed` (V2 type).
/// Energy debit: delta of `EnergyTick` / `energy_tick` cumulative totals.
///
/// Silently skips lines that fail to parse (best-effort over a partial file).
fn scan_events(path: &Path, cutoff: DateTime<Utc>, now: DateTime<Utc>) -> (u64, u64, u64) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (0, 0, 0);
    };

    let mut progress_count: u64 = 0;
    let mut energy_first: Option<u64> = None;
    let mut energy_last: Option<u64> = None;
    let mut scanned: u64 = 0;

    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(val): Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        scanned += 1;

        let Some(ts) = parse_ts(&val) else { continue };
        if ts < cutoff || ts > now {
            continue;
        }

        let kind = event_kind(&val);
        match kind {
            "step_completed"
            | "molecule_completed"
            | "molecule_evolved"
            | "molecule_step_completed" => {
                progress_count += 1;
            }
            "energy_tick" => {
                let total = energy_total(&val);
                energy_first.get_or_insert(total);
                energy_last = Some(total);
            }
            _ => {}
        }
    }

    let fuel_debit = match (energy_first, energy_last) {
        (Some(first), Some(last)) => last.saturating_sub(first),
        _ => 0,
    };

    (progress_count, fuel_debit, scanned)
}

/// Measure the age of the scheduler's heartbeat log in seconds.
///
/// Reads the file mtime of the scheduler events log (written by
/// `cosmon-scheduler tick` on every successful tick via a `scheduler.ticked`
/// event). This is the canonical freshness signal for the scheduler voyant:
///
/// - File **absent** → returns `f64::MAX` (treat as dead → RED)
/// - File **present** → returns elapsed seconds since its last modification
///
/// The mtime proxy is reliable because `cosmon-scheduler` always appends a
/// `scheduler.ticked` event after saving state — so the file is touched on
/// every tick, not just on patrol fires.
fn read_sched_log_age_secs(path: &Path, now: DateTime<Utc>) -> f64 {
    let Ok(meta) = std::fs::metadata(path) else {
        // File absent → scheduler never ran or log was deleted → treat as dead.
        return f64::MAX;
    };
    let Ok(mtime) = meta.modified() else {
        // Platform does not expose mtime (extremely rare on POSIX).
        return f64::MAX;
    };
    // Convert SystemTime to DateTime<Utc> for consistent arithmetic.
    let mtime_utc: DateTime<Utc> = mtime.into();
    secs_since(now, mtime_utc)
}

/// Default path for the scheduler heartbeat log.
///
/// Derived from the conventional scheduler state file location
/// (`~/.cosmon/scheduler.state.json`) by appending `.events.jsonl`, matching
/// the `derive_events_path` convention in `cosmon-scheduler`.
fn default_sched_log_path() -> std::path::PathBuf {
    // Use HOME to stay host-global regardless of which galaxy `cs` runs from.
    let home = std::env::var_os("HOME")
        .map_or_else(|| std::path::PathBuf::from("~"), std::path::PathBuf::from);
    home.join(".cosmon")
        .join("scheduler.state.json.events.jsonl")
}

/// Default path for the scheduler state JSON.
///
/// `~/.cosmon/scheduler.state.json` — contains `patrols.<name>.last_fired_at`
/// for every registered patrol, updated on every scheduler dispatch.
fn default_sched_state_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map_or_else(|| std::path::PathBuf::from("~"), std::path::PathBuf::from);
    home.join(".cosmon").join("scheduler.state.json")
}

/// Read the age (seconds) of a patrol's last fire from `scheduler.state.json`.
///
/// Looks up `patrols.<patrol_name>.last_fired_at` (ISO-8601) and returns how
/// many seconds ago that timestamp is relative to `now`.
///
/// Returns `f64::MAX` when:
/// - the state file is absent or unreadable (scheduler never ran),
/// - the JSON is malformed,
/// - the patrol key is absent (patrol never fired or was removed),
/// - the timestamp fails to parse.
///
/// All of these yield `Red` via `patrol_voyant` — the anti-silence invariant.
fn read_scheduler_state_patrol_age_secs(
    state_path: &Path,
    patrol_name: &str,
    now: DateTime<Utc>,
) -> f64 {
    let Ok(content) = std::fs::read_to_string(state_path) else {
        return f64::MAX;
    };
    let Ok(val): Result<Value, _> = serde_json::from_str(&content) else {
        return f64::MAX;
    };
    let Some(ts_str) = val
        .get("patrols")
        .and_then(|p| p.get(patrol_name))
        .and_then(|entry| entry.get("last_fired_at"))
        .and_then(Value::as_str)
    else {
        return f64::MAX;
    };
    let Ok(ts): Result<DateTime<Utc>, _> = ts_str.parse() else {
        return f64::MAX;
    };
    secs_since(now, ts)
}

/// Derive a rough fuel percentage proxy from `EnergyTick` cumulative totals.
///
/// Phase 1: uses the ratio of last-known cumulative vs an estimated cap of
/// 10M tokens/month. Full wiring (ADR-138 `FuelBudget`) is P1.5.
/// Returns 0.0 when no `EnergyTick` events are present.
fn derive_fuel_pct(path: &Path, now: DateTime<Utc>) -> f64 {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0.0;
    };

    let cutoff = now - Duration::days(30);
    let mut last_total: u64 = 0;

    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(val): Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        let Some(ts) = parse_ts(&val) else { continue };
        if ts < cutoff {
            continue;
        }
        if event_kind(&val) == "energy_tick" {
            let total = energy_total(&val);
            if total > last_total {
                last_total = total;
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let ratio = last_total as f64 / FUEL_CAP_TOKENS as f64;
    ratio.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Small parsing utilities
// ---------------------------------------------------------------------------

/// Extract the timestamp from a JSON event line (both V1 and V2 formats).
fn parse_ts(val: &Value) -> Option<DateTime<Utc>> {
    val.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
}

/// Extract the event discriminator ("kind" for V1, "type" for V2).
fn event_kind(val: &Value) -> &str {
    val.get("kind")
        .or_else(|| val.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Sum input + output tokens from an `energy_tick` event.
fn energy_total(val: &Value) -> u64 {
    val.get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(
            val.get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn print_json(out: &mut impl Write, pulse: &Pulse) -> anyhow::Result<()> {
    let line = serde_json::to_string(pulse)?;
    writeln!(out, "{line}")?;
    Ok(())
}

fn print_human(out: &mut impl Write, pulse: &Pulse, window_secs: f64) -> anyhow::Result<()> {
    let headline = pulse.headline();
    let glyph = pulse.state.glyph();
    let window_label = format_window(window_secs);

    let state_colored = match pulse.state {
        PulseState::Green => format!("{glyph} {headline}").green().bold().to_string(),
        PulseState::Amber => format!("{glyph} {headline}").yellow().bold().to_string(),
        PulseState::Red => format!("{glyph} {headline}").red().bold().to_string(),
    };
    writeln!(
        out,
        "{state_colored}  [{} | window {window_label}]",
        pulse.state
    )?;

    let v = &pulse.voyants;
    write!(out, "  ")?;
    write_voyant(out, v.scheduler, "scheduler")?;
    write!(out, "  ")?;
    write_voyant(out, v.drainage, "drainage")?;
    write!(out, "  ")?;
    write_voyant(out, v.propel, "propel")?;
    write!(out, "  ")?;
    write_voyant(out, v.heal, "heal")?;
    write!(out, "  ")?;
    write_voyant(out, v.fuel, "fuel")?;
    write!(out, "  ")?;
    write_voyant_with_count(out, v.workers, "workers", Some(pulse.workers_count))?;
    writeln!(out)?;

    writeln!(
        out,
        "  fuel {:.0}%  scanned {}",
        pulse.fuel_pct * 100.0,
        pulse.scanned
    )?;

    Ok(())
}

fn write_voyant(out: &mut impl Write, state: VoyantState, label: &str) -> anyhow::Result<()> {
    write_voyant_with_count(out, state, label, None)
}

fn write_voyant_with_count(
    out: &mut impl Write,
    state: VoyantState,
    label: &str,
    count: Option<u64>,
) -> anyhow::Result<()> {
    let glyph = state.glyph().to_string();
    let colored = match state {
        VoyantState::Green => glyph.green().to_string(),
        VoyantState::Amber => glyph.yellow().to_string(),
        VoyantState::Red => glyph.red().bold().to_string(),
        VoyantState::Off => glyph.dimmed().to_string(),
    };
    if let Some(n) = count {
        write!(out, "{colored} {label}  ({n})")?;
    } else {
        write!(out, "{colored} {label}")?;
    }
    Ok(())
}

/// Emit `SwiftBar`/`BitBar` format for `cs pulse --swiftbar`.
///
/// Layout mirrors the respire plugin pattern (lecture > intervention):
/// - Line 1 = menubar face: dot emoji + headline + `SwiftBar` color/font params
/// - `---`
/// - Six voyant lines: dot + name + state label
/// - Fuel% and scanned count
/// - `---`
/// - Action items: "Open cs peek" (terminal=true) and "Refresh"
///
/// The dot-emoji mapping (🟢/🟡/🔴/⚫) is the UI projection of
/// `PulseState`/`VoyantState`; formatting stays in the CLI shell per ADR-068.
fn print_swiftbar(out: &mut impl Write, pulse: &Pulse) -> anyhow::Result<()> {
    let dot = pulse_state_dot(pulse.state);
    let color = pulse_state_color(pulse.state);
    let headline = pulse.headline();

    // Menubar face — the only thing visible until the operator clicks.
    writeln!(out, "{dot} {headline} | color={color} font=Menlo-Bold")?;
    writeln!(out, "---")?;

    // Six voyants — lecture surface (no action on these lines).
    let v = &pulse.voyants;
    writeln!(
        out,
        "{} scheduler  {}",
        voyant_dot(v.scheduler),
        v.scheduler
    )?;
    writeln!(out, "{} drainage   {}", voyant_dot(v.drainage), v.drainage)?;
    writeln!(out, "{} propel     {}", voyant_dot(v.propel), v.propel)?;
    writeln!(out, "{} heal       {}", voyant_dot(v.heal), v.heal)?;
    writeln!(out, "{} fuel       {}", voyant_dot(v.fuel), v.fuel)?;
    writeln!(
        out,
        "{} workers    {}   {}",
        voyant_dot(v.workers),
        v.workers,
        pulse.workers_count
    )?;
    writeln!(
        out,
        "fuel {:.0}%  scanned {}",
        pulse.fuel_pct * 100.0,
        pulse.scanned
    )?;

    // Actions — operator-triggered only (lecture > intervention).
    writeln!(out, "---")?;
    let cs_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| "/Users/you/.local/bin/cs".to_owned());
    // cd into the cosmon project before running cs peek so the walk-up
    // discovery finds .cosmon/config.toml (SwiftBar runs from $HOME).
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/you".to_owned());
    let cosmon_dir = format!("{home}/galaxies/cosmon");
    writeln!(
        out,
        "Open cs peek | bash=/bin/bash param1=-lc param2=\"cd {cosmon_dir} && exec {cs_bin} peek\" terminal=true"
    )?;
    writeln!(out, "Refresh | refresh=true")?;

    Ok(())
}

/// Dot emoji for the overall pulse state (menubar face).
fn pulse_state_dot(state: PulseState) -> &'static str {
    match state {
        PulseState::Green => "🟢",
        PulseState::Amber => "🟡",
        PulseState::Red => "🔴",
    }
}

/// Hex color for the menubar headline text.
fn pulse_state_color(state: PulseState) -> &'static str {
    match state {
        PulseState::Green => "#19C37D",
        PulseState::Amber => "#F0A202",
        PulseState::Red => "#FF1744",
    }
}

/// Dot emoji for per-voyant lines in the `SwiftBar` dropdown.
fn voyant_dot(state: VoyantState) -> &'static str {
    match state {
        VoyantState::Off => "⚫",
        VoyantState::Green => "🟢",
        VoyantState::Amber => "🟡",
        VoyantState::Red => "🔴",
    }
}

fn format_window(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.0}s")
    } else if secs < 3600.0 {
        format!("{:.0}m", secs / 60.0)
    } else {
        format!("{:.0}h", secs / 3600.0)
    }
}

// ---------------------------------------------------------------------------
// Duration parsing
// ---------------------------------------------------------------------------

fn parse_duration_secs(s: &str) -> anyhow::Result<f64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("duration cannot be empty");
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: f64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("cannot parse duration: {s:?}"))?;
    if n < 0.0 {
        anyhow::bail!("duration cannot be negative: {s:?}");
    }
    match unit {
        "d" => Ok(n * 86400.0),
        "h" => Ok(n * 3600.0),
        "m" => Ok(n * 60.0),
        "s" => Ok(n),
        _ => anyhow::bail!("unknown duration unit {unit:?} (use d/h/m/s)"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_secs_units() {
        assert!((parse_duration_secs("5m").unwrap() - 300.0).abs() < 1e-9);
        assert!((parse_duration_secs("10m").unwrap() - 600.0).abs() < 1e-9);
        assert!((parse_duration_secs("1h").unwrap() - 3600.0).abs() < 1e-9);
        assert!((parse_duration_secs("30s").unwrap() - 30.0).abs() < 1e-9);
        assert!((parse_duration_secs("1d").unwrap() - 86400.0).abs() < 1e-9);
    }

    #[test]
    fn test_parse_duration_secs_rejects_bad_input() {
        assert!(parse_duration_secs("").is_err());
        assert!(parse_duration_secs("5y").is_err());
        assert!(parse_duration_secs("-1m").is_err());
    }

    #[test]
    fn test_format_window() {
        assert_eq!(format_window(30.0), "30s");
        assert_eq!(format_window(300.0), "5m");
        assert_eq!(format_window(3600.0), "1h");
    }

    #[test]
    fn test_scan_events_empty_file_returns_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let (p, b, scanned) = scan_events(&path, Utc::now() - Duration::hours(1), Utc::now());
        assert_eq!(p, 0);
        assert_eq!(b, 0);
        assert_eq!(scanned, 0);
    }

    #[test]
    fn test_scan_events_counts_progress_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let now = Utc::now();
        let ts = now - Duration::minutes(1);

        let lines = vec![
            format!(
                r#"{{"timestamp":"{t}","kind":"step_completed","molecule_id":"m1","step":0,"total":2}}"#,
                t = ts.to_rfc3339()
            ),
            format!(
                r#"{{"timestamp":"{t}","kind":"molecule_completed","molecule_id":"m1","reason":"done"}}"#,
                t = ts.to_rfc3339()
            ),
            format!(
                r#"{{"timestamp":"{t}","type":"molecule_step_completed","molecule_id":"m2","step":1,"total":3}}"#,
                t = ts.to_rfc3339()
            ),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let (p, _b, scanned) = scan_events(&path, now - Duration::minutes(5), now);
        assert_eq!(p, 3);
        assert_eq!(scanned, 3);
    }

    #[test]
    fn test_scan_events_ignores_outside_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let now = Utc::now();
        let old_ts = now - Duration::hours(2);
        let line = format!(
            r#"{{"timestamp":"{t}","kind":"step_completed","molecule_id":"m1","step":0,"total":1}}"#,
            t = old_ts.to_rfc3339()
        );
        std::fs::write(&path, line + "\n").unwrap();

        let (p, _b, _scanned) = scan_events(&path, now - Duration::minutes(5), now);
        assert_eq!(p, 0, "old event should be outside window");
    }

    #[test]
    fn test_derive_fuel_pct_no_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        assert!((derive_fuel_pct(&path, Utc::now()) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_classify_age() {
        // age > tau → Red
        assert_eq!(classify_age(700.0, 600.0), VoyantState::Red);
        // age in (tau*0.5, tau] → Amber
        assert_eq!(classify_age(400.0, 600.0), VoyantState::Amber);
        // age <= tau*0.5 → Green
        assert_eq!(classify_age(10.0, 600.0), VoyantState::Green);
    }

    // ---------------------------------------------------------------------------
    // Scheduler heartbeat log tests (P1.5)
    // ---------------------------------------------------------------------------

    #[test]
    fn test_sched_log_absent_returns_max() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.events.jsonl");
        let age = read_sched_log_age_secs(&path, Utc::now());
        assert!(
            age.is_infinite() || age > 1e15,
            "absent log must return f64::MAX, got {age}"
        );
    }

    #[test]
    fn test_sched_log_fresh_returns_small_age() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scheduler.state.json.events.jsonl");
        // Write a minimal scheduler.ticked line to the log.
        std::fs::write(&path, r#"{"ts":"2026-06-26T00:00:00Z","kind":"scheduler.ticked","patrol":"","detail":{"patrols_fired":0,"patrols_skipped":1}}"#).unwrap();
        let now = Utc::now();
        let age = read_sched_log_age_secs(&path, now);
        // The file was just written — age must be well within τ (10 min).
        assert!(
            age < 60.0,
            "freshly written log should have age < 60s, got {age}"
        );
        // Scheduler voyant must be GREEN at τ=600s.
        assert_eq!(
            classify_age(age, 600.0),
            VoyantState::Green,
            "fresh log must yield GREEN voyant"
        );
    }

    #[test]
    fn test_sched_log_stale_yields_red() {
        // Simulate a stale log by using a far-future `now` (the file was
        // written "long ago" relative to our synthetic clock).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scheduler.state.json.events.jsonl");
        std::fs::write(
            &path,
            r#"{"ts":"2026-06-26T00:00:00Z","kind":"scheduler.ticked","patrol":"","detail":{}}"#,
        )
        .unwrap();
        // Advance synthetic clock by 20 minutes (> τ default of 10m).
        let now = Utc::now() + Duration::minutes(20);
        let age = read_sched_log_age_secs(&path, now);
        // τ = 600s (10m). Advancing 20m means age > τ.
        assert!(
            age > 600.0,
            "stale log: age should exceed τ=600s, got {age}"
        );
        assert_eq!(
            classify_age(age, 600.0),
            VoyantState::Red,
            "stale log must yield RED voyant"
        );
    }

    // ---------------------------------------------------------------------------
    // SwiftBar formatter tests
    // ---------------------------------------------------------------------------

    fn make_green_pulse() -> Pulse {
        use cosmon_core::vitality::vitality;
        let inputs = VitalityInputs {
            now: Utc::now(),
            progress_count: 6,
            fuel_debit: 0,
            live_work: 2,
            live_workers: 2,
            sched_age_secs: 10.0,
            starved_count: 0,
            window_secs: 300.0,
            sched_tau_secs: 600.0,
            b_min_tokens: 100,
            fuel_pct: 0.4,
            scanned: 42,
            subsystem_scheduler: VoyantState::Green,
            subsystem_drainage: VoyantState::Green,
            subsystem_propel: VoyantState::Green,
            subsystem_heal: VoyantState::Green,
            subsystem_fuel: VoyantState::Green,
            subsystem_workers: VoyantState::Green,
        };
        vitality(&inputs)
    }

    fn make_red_drainage_off_pulse() -> Pulse {
        use cosmon_core::vitality::vitality;
        let inputs = VitalityInputs {
            now: Utc::now(),
            progress_count: 0,
            fuel_debit: 0,
            live_work: 2,
            live_workers: 2,
            sched_age_secs: 700.0, // > τ → subsystem_dead
            starved_count: 0,
            window_secs: 300.0,
            sched_tau_secs: 600.0,
            b_min_tokens: 100,
            fuel_pct: 0.1,
            scanned: 10,
            subsystem_scheduler: VoyantState::Green,
            subsystem_drainage: VoyantState::Off, // drainage off → DRAINAGE OFF word
            subsystem_propel: VoyantState::Green,
            subsystem_heal: VoyantState::Green,
            subsystem_fuel: VoyantState::Green,
            subsystem_workers: VoyantState::Green,
        };
        vitality(&inputs)
    }

    #[test]
    fn test_swiftbar_green_first_line() {
        let pulse = make_green_pulse();
        let mut buf = Vec::new();
        print_swiftbar(&mut buf, &pulse).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let first_line = out.lines().next().unwrap();
        // Green state → 🟢 dot + RPM headline + green color
        assert!(
            first_line.starts_with("🟢 "),
            "first line must start with green dot: {first_line}"
        );
        assert!(
            first_line.contains("rpm"),
            "first line must contain rpm: {first_line}"
        );
        assert!(
            first_line.contains("#19C37D"),
            "green color expected: {first_line}"
        );
        assert!(
            first_line.contains("Menlo-Bold"),
            "font param expected: {first_line}"
        );
    }

    #[test]
    fn test_swiftbar_red_drainage_off_first_line() {
        let pulse = make_red_drainage_off_pulse();
        let mut buf = Vec::new();
        print_swiftbar(&mut buf, &pulse).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let first_line = out.lines().next().unwrap();
        // Red state → 🔴 dot + DRAINAGE OFF word + red color
        assert!(
            first_line.starts_with("🔴 "),
            "first line must start with red dot: {first_line}"
        );
        assert!(
            first_line.contains("DRAINAGE OFF"),
            "word seizes the slot: {first_line}"
        );
        assert!(
            first_line.contains("#FF1744"),
            "red color expected: {first_line}"
        );
    }

    #[test]
    fn test_swiftbar_dropdown_voyant_lines() {
        let pulse = make_green_pulse();
        let mut buf = Vec::new();
        print_swiftbar(&mut buf, &pulse).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // All six voyant names must appear in the dropdown
        assert!(out.contains("scheduler"), "scheduler voyant missing");
        assert!(out.contains("drainage"), "drainage voyant missing");
        assert!(out.contains("propel"), "propel voyant missing");
        assert!(out.contains("heal"), "heal voyant missing");
        assert!(out.contains("fuel"), "fuel voyant missing");
        assert!(out.contains("workers"), "workers voyant missing");
        // Fuel % and scanned must appear
        assert!(out.contains("fuel "), "fuel % line missing");
        assert!(out.contains("scanned"), "scanned count missing");
        // Actions must appear
        assert!(out.contains("Open cs peek"), "peek action missing");
        assert!(out.contains("refresh=true"), "refresh action missing");
    }

    // ---------------------------------------------------------------------------
    // patrol_voyant helper — unit tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_patrol_voyant_green_within_interval() {
        // Patrol fired 400s ago with 600s interval: age(400) ≤ interval(600) → GREEN.
        assert_eq!(patrol_voyant(400.0, 600.0), VoyantState::Green);
    }

    #[test]
    fn test_patrol_voyant_amber_between_one_and_two_intervals() {
        // Patrol fired 800s ago with 600s interval: 600 < 800 ≤ 1200 → AMBER.
        assert_eq!(patrol_voyant(800.0, 600.0), VoyantState::Amber);
    }

    #[test]
    fn test_patrol_voyant_red_beyond_two_intervals() {
        // Patrol fired 1400s ago with 600s interval: 1400 > 1200 → RED.
        assert_eq!(patrol_voyant(1400.0, 600.0), VoyantState::Red);
    }

    #[test]
    fn test_patrol_voyant_red_when_log_absent() {
        // f64::MAX (absent log) → RED (anti-silence invariant).
        assert_eq!(patrol_voyant(f64::MAX, 600.0), VoyantState::Red);
    }

    // ---------------------------------------------------------------------------
    // Drainage calibration tests — patrol log mtime, not dispatch events
    // ---------------------------------------------------------------------------

    #[test]
    fn test_drainage_green_when_patrol_fired_recently() {
        // Patrol log written at real-now; simulate 400s elapsed → GREEN.
        // 400s < interval(600s) → GREEN via patrol_voyant.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("galaxy-drainage.log");
        std::fs::write(&path, "ALL GALAXIES DRAINED\n").unwrap();
        // Advance synthetic clock by 400s past file mtime.
        let now = Utc::now() + Duration::seconds(400);
        let age = read_sched_log_age_secs(&path, now);
        let state = patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS);
        assert_eq!(
            state,
            VoyantState::Green,
            "patrol fired 400s ago with 600s interval must be GREEN (age={age})"
        );
    }

    #[test]
    fn test_drainage_green_when_patrol_fired_but_no_dispatches() {
        // THE BUG SCENARIO: drainage patrol fires and logs "ALL GALAXIES DRAINED"
        // but emits zero dispatch events in events.jsonl. Old code → RED; new → GREEN.
        let dir = tempfile::tempdir().unwrap();
        let drain_log = dir.path().join("galaxy-drainage.log");
        // Patrol ran just now (mtime = filesystem-now). Simulate 400s elapsed.
        std::fs::write(&drain_log, "[tick] ALL GALAXIES DRAINED\n").unwrap();
        let now = Utc::now() + Duration::seconds(400);
        let age = read_sched_log_age_secs(&drain_log, now);
        let state = patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS);
        assert_eq!(
            state,
            VoyantState::Green,
            "drainage is GREEN when patrol fired recently even with zero dispatch events"
        );
    }

    #[test]
    fn test_drainage_red_when_patrol_too_old() {
        // Patrol fired 1400s ago with 600s interval (tau = 1200s) → RED.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("galaxy-drainage.log");
        std::fs::write(&path, "ALL GALAXIES DRAINED\n").unwrap();
        let now = Utc::now() + Duration::seconds(1400);
        let age = read_sched_log_age_secs(&path, now);
        let state = patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS);
        assert_eq!(
            state,
            VoyantState::Red,
            "patrol not fired in 1400s > 2×600s must be RED (age={age})"
        );
    }

    #[test]
    fn test_drainage_red_when_log_absent() {
        // Absent log = patrol never ran or was cleared → RED (anti-silence).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent-drain.log");
        let age = read_sched_log_age_secs(&path, Utc::now());
        let state = patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS);
        assert_eq!(
            state,
            VoyantState::Red,
            "absent log must yield RED (anti-silence)"
        );
    }

    // ---------------------------------------------------------------------------
    // Propel/Heal patrol voyants — they no longer hardcode Green
    // ---------------------------------------------------------------------------

    #[test]
    fn test_propel_voyant_green_when_log_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-propel.log");
        std::fs::write(&path, "[tick] propel ran\n").unwrap();
        // 100s << 180s interval → GREEN
        let now = Utc::now() + Duration::seconds(100);
        let age = read_sched_log_age_secs(&path, now);
        assert_eq!(
            patrol_voyant(age, PROPEL_PATROL_INTERVAL_SECS),
            VoyantState::Green
        );
    }

    #[test]
    fn test_propel_voyant_red_when_log_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-propel.log");
        std::fs::write(&path, "[tick] propel ran\n").unwrap();
        // 500s > 2×180s = 360s → RED
        let now = Utc::now() + Duration::seconds(500);
        let age = read_sched_log_age_secs(&path, now);
        assert_eq!(
            patrol_voyant(age, PROPEL_PATROL_INTERVAL_SECS),
            VoyantState::Red
        );
    }

    #[test]
    fn test_heal_voyant_green_when_log_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-heal.log");
        std::fs::write(&path, "[tick] heal ran\n").unwrap();
        // 400s < 600s interval → GREEN
        let now = Utc::now() + Duration::seconds(400);
        let age = read_sched_log_age_secs(&path, now);
        assert_eq!(
            patrol_voyant(age, HEAL_PATROL_INTERVAL_SECS),
            VoyantState::Green
        );
    }

    #[test]
    fn test_heal_voyant_red_when_log_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent-heal.log");
        let age = read_sched_log_age_secs(&path, Utc::now());
        assert_eq!(
            patrol_voyant(age, HEAL_PATROL_INTERVAL_SECS),
            VoyantState::Red
        );
    }

    // ---------------------------------------------------------------------------
    // scheduler.state.json patrol last_fired_at tests (definitive source fix)
    // ---------------------------------------------------------------------------

    fn write_sched_state(dir: &std::path::Path, patrols: serde_json::Value) -> std::path::PathBuf {
        let path = dir.join("scheduler.state.json");
        let doc = serde_json::json!({ "version": 0, "patrols": patrols });
        std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
        path
    }

    #[test]
    fn test_scheduler_state_patrol_green_within_interval() {
        // galaxy-drainage fired 400s ago with 600s interval → GREEN.
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let fired_at = now - Duration::seconds(400);
        let path = write_sched_state(
            dir.path(),
            serde_json::json!({
                "galaxy-drainage": { "last_fired_at": fired_at.to_rfc3339(), "fire_count": 5 }
            }),
        );
        let age = read_scheduler_state_patrol_age_secs(&path, "galaxy-drainage", now);
        assert!((age - 400.0).abs() < 2.0, "expected ~400s age, got {age}");
        assert_eq!(
            patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS),
            VoyantState::Green,
            "400s ago with 600s interval must be GREEN"
        );
    }

    #[test]
    fn test_scheduler_state_patrol_absent_key_red() {
        // Patrol key absent → f64::MAX → RED (anti-silence).
        let dir = tempfile::tempdir().unwrap();
        let path = write_sched_state(dir.path(), serde_json::json!({}));
        let age = read_scheduler_state_patrol_age_secs(&path, "galaxy-drainage", Utc::now());
        assert!(
            age.is_infinite() || age > 1e15,
            "absent key must yield f64::MAX, got {age}"
        );
        assert_eq!(
            patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS),
            VoyantState::Red,
            "absent patrol key must yield RED"
        );
    }

    #[test]
    fn test_scheduler_state_patrol_stale_red() {
        // Fired 2000s ago with 600s interval (tau = 1200s) → RED.
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let fired_at = now - Duration::seconds(2000);
        let path = write_sched_state(
            dir.path(),
            serde_json::json!({
                "galaxy-drainage": { "last_fired_at": fired_at.to_rfc3339(), "fire_count": 1 }
            }),
        );
        let age = read_scheduler_state_patrol_age_secs(&path, "galaxy-drainage", now);
        assert!(age > 1900.0, "expected ~2000s age, got {age}");
        assert_eq!(
            patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS),
            VoyantState::Red,
            "2000s ago with 600s interval (tau=1200s) must be RED"
        );
    }

    #[test]
    fn test_scheduler_state_file_absent_red() {
        // Missing state file → RED (anti-silence).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let age = read_scheduler_state_patrol_age_secs(&path, "galaxy-drainage", Utc::now());
        assert!(
            age.is_infinite() || age > 1e15,
            "absent file must yield f64::MAX, got {age}"
        );
        assert_eq!(
            patrol_voyant(age, DRAIN_PATROL_INTERVAL_SECS),
            VoyantState::Red
        );
    }

    #[test]
    fn test_scheduler_state_propel_and_heal_also_read_state_json() {
        // Verify all three patrol names resolve correctly from the same state file.
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let recent = now - Duration::seconds(50);
        let path = write_sched_state(
            dir.path(),
            serde_json::json!({
                "galaxy-drainage":   { "last_fired_at": recent.to_rfc3339(), "fire_count": 1 },
                "cosmon-fleet-propel": { "last_fired_at": recent.to_rfc3339(), "fire_count": 1 },
                "cosmon-fleet-heal": { "last_fired_at": recent.to_rfc3339(), "fire_count": 1 }
            }),
        );
        for (name, interval) in [
            ("galaxy-drainage", DRAIN_PATROL_INTERVAL_SECS),
            ("cosmon-fleet-propel", PROPEL_PATROL_INTERVAL_SECS),
            ("cosmon-fleet-heal", HEAL_PATROL_INTERVAL_SECS),
        ] {
            let age = read_scheduler_state_patrol_age_secs(&path, name, now);
            assert_eq!(
                patrol_voyant(age, interval),
                VoyantState::Green,
                "patrol {name} fired 50s ago must be GREEN"
            );
        }
    }

    #[test]
    fn test_workers_count_in_human_output() {
        let pulse = make_green_pulse(); // live_workers=2 → workers_count=2
        let mut buf = Vec::new();
        print_human(&mut buf, &pulse, 300.0).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // workers voyant line must include the count in parentheses
        assert!(
            out.contains("workers  (2)") || out.contains("workers  ("),
            "workers count missing from human output: {out}"
        );
    }

    #[test]
    fn test_workers_count_in_swiftbar_output() {
        let pulse = make_green_pulse(); // live_workers=2 → workers_count=2
        let mut buf = Vec::new();
        print_swiftbar(&mut buf, &pulse).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // workers line in dropdown must show the numeric count
        let workers_line = out.lines().find(|l| l.contains("workers")).unwrap_or("");
        assert!(
            workers_line.contains("2"),
            "workers count missing from swiftbar workers line: {workers_line}"
        );
    }

    #[test]
    fn test_swiftbar_peek_action_contains_cd() {
        let pulse = make_green_pulse();
        let mut buf = Vec::new();
        print_swiftbar(&mut buf, &pulse).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let peek_line = out
            .lines()
            .find(|l| l.contains("Open cs peek"))
            .unwrap_or("");
        // Must use bash=/bin/bash with -lc and a cd before exec
        assert!(
            peek_line.contains("bash=/bin/bash"),
            "must delegate to /bin/bash: {peek_line}"
        );
        assert!(
            peek_line.contains("param1=-lc"),
            "must pass -lc to bash: {peek_line}"
        );
        assert!(
            peek_line.contains("galaxies/cosmon"),
            "must cd into cosmon project dir: {peek_line}"
        );
        assert!(
            peek_line.contains("peek"),
            "must invoke cs peek: {peek_line}"
        );
    }

    #[test]
    fn test_swiftbar_structure_separator_and_sections() {
        let pulse = make_green_pulse();
        let mut buf = Vec::new();
        print_swiftbar(&mut buf, &pulse).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        // Second line must be a separator
        assert_eq!(lines[1], "---", "second line must be ---");
        // There must be at least two --- separators (voyants section + actions section)
        let sep_count = lines.iter().filter(|l| **l == "---").count();
        assert!(
            sep_count >= 2,
            "expected at least 2 --- separators, got {sep_count}"
        );
    }

    #[test]
    fn test_voyant_dot_mapping() {
        assert_eq!(voyant_dot(VoyantState::Green), "🟢");
        assert_eq!(voyant_dot(VoyantState::Amber), "🟡");
        assert_eq!(voyant_dot(VoyantState::Red), "🔴");
        assert_eq!(voyant_dot(VoyantState::Off), "⚫");
    }

    #[test]
    fn test_pulse_state_dot_and_color() {
        assert_eq!(pulse_state_dot(PulseState::Green), "🟢");
        assert_eq!(pulse_state_dot(PulseState::Amber), "🟡");
        assert_eq!(pulse_state_dot(PulseState::Red), "🔴");
        assert_eq!(pulse_state_color(PulseState::Green), "#19C37D");
        assert_eq!(pulse_state_color(PulseState::Amber), "#F0A202");
        assert_eq!(pulse_state_color(PulseState::Red), "#FF1744");
    }
}
