// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-scheduler` binary — argv dispatch for the patrol scheduler.
//!
//! Two subcommands:
//!
//! - `cosmon-scheduler tick` — full pipeline: load config + state, evaluate,
//!   dispatch `WouldFire` patrols, update state atomically.
//! - `cosmon-scheduler tick --dry-run` — same evaluation without dispatch or
//!   state mutation. Use this when migrating a new `patrols.toml` before
//!   wiring it into the `LaunchAgent`.
//!
//! Planned (not yet implemented):
//!
//! - `cosmon-scheduler status` — last-fire table (Step 3, also reachable as
//!   `cs scheduler status`).
//! - `cosmon-scheduler validate` — lint-only, zero-side-effect; useful in CI.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use cosmon_scheduler::environment::shellexpand_home;
use cosmon_scheduler::{
    append_event, derive_events_path, run_sunset_action, run_sunset_hooks, tick, Config, Decision,
    DispatchOutcome, Dispatcher, EnvProbe, Environment, HookStatus, Patrol, ProcessDispatcher,
    SchedulerEvent, SchedulerState,
};

/// Unified patrol scheduler — one TOML, one binary, many patrols.
#[derive(Debug, Parser)]
#[command(name = "cosmon-scheduler", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Evaluate the config and decide what to fire this tick.
    ///
    /// Without flags, this dispatches every `WouldFire` patrol as a real
    /// subprocess and atomically updates the state file. With `--dry-run`,
    /// prints decisions and exits without dispatching or mutating state;
    /// exit non-zero if any patrol is [`Decision::Invalid`].
    ///
    /// Dry-run row types (one per patrol, one line each):
    ///   FIRE     due; dispatch would spawn now
    ///   SKIP     gate rejected (disabled, kill-switch, not-due, already-sunsetted, env unset, …)
    ///   SUNSET   `[patrol.sunset]` convergence rule fired — scheduler records
    ///            `sunset_decided_at`, emits `patrol.sunsetted`, runs `on_sunset`
    ///            hooks, then short-circuits to SKIP on every subsequent tick
    ///   INVALID  schema or cadence error (exit code 3)
    ///
    /// See `docs/probes/sample-file-convention.md` for the TSV shape read by
    /// `[patrol.sunset]`, and for the `on_sunset` hook matrix (`notify_telegram`,
    /// `write_chronicle_stub`).
    Tick {
        /// Path to the patrol config TOML. Defaults to
        /// `~/.config/cosmon/patrols.toml`.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Print decisions and exit without dispatching.
        #[arg(long)]
        dry_run: bool,
    },

    /// Lint the config and exit — zero side-effects, CI-friendly.
    ///
    /// Loads and validates `patrols.toml` without reading state, dispatching,
    /// or touching any kill-switch. Exit codes:
    ///   0  valid
    ///   2  config error (missing / unreadable / malformed TOML / semantic)
    ///
    /// Use this in CI to catch a drifted patrol field before it ships, and as
    /// the safe pre-flight when editing the file. Mirrors `cs scheduler
    /// validate` (the operator-facing alias).
    Validate {
        /// Path to the patrol config TOML. Defaults to
        /// `~/.config/cosmon/patrols.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Tick { config, dry_run } => run_tick(config, dry_run),
        Cmd::Validate { config } => run_validate(config),
    }
}

/// Load + validate the config without any side-effect, then exit.
///
/// Zero state read, zero dispatch, zero kill-switch touch — safe to run on a
/// machine where the scheduler is actively ticking. Exit `0` on a clean
/// config, `2` on any [`Config::load`] failure (the same code `tick` uses for
/// config errors, so CI can treat them uniformly).
fn run_validate(config_path: Option<PathBuf>) -> ExitCode {
    let path = config_path.unwrap_or_else(default_config_path);
    match Config::load(&path) {
        Ok(cfg) => {
            let enabled = cfg.patrols.iter().filter(|p| p.enabled).count();
            println!(
                "ok: {} valid ({} patrol(s), {} enabled)",
                path.display(),
                cfg.patrols.len(),
                enabled
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cosmon-scheduler: config error ({}): {e}", path.display());
            ExitCode::from(2)
        }
    }
}

fn run_tick(config_path: Option<PathBuf>, dry_run: bool) -> ExitCode {
    let path = config_path.unwrap_or_else(default_config_path);

    let cfg = match Config::load(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("cosmon-scheduler: config error ({}): {e}", path.display());
            return ExitCode::from(2);
        }
    };

    let env = EnvProbe;
    let state_path = PathBuf::from(shellexpand_home(&cfg.scheduler.state_file).into_owned());

    if dry_run {
        // Dry-run: read state (so gates that depend on it evaluate
        // faithfully) but never write.
        let state = match SchedulerState::load_or_default(&state_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "cosmon-scheduler: state error ({}): {e}",
                    state_path.display()
                );
                return ExitCode::from(2);
            }
        };
        let decisions = tick(&cfg, &env, &state);
        return print_and_exit(&decisions);
    }

    // Real dispatch path.
    let mut state = match SchedulerState::load_or_default(&state_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "cosmon-scheduler: state error ({}): {e}",
                state_path.display()
            );
            return ExitCode::from(2);
        }
    };

    let decisions = tick(&cfg, &env, &state);

    let now = env.now();
    let dispatcher = ProcessDispatcher;
    let events_path = derive_events_path(&state_path);
    let mut had_invalid = false;
    let mut patrols_fired: u32 = 0;
    let mut patrols_skipped: u32 = 0;

    for (name, decision) in &decisions {
        match decision {
            Decision::WouldFire => {
                let Some(patrol) = find_patrol(&cfg, name) else {
                    eprintln!("cosmon-scheduler: internal: patrol '{name}' not found");
                    continue;
                };
                match dispatcher.dispatch(patrol, &cfg.scheduler) {
                    Ok(outcome) => {
                        record_fire(&mut state, name, now, outcome);
                        patrols_fired = patrols_fired.saturating_add(1);
                        println!(
                            "{label:<7} {name}  ({detail})",
                            label = "FIRE",
                            detail = format_outcome(outcome)
                        );
                    }
                    Err(e) => {
                        eprintln!("cosmon-scheduler: dispatch failed for '{name}': {e}");
                        // Non-fatal for the tick: other patrols still get a
                        // chance, and state is saved at end.
                    }
                }
            }
            Decision::WouldSunset { reason } => {
                handle_sunset(&cfg, &mut state, name, reason, now, &events_path);
            }
            Decision::WouldSkip { .. } | Decision::Invalid { .. } => {
                patrols_skipped = patrols_skipped.saturating_add(1);
                match decision {
                    Decision::WouldSkip { reason } => println!("SKIP    {name}  ({reason})"),
                    Decision::Invalid { reason } => {
                        println!("INVALID {name}  ({reason})");
                        had_invalid = true;
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    // Persist state even if some dispatches failed — we want
    // last_fired_at recorded for the ones that did spawn.
    if let Err(e) = state.save_atomic(&state_path) {
        eprintln!(
            "cosmon-scheduler: state save failed ({}): {e}",
            state_path.display()
        );
        return ExitCode::from(5);
    }

    // Heartbeat event — lets `cs pulse` measure the real scheduler cadence
    // by reading this file's freshness instead of proxying via worker-spawn
    // events in cosmon's own events.jsonl. Defensive I/O: a failed append
    // is logged but does not abort the tick.
    let tick_ev = SchedulerEvent::ticked(now, patrols_fired, patrols_skipped);
    if let Err(e) = append_event(&events_path, &tick_ev) {
        eprintln!(
            "cosmon-scheduler: heartbeat append failed ({}): {e}",
            events_path.display()
        );
    }

    if had_invalid {
        eprintln!("cosmon-scheduler: one or more patrols are invalid");
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_and_exit(decisions: &[(String, Decision)]) -> ExitCode {
    let mut had_invalid = false;
    for (name, decision) in decisions {
        let label = decision.label();
        let detail = decision.detail();
        if detail.is_empty() {
            println!("{label:<7} {name}");
        } else {
            println!("{label:<7} {name}  ({detail})");
        }
        if matches!(decision, Decision::Invalid { .. }) {
            had_invalid = true;
        }
    }
    if had_invalid {
        eprintln!("cosmon-scheduler: one or more patrols are invalid");
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

fn find_patrol<'a>(cfg: &'a Config, name: &str) -> Option<&'a Patrol> {
    cfg.patrols.iter().find(|p| p.name == name)
}

fn record_fire(
    state: &mut SchedulerState,
    name: &str,
    now: chrono::DateTime<chrono::Utc>,
    outcome: DispatchOutcome,
) {
    let entry = state.patrol_mut(name);
    entry.last_fired_at = Some(now);
    entry.last_pid = outcome.pid;
    entry.last_exit_code = outcome.exit_code;
    entry.fire_count = entry.fire_count.saturating_add(1);
}

fn record_sunset(state: &mut SchedulerState, name: &str, now: chrono::DateTime<chrono::Utc>) {
    let entry = state.patrol_mut(name);
    // Only set once — existing timestamp takes precedence so the record
    // preserves the first-converged time across a retry (should not
    // happen because the tick gate short-circuits, but belt-and-braces).
    if entry.sunset_decided_at.is_none() {
        entry.sunset_decided_at = Some(now);
    }
}

/// Orchestrate the sunset side-effects for a single patrol: run the
/// launchctl action, flip the idempotence flag, emit events. Extracted
/// from [`run_tick`] to keep the dispatch loop readable and to make the
/// ordering ("flip flag first, then attempt I/O") locally visible.
fn handle_sunset(
    cfg: &Config,
    state: &mut SchedulerState,
    name: &str,
    reason: &str,
    now: chrono::DateTime<chrono::Utc>,
    events_path: &std::path::Path,
) {
    let Some(patrol) = find_patrol(cfg, name) else {
        eprintln!("cosmon-scheduler: internal: patrol '{name}' not found");
        return;
    };
    let outcome = run_sunset_action(patrol);
    // Flip the idempotence flag FIRST — even if unload or event
    // emission fails afterwards, the next tick short-circuits to
    // "already sunsetted" instead of re-running the action.
    record_sunset(state, name, now);

    if let Some(err) = outcome.unload_error.as_deref() {
        let ev = SchedulerEvent::sunset_unload_failed(
            now,
            name,
            outcome.plist.as_deref().unwrap_or(""),
            err,
        );
        if let Err(e) = append_event(events_path, &ev) {
            eprintln!(
                "cosmon-scheduler: events.jsonl append failed ({}): {e}",
                events_path.display()
            );
        }
    }

    let sunsetted = SchedulerEvent::sunsetted(now, name, reason);
    if let Err(e) = append_event(events_path, &sunsetted) {
        eprintln!(
            "cosmon-scheduler: events.jsonl append failed ({}): {e}",
            events_path.display()
        );
    }

    // `on_sunset` hooks run AFTER the flag flip and AFTER the primary
    // `patrol.sunsetted` event — they are notifications, not state
    // transitions. A failing hook emits `patrol.sunset_hook_failed` but
    // never re-runs the sunset action on the next tick.
    for outcome in run_sunset_hooks(patrol, now, reason) {
        match outcome.status {
            HookStatus::Failed | HookStatus::Unknown => {
                let err = outcome
                    .error
                    .as_deref()
                    .unwrap_or("hook failed with no detail");
                let ev = SchedulerEvent::sunset_hook_failed(now, name, &outcome.name, err);
                if let Err(e) = append_event(events_path, &ev) {
                    eprintln!(
                        "cosmon-scheduler: events.jsonl append failed ({}): {e}",
                        events_path.display()
                    );
                }
                eprintln!(
                    "cosmon-scheduler: hook '{}' failed for '{name}': {err}",
                    outcome.name
                );
            }
            HookStatus::Ran | HookStatus::Skipped => {}
        }
    }

    println!("SUNSET  {name}  ({reason})");
}

fn format_outcome(outcome: DispatchOutcome) -> String {
    match (outcome.pid, outcome.exit_code) {
        (Some(pid), Some(code)) => format!("pid={pid} exit={code}"),
        (Some(pid), None) => format!("pid={pid} detached"),
        (None, Some(code)) => format!("exit={code}"),
        (None, None) => "spawned".to_owned(),
    }
}

/// `~/.config/cosmon/patrols.toml`, falling back to the literal string if
/// `HOME` is unset (which would already break the rest of the scheduler;
/// let `Config::load` produce the useful error).
fn default_config_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => {
            let mut p = PathBuf::from(home);
            p.push(".config");
            p.push("cosmon");
            p.push("patrols.toml");
            p
        }
        None => PathBuf::from("~/.config/cosmon/patrols.toml"),
    }
}
