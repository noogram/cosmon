// SPDX-License-Identifier: AGPL-3.0-only

//! `cs scheduler status` — read-only view onto `cosmon-scheduler`'s state.
//!
//! This subcommand is the operator-facing mirror of the dedicated
//! `cosmon-scheduler` binary. The scheduler writes
//! `~/.cosmon/scheduler.state.json` on every tick; this command reads it
//! (and, optionally, tails the aggregate log) so operators can answer the
//! question "when did patrol X last run?" without leaving `cs` vocabulary.
//!
//! **The canonical image.** `cosmon-scheduler` is the house's **alarm
//! clock** — it looks at the wall clock every 60s, reads its tablet
//! (`patrols.toml`), and fires whatever was supposed to ring. Short-lived
//! gestures. Cron-like. The sibling `cs daemons` subcommand mirrors the
//! **night watchman** who keeps long-running dogs alive. See the
//! chronicle entry 2026-04-19 *"Deux métiers, deux outils"* for the full image, and
//! [ADR-050](../../../../docs/adr/050-unified-patrol-scheduler.md) for
//! the architectural rationale.
//!
//! **Zero mutation.** Nothing in this module opens either file for writing,
//! calls `Command::spawn`, or touches any patrol's kill-switch. That
//! discipline is what makes `cs scheduler status` safe to run on a machine
//! where the scheduler is actively ticking.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use cosmon_scheduler::config::{Config, ConfigError, Patrol};
use cosmon_scheduler::environment::shellexpand_home;
use cosmon_scheduler::state::{PatrolState, SchedulerState};

use super::Context;

/// Arguments for `cs scheduler`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: SchedulerCommand,
}

/// Read-only scheduler subcommands: `status` (last-fire table) and
/// `validate` (lint `patrols.toml`). Both are zero-mutation — they read the
/// state file or the config file and never dispatch a patrol.
///
/// The companion binary is `cosmon-scheduler`; its `tick --dry-run`
/// prints one row per patrol with label `FIRE` / `SKIP` / `SUNSET` /
/// `INVALID`. `SUNSET` fires at most once per patrol lifetime — when a
/// `[patrol.sunset]` convergence rule (`variance-threshold`,
/// `sample-count`, `operator-trigger-only`) declares the campaign
/// converged. After that, every subsequent `tick` short-circuits to
/// `SKIP ("already sunsetted")`. See
/// [`docs/probes/sample-file-convention.md`](../../../../docs/probes/sample-file-convention.md)
/// for the TSV shape and the `on_sunset` hook matrix.
#[derive(clap::Subcommand)]
pub enum SchedulerCommand {
    /// Show the last-known state of every patrol the scheduler has observed.
    Status(StatusArgs),

    /// Lint `patrols.toml` without firing anything — the safe pre-flight when
    /// adding or editing a patrol (success criterion (i) of the autopilot
    /// primitive). Zero side-effects: no state read, no dispatch, no
    /// kill-switch touch. Exits 0 when the file parses and validates, 1
    /// otherwise — so it doubles as a CI gate. Mirrors
    /// `cosmon-scheduler validate`.
    Validate(ValidateArgs),
}

/// Arguments for `cs scheduler validate`.
///
/// `config` defaults to the canonical `~/.config/cosmon/patrols.toml`. The
/// command performs no walk-up discovery — the scheduler is a per-operator
/// singleton, not a project-local artifact, so the path is fixed unless the
/// operator overrides it (e.g. to lint a candidate file before promoting it).
#[derive(clap::Args, Debug)]
pub struct ValidateArgs {
    /// Path to the patrol config TOML to lint.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Default path for the patrol config (`~/.config/cosmon/patrols.toml`).
fn default_patrols_path() -> PathBuf {
    let expanded = shellexpand_home("~/.config/cosmon/patrols.toml");
    PathBuf::from(expanded.into_owned())
}

/// One-phrase description of a patrol's cadence, for the validate summary.
///
/// Reflects the `interval_seconds` XOR `cron` invariant: a well-formed patrol
/// shows exactly one of the two. A malformed cadence never reaches this
/// helper because [`Config::load`] rejects it first.
fn cadence_phrase(patrol: &Patrol) -> String {
    match (patrol.interval_seconds, patrol.cron.as_deref()) {
        (Some(n), _) => format!("every {n}s"),
        (None, Some(expr)) => format!("cron {expr}"),
        (None, None) => "no cadence".to_owned(),
    }
}

/// Run `cs scheduler validate`: load + validate the config, print a per-patrol
/// summary, and map the outcome to a process exit code via the returned
/// `anyhow::Result`.
///
/// # Errors
///
/// Returns an error (mapped to a non-zero exit by `main`) when the file is
/// missing, malformed, or fails semantic validation. The error message
/// carries the full multi-line diagnostic from [`ConfigError::Invalid`] so
/// the operator sees every offending patrol in one pass.
fn run_validate(ctx: &Context, args: &ValidateArgs) -> anyhow::Result<()> {
    let path = args.config.clone().unwrap_or_else(default_patrols_path);

    let cfg = match Config::load(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            if ctx.json {
                let kind = match &e {
                    ConfigError::Io(_) => "io",
                    ConfigError::Parse(_) => "parse",
                    ConfigError::Invalid(_) => "invalid",
                };
                let obj = serde_json::json!({
                    "config": path.display().to_string(),
                    "valid": false,
                    "error_kind": kind,
                    "error": e.to_string(),
                });
                println!("{obj}");
            }
            return Err(anyhow::anyhow!("{}: {e}", path.display()));
        }
    };

    let enabled = cfg.patrols.iter().filter(|p| p.enabled).count();
    let disabled = cfg.patrols.len() - enabled;

    if ctx.json {
        let meta = serde_json::json!({
            "config": path.display().to_string(),
            "valid": true,
            "patrols_count": cfg.patrols.len(),
            "enabled_count": enabled,
            "disabled_count": disabled,
        });
        println!("{meta}");
        for p in &cfg.patrols {
            let obj = serde_json::json!({
                "patrol": p.name,
                "cadence": cadence_phrase(p),
                "enabled": p.enabled,
                "command": p.command,
                "sunset": p.sunset.as_ref().map(|s| format!("{:?}", s.strategy)),
            });
            println!("{obj}");
        }
        return Ok(());
    }

    println!(
        "✓ {} valid — {} patrol(s)",
        path.display(),
        cfg.patrols.len()
    );
    if cfg.patrols.is_empty() {
        return Ok(());
    }

    let name_width = cfg
        .patrols
        .iter()
        .map(|p| p.name.len())
        .max()
        .unwrap_or(6)
        .max("PATROL".len());

    println!("{:<name_width$}  {:<18}  STATE", "PATROL", "CADENCE");
    for p in &cfg.patrols {
        let cadence = cadence_phrase(p);
        let state = if p.enabled { "enabled" } else { "disabled" };
        let sunset = p
            .sunset
            .as_ref()
            .map(|s| format!(" · sunset {:?}", s.strategy))
            .unwrap_or_default();
        println!("{:<name_width$}  {cadence:<18}  {state}{sunset}", p.name);
    }
    Ok(())
}

/// Arguments for `cs scheduler status`.
///
/// `state_file` and `log_file` default to `~/.cosmon/scheduler.state.json`
/// and `~/.cosmon/scheduler.log`. Operators with non-standard installs can
/// override either path explicitly — the command performs no walk-up
/// discovery because there is no notion of a project-local scheduler.
#[derive(clap::Args, Debug)]
pub struct StatusArgs {
    /// Path to the scheduler state file.
    #[arg(long, value_name = "PATH")]
    pub state_file: Option<PathBuf>,

    /// Path to the aggregate scheduler log.
    #[arg(long, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    /// Tail the last N lines of the scheduler log after the status table.
    /// Omit the flag for no log output.
    #[arg(long, value_name = "N")]
    pub log_lines: Option<usize>,
}

/// Default path for the scheduler state file (`~/.cosmon/scheduler.state.json`).
fn default_state_file_path() -> PathBuf {
    let expanded = shellexpand_home("~/.cosmon/scheduler.state.json");
    PathBuf::from(expanded.into_owned())
}

/// Default path for the aggregate scheduler log (`~/.cosmon/scheduler.log`).
fn default_log_file_path() -> PathBuf {
    let expanded = shellexpand_home("~/.cosmon/scheduler.log");
    PathBuf::from(expanded.into_owned())
}

/// Execute the `scheduler` command.
///
/// # Errors
///
/// Propagates [`cosmon_scheduler::state::StateError`] on malformed JSON or
/// non-`NotFound` I/O failure. A missing state file renders as an
/// informational message, not a non-zero exit.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        SchedulerCommand::Status(status_args) => run_status(ctx, status_args),
        SchedulerCommand::Validate(validate_args) => run_validate(ctx, validate_args),
    }
}

fn run_status(ctx: &Context, args: &StatusArgs) -> anyhow::Result<()> {
    let state_path = args
        .state_file
        .clone()
        .unwrap_or_else(default_state_file_path);
    let log_path = args.log_file.clone().unwrap_or_else(default_log_file_path);

    // We need to distinguish "file missing" (friendly message) from "file
    // present but empty" (may have no patrols yet) from "file present with
    // patrols". `load_or_default` collapses the first two cases, so check
    // existence separately before delegating.
    let state_exists = state_path.exists();
    let state = if state_exists {
        Some(SchedulerState::load_or_default(&state_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to load scheduler state {}: {e}",
                state_path.display()
            )
        })?)
    } else {
        None
    };

    let now = Utc::now();

    if ctx.json {
        render_json(state.as_ref(), &state_path, now);
    } else {
        render_table(state.as_ref(), &state_path, now);
    }

    if let Some(n) = args.log_lines {
        if n > 0 {
            print_log_tail(&log_path, n, ctx.json)?;
        }
    }

    Ok(())
}

/// NDJSON output: one `{…}` object per patrol on its own line. Missing state
/// renders as a single meta line so scripts can branch on `patrols_count`.
///
/// Pretty-printing is intentionally off — NDJSON is for `jq`, not humans.
fn render_json(state: Option<&SchedulerState>, state_path: &Path, now: DateTime<Utc>) {
    let Some(state) = state else {
        let meta = serde_json::json!({
            "meta": {
                "state_file": state_path.display().to_string(),
                "exists": false,
                "patrols_count": 0,
                "generated_at": now.to_rfc3339(),
            }
        });
        println!("{meta}");
        return;
    };

    let meta = serde_json::json!({
        "meta": {
            "state_file": state_path.display().to_string(),
            "exists": true,
            "version": state.version,
            "patrols_count": state.patrols.len(),
            "generated_at": now.to_rfc3339(),
        }
    });
    println!("{meta}");

    for (name, patrol) in &state.patrols {
        let age_seconds = patrol.last_fired_at.map(|t| (now - t).num_seconds().max(0));
        let obj = serde_json::json!({
            "patrol": name,
            "last_fired_at": patrol.last_fired_at.map(|t| t.to_rfc3339()),
            "last_age_seconds": age_seconds,
            "last_exit_code": patrol.last_exit_code,
            "last_pid": patrol.last_pid,
            "fire_count": patrol.fire_count,
        });
        println!("{obj}");
    }
}

/// Human-readable table. Columns: `PATROL`, `LAST FIRED`, `FIRES`, `STATUS`.
///
/// `STATUS` is a one-phrase verdict: `ok (exit=0)`, `fail (exit=N)`,
/// `detached (pid=N)`, or `no fire yet`. When the patrol has never fired we
/// show `no fire yet` in both the timestamp and status columns so the reader
/// doesn't have to pattern-match on dashes.
fn render_table(state: Option<&SchedulerState>, state_path: &Path, now: DateTime<Utc>) {
    let Some(state) = state else {
        println!("no scheduler state found ({}).", state_path.display());
        println!("the scheduler has not completed a tick yet — run it once, or check the path.");
        return;
    };

    if state.patrols.is_empty() {
        println!(
            "scheduler state at {} is empty (no patrols observed yet).",
            state_path.display()
        );
        return;
    }

    let name_width = state
        .patrols
        .keys()
        .map(String::len)
        .max()
        .unwrap_or(6)
        .max("PATROL".len());

    println!(
        "{:<name_width$}  {:<27}  {:>5}  STATUS",
        "PATROL", "LAST FIRED", "FIRES"
    );

    for (name, patrol) in &state.patrols {
        let (last_fired, status) = render_row(patrol, now);
        let count = patrol.fire_count;
        println!("{name:<name_width$}  {last_fired:<27}  {count:>5}  {status}");
    }

    println!();
    println!(
        "state file: {} (schema v{})",
        state_path.display(),
        state.version
    );
}

/// Compute the two variable columns — `LAST FIRED` and `STATUS` — for a
/// single patrol row. Extracted so it can be unit-tested without writing to
/// `stdout`.
fn render_row(patrol: &PatrolState, now: DateTime<Utc>) -> (String, String) {
    let fired = match patrol.last_fired_at {
        Some(t) => {
            let age = (now - t).num_seconds();
            format!("{} ({})", t.to_rfc3339(), humanize_age(age))
        }
        None => "no fire yet".to_owned(),
    };

    let status = patrol_status(patrol);
    (fired, status)
}

/// Phrase the patrol's current disposition in one line.
///
/// Precedence (first match wins):
///
/// 1. Never-fired — before any exit code can exist.
/// 2. `last_exit_code == Some(0)` → `ok (exit=0)` (wait-mode success).
/// 3. `last_exit_code == Some(N)` → `fail (exit=N)` (wait-mode failure).
/// 4. `last_exit_code == None` but `last_pid` set → `detached (pid=N)`
///    (the scheduler spawned and returned; no exit code by design).
/// 5. Fallback — the schema permits a fired state with neither exit nor
///    PID (shouldn't happen in practice); render a dash rather than lie.
fn patrol_status(patrol: &PatrolState) -> String {
    if patrol.last_fired_at.is_none() {
        return "no fire yet".to_owned();
    }
    match patrol.last_exit_code {
        Some(0) => "ok (exit=0)".to_owned(),
        Some(n) => format!("fail (exit={n})"),
        None => match patrol.last_pid {
            Some(pid) => format!("detached (pid={pid})"),
            None => "-".to_owned(),
        },
    }
}

/// Format a signed age in seconds as `Ns` / `Nm` / `Nh` / `Nd`.
///
/// Negative ages happen when the scheduler host's clock drifts ahead of the
/// reader's clock; we coerce them to `0s` so the operator sees a sensible
/// string rather than `-3s`.
fn humanize_age(secs: i64) -> String {
    if secs < 0 {
        return "0s".to_owned();
    }
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m", secs / 60);
    }
    if secs < 86400 {
        return format!("{}h", secs / 3600);
    }
    format!("{}d", secs / 86400)
}

/// Print the last `n` non-empty lines of `path`. If the log file is missing
/// we emit a friendly note (not an error) — it is perfectly valid for a
/// fresh install to not yet have a log.
fn print_log_tail(path: &Path, n: usize, json: bool) -> anyhow::Result<()> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if json {
                let meta = serde_json::json!({
                    "log_meta": {
                        "log_file": path.display().to_string(),
                        "exists": false,
                    }
                });
                println!("{meta}");
            } else {
                println!();
                println!("log: {} (not found)", path.display());
            }
            return Ok(());
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to read log file {}: {e}",
                path.display()
            ));
        }
    };

    let tail: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
    let start = tail.len().saturating_sub(n);
    let slice = &tail[start..];

    if json {
        let meta = serde_json::json!({
            "log_meta": {
                "log_file": path.display().to_string(),
                "exists": true,
                "lines_shown": slice.len(),
            }
        });
        println!("{meta}");
        for line in slice {
            let obj = serde_json::json!({ "log_line": line });
            println!("{obj}");
        }
    } else {
        println!();
        println!(
            "--- last {} log line(s) from {} ---",
            slice.len(),
            path.display()
        );
        for line in slice {
            println!("{line}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ctx(json: bool) -> Context {
        Context {
            verbose: false,
            json,
            config: None,
        }
    }

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn humanize_age_covers_all_bands() {
        assert_eq!(humanize_age(0), "0s");
        assert_eq!(humanize_age(3), "3s");
        assert_eq!(humanize_age(59), "59s");
        assert_eq!(humanize_age(60), "1m");
        assert_eq!(humanize_age(59 * 60), "59m");
        assert_eq!(humanize_age(60 * 60), "1h");
        assert_eq!(humanize_age(23 * 3600), "23h");
        assert_eq!(humanize_age(24 * 3600), "1d");
        assert_eq!(humanize_age(5 * 24 * 3600), "5d");
        assert_eq!(humanize_age(-10), "0s");
    }

    #[test]
    fn patrol_status_never_fired() {
        let fresh = PatrolState::default();
        assert_eq!(patrol_status(&fresh), "no fire yet");
    }

    #[test]
    fn patrol_status_wait_ok() {
        let p = PatrolState {
            last_fired_at: Some(dt(2026, 4, 17, 0, 0, 0)),
            last_exit_code: Some(0),
            last_pid: Some(42),
            fire_count: 1,
            sunset_decided_at: None,
        };
        assert_eq!(patrol_status(&p), "ok (exit=0)");
    }

    #[test]
    fn patrol_status_wait_fail() {
        let p = PatrolState {
            last_fired_at: Some(dt(2026, 4, 17, 0, 0, 0)),
            last_exit_code: Some(137),
            last_pid: Some(42),
            fire_count: 1,
            sunset_decided_at: None,
        };
        assert_eq!(patrol_status(&p), "fail (exit=137)");
    }

    #[test]
    fn patrol_status_detached_shows_pid() {
        let p = PatrolState {
            last_fired_at: Some(dt(2026, 4, 17, 0, 0, 0)),
            last_exit_code: None,
            last_pid: Some(54321),
            fire_count: 1,
            sunset_decided_at: None,
        };
        assert_eq!(patrol_status(&p), "detached (pid=54321)");
    }

    #[test]
    fn patrol_status_fired_without_pid_or_exit_falls_back() {
        // Schema permits this; we render a dash rather than claim success.
        let p = PatrolState {
            last_fired_at: Some(dt(2026, 4, 17, 0, 0, 0)),
            last_exit_code: None,
            last_pid: None,
            fire_count: 1,
            sunset_decided_at: None,
        };
        assert_eq!(patrol_status(&p), "-");
    }

    #[test]
    fn render_row_formats_fired_and_status() {
        let now = dt(2026, 4, 18, 0, 0, 30);
        let patrol = PatrolState {
            last_fired_at: Some(dt(2026, 4, 18, 0, 0, 0)),
            last_exit_code: None,
            last_pid: Some(42),
            fire_count: 7,
            sunset_decided_at: None,
        };
        let (fired, status) = render_row(&patrol, now);
        assert!(fired.starts_with("2026-04-18T00:00:00"));
        assert!(fired.ends_with("(30s)"));
        assert_eq!(status, "detached (pid=42)");
    }

    #[test]
    fn render_row_never_fired_yields_placeholders() {
        let now = Utc::now();
        let patrol = PatrolState::default();
        let (fired, status) = render_row(&patrol, now);
        assert_eq!(fired, "no fire yet");
        assert_eq!(status, "no fire yet");
    }

    #[test]
    fn status_with_missing_state_file_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-state.json");
        let args = StatusArgs {
            state_file: Some(missing),
            log_file: Some(tmp.path().join("no-log")),
            log_lines: None,
        };
        run_status(&ctx(false), &args).expect("missing state is friendly, not fatal");
        run_status(&ctx(true), &args).expect("missing state works in json mode too");
    }

    #[test]
    fn status_with_malformed_state_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        fs::write(&path, "{ not json").unwrap();
        let args = StatusArgs {
            state_file: Some(path),
            log_file: Some(tmp.path().join("no-log")),
            log_lines: None,
        };
        let err = run_status(&ctx(false), &args).expect_err("malformed state surfaces");
        assert!(err.to_string().contains("failed to load"), "got: {err}");
    }

    #[test]
    fn status_reads_written_state_and_returns_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        let mut state = SchedulerState::default();
        state.patrols.insert(
            "hello".to_owned(),
            PatrolState {
                last_fired_at: Some(dt(2026, 4, 17, 22, 15, 1)),
                last_exit_code: None,
                last_pid: Some(99),
                fire_count: 3,
                sunset_decided_at: None,
            },
        );
        // Use the writer's canonical atomic save so we exercise the real
        // round-trip, not a hand-rolled serialize.
        state.save_atomic(&path).unwrap();

        let args = StatusArgs {
            state_file: Some(path),
            log_file: Some(tmp.path().join("no-log")),
            log_lines: None,
        };
        run_status(&ctx(false), &args).expect("reads cleanly");
        run_status(&ctx(true), &args).expect("json mode works");
    }

    #[test]
    fn log_tail_missing_file_is_friendly() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-log");
        print_log_tail(&missing, 10, false).expect("missing log is informational");
        print_log_tail(&missing, 10, true).expect("json mode also survives");
    }

    const VALID_PATROLS: &str = r#"
        [scheduler]
        state_file = "~/.cosmon/scheduler.state.json"
        log_file = "~/.cosmon/scheduler.log"
        kill_switch = "~/.cosmon/stand-down.lock"
        tick_interval_seconds = 60

        [[patrol]]
        name = "cosmon-ward-mayor"
        cron = "0 6 * * *"
        command = ["cs", "nucleate", "cosmon-ward-mayor"]
        enabled = true

        [[patrol]]
        name = "legacy-noop"
        interval_seconds = 3600
        command = ["true"]
        enabled = false
    "#;

    #[test]
    fn cadence_phrase_renders_interval_and_cron() {
        let cfg = Config::from_str_validated(VALID_PATROLS).expect("valid");
        assert_eq!(cadence_phrase(&cfg.patrols[0]), "cron 0 6 * * *");
        assert_eq!(cadence_phrase(&cfg.patrols[1]), "every 3600s");
    }

    #[test]
    fn validate_accepts_well_formed_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("patrols.toml");
        fs::write(&path, VALID_PATROLS).unwrap();
        let args = ValidateArgs { config: Some(path) };
        run_validate(&ctx(false), &args).expect("valid config passes");
        run_validate(&ctx(true), &args).expect("json mode also passes");
    }

    #[test]
    fn validate_rejects_xor_cadence_violation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("patrols.toml");
        // Both cadences set — the XOR invariant must fail validation.
        fs::write(
            &path,
            r#"
            [scheduler]
            state_file = "s"
            log_file = "l"
            kill_switch = "k"
            tick_interval_seconds = 60

            [[patrol]]
            name = "double-cadence"
            interval_seconds = 300
            cron = "0 9 * * 0"
            command = ["true"]
            "#,
        )
        .unwrap();
        let args = ValidateArgs { config: Some(path) };
        let err = run_validate(&ctx(false), &args).expect_err("XOR violation surfaces");
        assert!(err.to_string().contains("XOR"), "got: {err}");
    }

    #[test]
    fn validate_missing_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let args = ValidateArgs {
            config: Some(tmp.path().join("absent.toml")),
        };
        run_validate(&ctx(false), &args).expect_err("missing config is a hard error");
    }

    #[test]
    fn log_tail_returns_last_n_non_empty_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("log");
        fs::write(
            &path,
            "2026-04-17T22:15:01Z TICK\n\n2026-04-17T22:15:01Z patrol=foo fire=detached\n2026-04-17T22:15:02Z TICK_END\n",
        )
        .unwrap();
        print_log_tail(&path, 2, false).expect("tails log");
    }
}
