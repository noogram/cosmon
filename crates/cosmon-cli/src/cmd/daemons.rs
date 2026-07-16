// SPDX-License-Identifier: AGPL-3.0-only

//! `cs daemons` — read-only view onto `cosmon-daemon-supervisor`'s state.
//!
//! The operator-facing mirror of the `cosmon-daemon-supervisor` binary.
//! The supervisor writes `~/.cosmon/daemon-supervisor.state.json` on every
//! step; this command reads it (and, optionally, tails the supervisor log)
//! so operators can answer "is tg-bot running?" without leaving `cs`
//! vocabulary.
//!
//! **The canonical image.** `cosmon-daemon-supervisor` is the **night
//! watchman** — it does not look at the clock, it looks at the dogs. It
//! reads its tablet (`daemons.toml`), keeps every declared daemon alive,
//! and if one dies it calls it back. The sibling `cs scheduler`
//! subcommand mirrors the **alarm clock** that fires short-lived
//! patrols on a cadence. See the chronicle entries
//! 2026-04-19 *"Le gardien des chiens, et le gardien des portes"* and
//! 2026-04-19 *"Deux métiers, deux outils"* for the full image, and
//! [ADR-053](../../../../docs/adr/053-cosmon-daemon-supervisor.md) for
//! the architectural rationale.
//!
//! Mirrors `cs scheduler status` in discipline:
//!
//! - **Zero mutation** on `list` / `status` / `logs` — nothing opens either
//!   file for writing, calls `Command::spawn`, or touches any kill-switch.
//! - **`reload`** is the single mutating verb: it `touch`es the watched
//!   config file so the supervisor's notify loop re-runs [`diff`]. It does
//!   *not* send a signal, restart the supervisor, or write state itself.
//!
//! [`diff`]: cosmon_daemon_supervisor::reload::diff

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use cosmon_daemon_supervisor::config::{expand_tilde, Config};
use cosmon_daemon_supervisor::model::ChildStatus;
use cosmon_daemon_supervisor::ports::{PersistedChild, StatePort, SupervisorState};
use cosmon_daemon_supervisor::FileStatePort;

use super::Context;

/// Arguments for `cs daemons`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: DaemonsCommand,
}

/// Subcommands for `cs daemons`.
#[derive(clap::Subcommand)]
pub enum DaemonsCommand {
    /// List declared daemons (reads `daemons.toml`, no state).
    List(ListArgs),

    /// Show current status of each supervised child (reads `state.json`).
    Status(StatusArgs),

    /// Trigger a hot-reload by touching the config file.
    ///
    /// The supervisor's notify watcher picks up the modification and
    /// runs [`diff`]; no signal is sent and no child is restarted
    /// unless its `DaemonSpec` actually changed.
    ///
    /// [`diff`]: cosmon_daemon_supervisor::reload::diff
    Reload(ReloadArgs),

    /// Tail the supervisor log (the aggregate one, not per-daemon stdout).
    Logs(LogsArgs),
}

/// Arguments for `cs daemons list`.
#[derive(clap::Args, Debug)]
pub struct ListArgs {
    /// Path to the daemons config file. Defaults to
    /// `~/.config/cosmon/daemons.toml`.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Arguments for `cs daemons status`.
#[derive(clap::Args, Debug)]
pub struct StatusArgs {
    /// Path to the daemons config file. Defaults to
    /// `~/.config/cosmon/daemons.toml`. Used to resolve the state file.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Path to the supervisor state file. Overrides the `state_file`
    /// declared in the config.
    #[arg(long, value_name = "PATH")]
    pub state_file: Option<PathBuf>,
}

/// Arguments for `cs daemons reload`.
#[derive(clap::Args, Debug)]
pub struct ReloadArgs {
    /// Path to the daemons config file. Defaults to
    /// `~/.config/cosmon/daemons.toml`.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Arguments for `cs daemons logs`.
#[derive(clap::Args, Debug)]
pub struct LogsArgs {
    /// Path to the daemons config file. Defaults to
    /// `~/.config/cosmon/daemons.toml`. Used to resolve the log file.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Path to the supervisor log file. Overrides the `log_file`
    /// declared in the config.
    #[arg(long, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    /// Number of trailing lines to print. Defaults to 50.
    #[arg(long, value_name = "N", default_value_t = 50)]
    pub lines: usize,
}

/// Default path for the daemons config file (`~/.config/cosmon/daemons.toml`).
fn default_config_path() -> PathBuf {
    home().map_or_else(
        || PathBuf::from("daemons.toml"),
        |h| h.join(".config").join("cosmon").join("daemons.toml"),
    )
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Resolve `config`, `state_file`, `log_file` given optional overrides.
///
/// If the CLI caller passed an explicit path it wins; otherwise we load
/// the config and apply `~` expansion to the declared paths.
fn resolve_paths(
    config_override: Option<&Path>,
    state_override: Option<&Path>,
    log_override: Option<&Path>,
) -> anyhow::Result<(PathBuf, Option<Config>, PathBuf, PathBuf)> {
    let config_path = config_override.map_or_else(default_config_path, Path::to_path_buf);

    let cfg = if config_path.exists() {
        match Config::load(&config_path) {
            Ok(c) => Some(c),
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to load config {}: {e}",
                    config_path.display()
                ));
            }
        }
    } else {
        None
    };

    let home = home().unwrap_or_else(|| PathBuf::from("/"));

    let state_path = state_override.map_or_else(
        || {
            cfg.as_ref().map_or_else(
                || home.join(".cosmon").join("daemon-supervisor.state.json"),
                |c| expand_tilde(&c.supervisor.state_file, &home),
            )
        },
        Path::to_path_buf,
    );

    let log_path = log_override.map_or_else(
        || {
            cfg.as_ref().map_or_else(
                || home.join(".cosmon").join("daemon-supervisor.log"),
                |c| expand_tilde(&c.supervisor.log_file, &home),
            )
        },
        Path::to_path_buf,
    );

    Ok((config_path, cfg, state_path, log_path))
}

/// Execute the `daemons` command.
///
/// # Errors
///
/// Propagates file-not-found / malformed-config errors with operator-
/// friendly context. A missing state file is **not** an error (the
/// supervisor may not have booted yet); we print a friendly message
/// instead.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        DaemonsCommand::List(a) => run_list(ctx, a),
        DaemonsCommand::Status(a) => run_status(ctx, a),
        DaemonsCommand::Reload(a) => run_reload(ctx, a),
        DaemonsCommand::Logs(a) => run_logs(ctx, a),
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn run_list(ctx: &Context, args: &ListArgs) -> anyhow::Result<()> {
    let (config_path, cfg, _, _) = resolve_paths(args.config.as_deref(), None, None)?;

    let Some(cfg) = cfg else {
        if ctx.json {
            let meta = serde_json::json!({
                "meta": {
                    "config_file": config_path.display().to_string(),
                    "exists": false,
                    "daemons_count": 0,
                }
            });
            println!("{meta}");
        } else {
            println!("no daemons config found ({}).", config_path.display());
            println!(
                "create one with `mkdir -p ~/.config/cosmon && \
                 $EDITOR ~/.config/cosmon/daemons.toml`."
            );
        }
        return Ok(());
    };

    if ctx.json {
        let meta = serde_json::json!({
            "meta": {
                "config_file": config_path.display().to_string(),
                "exists": true,
                "daemons_count": cfg.daemons.len(),
            }
        });
        println!("{meta}");
        for spec in &cfg.daemons {
            let obj = serde_json::json!({
                "name": spec.name,
                "binary": spec.binary,
                "args": spec.args,
                "enabled": spec.enabled,
                "throttle_seconds": spec.throttle_seconds,
                "kill_switch": spec.kill_switch,
                "log_stdout": spec.log_stdout,
                "log_stderr": spec.log_stderr,
            });
            println!("{obj}");
        }
        return Ok(());
    }

    if cfg.daemons.is_empty() {
        println!("config at {} declares no daemons.", config_path.display());
        return Ok(());
    }

    let name_width = cfg
        .daemons
        .iter()
        .map(|d| d.name.len())
        .max()
        .unwrap_or(6)
        .max("DAEMON".len());

    println!(
        "{:<name_width$}  {:<7}  {:>8}  BINARY",
        "DAEMON", "ENABLED", "THROTTLE"
    );
    for spec in &cfg.daemons {
        let enabled = if spec.enabled { "yes" } else { "no" };
        let throttle = format!("{}s", spec.throttle_seconds);
        println!(
            "{name:<name_width$}  {enabled:<7}  {throttle:>8}  {binary}",
            name = spec.name,
            binary = spec.binary,
        );
    }
    println!();
    println!("config: {}", config_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn run_status(ctx: &Context, args: &StatusArgs) -> anyhow::Result<()> {
    let (_, cfg, state_path, _) =
        resolve_paths(args.config.as_deref(), args.state_file.as_deref(), None)?;

    let state_exists = state_path.exists();
    let state = if state_exists {
        let port = FileStatePort::new(&state_path);
        Some(port.load().map_err(|e| {
            anyhow::anyhow!(
                "failed to load supervisor state {}: {e}",
                state_path.display()
            )
        })?)
    } else {
        None
    };

    let now = Utc::now();

    if ctx.json {
        render_status_json(cfg.as_ref(), state.as_ref(), &state_path, now);
    } else {
        render_status_table(cfg.as_ref(), state.as_ref(), &state_path, now);
    }
    Ok(())
}

fn render_status_json(
    cfg: Option<&Config>,
    state: Option<&SupervisorState>,
    state_path: &Path,
    now: DateTime<Utc>,
) {
    let meta = serde_json::json!({
        "meta": {
            "state_file": state_path.display().to_string(),
            "exists": state.is_some(),
            "daemons_count": cfg.map_or(0, |c| c.daemons.len()),
            "generated_at": now.to_rfc3339(),
        }
    });
    println!("{meta}");

    // Emit one object per declared daemon (stable order: config order).
    if let Some(cfg) = cfg {
        for spec in &cfg.daemons {
            let child = state.and_then(|s| s.children.get(&spec.name));
            let age = child
                .and_then(|c| c.last_spawn_at)
                .map(|t| (now - t).num_seconds().max(0));
            let obj = serde_json::json!({
                "name": spec.name,
                "enabled": spec.enabled,
                "status": child.map_or("unknown", |c| status_label(c.status)),
                "pid": child.and_then(|c| c.pid),
                "last_exit_code": child.and_then(|c| c.last_exit_code),
                "last_spawn_at": child
                    .and_then(|c| c.last_spawn_at)
                    .map(|t| t.to_rfc3339()),
                "last_spawn_age_seconds": age,
                "last_exit_at": child
                    .and_then(|c| c.last_exit_at)
                    .map(|t| t.to_rfc3339()),
                "respawn_count": child.map_or(0, |c| c.respawn_count),
            });
            println!("{obj}");
        }
    } else if let Some(state) = state {
        // No config, but we have state — surface what the supervisor remembers.
        for (name, child) in &state.children {
            let age = child.last_spawn_at.map(|t| (now - t).num_seconds().max(0));
            let obj = serde_json::json!({
                "name": name,
                "status": status_label(child.status),
                "pid": child.pid,
                "last_exit_code": child.last_exit_code,
                "last_spawn_at": child.last_spawn_at.map(|t| t.to_rfc3339()),
                "last_spawn_age_seconds": age,
                "last_exit_at": child.last_exit_at.map(|t| t.to_rfc3339()),
                "respawn_count": child.respawn_count,
                "config_present": false,
            });
            println!("{obj}");
        }
    }
}

fn render_status_table(
    cfg: Option<&Config>,
    state: Option<&SupervisorState>,
    state_path: &Path,
    now: DateTime<Utc>,
) {
    let Some(cfg) = cfg else {
        println!(
            "no daemons config found — run `cs daemons list --config PATH` \
             with an explicit path, or create ~/.config/cosmon/daemons.toml."
        );
        return;
    };

    if cfg.daemons.is_empty() {
        println!("config declares no daemons.");
        return;
    }

    if state.is_none() {
        println!(
            "no supervisor state at {} — the daemon supervisor has not \
             started yet (or --state-file points elsewhere).",
            state_path.display()
        );
        println!();
    }

    let name_width = cfg
        .daemons
        .iter()
        .map(|d| d.name.len())
        .max()
        .unwrap_or(6)
        .max("DAEMON".len());

    println!(
        "{:<name_width$}  {:<10}  {:>8}  {:<27}  RESPAWNS",
        "DAEMON", "STATUS", "PID", "LAST SPAWN"
    );

    for spec in &cfg.daemons {
        let child = state.and_then(|s| s.children.get(&spec.name));
        let (status_cell, pid_cell, last_spawn, respawns) = render_status_row(spec, child, now);
        println!(
            "{name:<name_width$}  {status_cell:<10}  {pid_cell:>8}  {last_spawn:<27}  {respawns}",
            name = spec.name,
        );
    }

    println!();
    if state.is_some() {
        println!("state file: {}", state_path.display());
    }
}

/// Compute the four variable columns for a single daemon row.
/// Extracted so unit tests can assert on format without stdout capture.
fn render_status_row(
    spec: &cosmon_daemon_supervisor::DaemonSpec,
    child: Option<&PersistedChild>,
    now: DateTime<Utc>,
) -> (String, String, String, u32) {
    let (status_cell, pid_cell) = match child {
        None if !spec.enabled => ("disabled".to_owned(), "-".to_owned()),
        None => ("unknown".to_owned(), "-".to_owned()),
        Some(c) => (
            status_label(c.status).to_owned(),
            c.pid.map_or_else(|| "-".to_owned(), |p| p.to_string()),
        ),
    };
    let last_spawn = match child.and_then(|c| c.last_spawn_at) {
        Some(t) => {
            let age = (now - t).num_seconds();
            format!("{} ({})", t.to_rfc3339(), humanize_age(age))
        }
        None => "no spawn yet".to_owned(),
    };
    let respawns = child.map_or(0, |c| c.respawn_count);
    (status_cell, pid_cell, last_spawn, respawns)
}

fn status_label(s: ChildStatus) -> &'static str {
    match s {
        ChildStatus::Spawning => "spawning",
        ChildStatus::Running => "running",
        ChildStatus::Exited => "exited",
        ChildStatus::Throttling => "throttling",
        ChildStatus::Respawning => "respawning",
    }
}

/// Format a signed age in seconds as `Ns` / `Nm` / `Nh` / `Nd`.
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

// ---------------------------------------------------------------------------
// reload
// ---------------------------------------------------------------------------

fn run_reload(ctx: &Context, args: &ReloadArgs) -> anyhow::Result<()> {
    let (config_path, _, _, _) = resolve_paths(args.config.as_deref(), None, None)?;

    if !config_path.exists() {
        return Err(anyhow::anyhow!(
            "config file not found: {}",
            config_path.display()
        ));
    }

    // Touch the config file so the supervisor's notify watcher re-evaluates
    // the diff. Using `File::set_modified` would be cleaner, but the stable
    // equivalent is to re-read + re-write the bytes untouched.
    touch(&config_path)
        .map_err(|e| anyhow::anyhow!("failed to touch {}: {e}", config_path.display()))?;

    if ctx.json {
        let obj = serde_json::json!({
            "reloaded": true,
            "config_file": config_path.display().to_string(),
            "method": "touch",
            "note": "supervisor will re-read on its next notify tick (~200ms debounce)",
        });
        println!("{obj}");
    } else {
        println!("touched {}", config_path.display());
        println!(
            "the daemon-supervisor will reload within ~200ms (notify \
             debounce window)."
        );
    }
    Ok(())
}

/// Update the mtime/atime of `path` to "now" without altering its contents.
///
/// Implemented as open-for-append + flush so the file's bytes are untouched
/// but the filesystem records a modification event for `notify` to observe.
fn touch(path: &Path) -> io::Result<()> {
    use std::io::Write;
    // Open without write-truncate; append flag so any writes would be
    // additive — but we write zero bytes. The simple portable trick is to
    // read + rewrite; here we use OpenOptions + set_modified equivalent
    // by reading and writing an empty byte slice after open.
    //
    // On stable Rust (no set_modified yet), we fall back to a round-trip:
    // read the whole file, rewrite it byte-for-byte. That's acceptable for
    // a config file (kilobytes at most) and it guarantees an inode mtime
    // bump that notify sees as a `Modify` event.
    let raw = fs::read(path)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    f.write_all(&raw)?;
    f.flush()?;
    // Also best-effort update mtime via utime if touch-by-content was a
    // no-op (unlikely but possible on filesystems that elide same-content
    // writes). We ignore errors here — the rewrite above is the authority.
    let _ = set_mtime_now(path);
    Ok(())
}

fn set_mtime_now(path: &Path) -> io::Result<()> {
    // Best-effort: `fs::File::set_modified` lands on stable 1.75+ (our MSRV
    // is 1.82) so we can use it.
    let f = fs::OpenOptions::new().write(true).open(path)?;
    f.set_modified(SystemTime::now())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

fn run_logs(ctx: &Context, args: &LogsArgs) -> anyhow::Result<()> {
    let (_, _, _, log_path) =
        resolve_paths(args.config.as_deref(), None, args.log_file.as_deref())?;

    match fs::read_to_string(&log_path) {
        Ok(raw) => {
            let lines: Vec<&str> = raw.lines().collect();
            let start = lines.len().saturating_sub(args.lines);
            let tail = &lines[start..];
            if ctx.json {
                let meta = serde_json::json!({
                    "log_meta": {
                        "log_file": log_path.display().to_string(),
                        "exists": true,
                        "lines_shown": tail.len(),
                    }
                });
                println!("{meta}");
                for line in tail {
                    let obj = serde_json::json!({ "log_line": line });
                    println!("{obj}");
                }
            } else {
                println!(
                    "--- last {} log line(s) from {} ---",
                    tail.len(),
                    log_path.display()
                );
                for line in tail {
                    println!("{line}");
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if ctx.json {
                let meta = serde_json::json!({
                    "log_meta": {
                        "log_file": log_path.display().to_string(),
                        "exists": false,
                    }
                });
                println!("{meta}");
            } else {
                println!("log: {} (not found)", log_path.display());
                println!(
                    "the daemon-supervisor has not yet written a log line. \
                     Check the LaunchAgent is loaded: `scripts/install-daemon-supervisor.sh status`."
                );
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to read log file {}: {e}",
                log_path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use cosmon_daemon_supervisor::config::DaemonSpec;
    use std::collections::BTreeMap;

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

    fn sample_spec(name: &str) -> DaemonSpec {
        DaemonSpec {
            name: name.into(),
            binary: "/bin/echo".into(),
            args: Vec::new(),
            throttle_seconds: 30,
            env: BTreeMap::new(),
            log_stdout: None,
            log_stderr: None,
            kill_switch: None,
            enabled: true,
        }
    }

    #[test]
    fn humanize_age_bands() {
        assert_eq!(humanize_age(0), "0s");
        assert_eq!(humanize_age(59), "59s");
        assert_eq!(humanize_age(60), "1m");
        assert_eq!(humanize_age(3600), "1h");
        assert_eq!(humanize_age(86400), "1d");
        assert_eq!(humanize_age(-3), "0s");
    }

    #[test]
    fn status_label_covers_every_variant() {
        assert_eq!(status_label(ChildStatus::Spawning), "spawning");
        assert_eq!(status_label(ChildStatus::Running), "running");
        assert_eq!(status_label(ChildStatus::Exited), "exited");
        assert_eq!(status_label(ChildStatus::Throttling), "throttling");
        assert_eq!(status_label(ChildStatus::Respawning), "respawning");
    }

    #[test]
    fn status_row_disabled_no_state() {
        let spec = DaemonSpec {
            enabled: false,
            ..sample_spec("x")
        };
        let (status, pid, spawn, respawns) = render_status_row(&spec, None, Utc::now());
        assert_eq!(status, "disabled");
        assert_eq!(pid, "-");
        assert_eq!(spawn, "no spawn yet");
        assert_eq!(respawns, 0);
    }

    #[test]
    fn status_row_unknown_when_enabled_but_no_state() {
        let spec = sample_spec("x");
        let (status, pid, _, _) = render_status_row(&spec, None, Utc::now());
        assert_eq!(status, "unknown");
        assert_eq!(pid, "-");
    }

    #[test]
    fn status_row_running_with_pid_and_spawn_age() {
        let spec = sample_spec("tg-bot");
        let child = PersistedChild {
            name: "tg-bot".into(),
            status: ChildStatus::Running,
            pid: Some(4242),
            last_exit_code: None,
            last_spawn_at: Some(dt(2026, 4, 19, 0, 0, 0)),
            last_exit_at: None,
            respawn_count: 3,
        };
        let now = dt(2026, 4, 19, 0, 1, 0);
        let (status, pid, spawn, respawns) = render_status_row(&spec, Some(&child), now);
        assert_eq!(status, "running");
        assert_eq!(pid, "4242");
        assert!(spawn.contains("1m"), "got: {spawn}");
        assert_eq!(respawns, 3);
    }

    #[test]
    fn status_missing_state_file_is_friendly_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("daemons.toml");
        fs::write(
            &cfg,
            "[supervisor]\nstate_file=\"s\"\nlog_file=\"l\"\nkill_switch=\"k\"\n",
        )
        .unwrap();
        let args = StatusArgs {
            config: Some(cfg),
            state_file: Some(tmp.path().join("no-state.json")),
        };
        run_status(&ctx(false), &args).expect("missing state is friendly");
        run_status(&ctx(true), &args).expect("missing state works in json mode too");
    }

    #[test]
    fn list_with_missing_config_is_friendly() {
        let tmp = tempfile::tempdir().unwrap();
        let args = ListArgs {
            config: Some(tmp.path().join("missing.toml")),
        };
        run_list(&ctx(false), &args).expect("missing config is friendly");
        run_list(&ctx(true), &args).expect("missing config in json mode too");
    }

    #[test]
    fn list_renders_declared_daemons() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("daemons.toml");
        fs::write(
            &cfg,
            r#"
            [supervisor]
            state_file = "~/.cosmon/s.json"
            log_file = "~/.cosmon/s.log"
            kill_switch = "~/.cosmon/k.lock"

            [[daemon]]
            name = "tg-bot"
            binary = "/bin/echo"
            "#,
        )
        .unwrap();
        let args = ListArgs { config: Some(cfg) };
        run_list(&ctx(false), &args).expect("list works");
        run_list(&ctx(true), &args).expect("list --json works");
    }

    #[test]
    fn reload_touches_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("daemons.toml");
        let body = "[supervisor]\nstate_file=\"s\"\nlog_file=\"l\"\nkill_switch=\"k\"\n";
        fs::write(&cfg, body).unwrap();
        let before = fs::metadata(&cfg).unwrap().modified().unwrap();

        // Sleep a hair so mtime can increment on coarse-grained clocks.
        std::thread::sleep(std::time::Duration::from_millis(15));

        let args = ReloadArgs {
            config: Some(cfg.clone()),
        };
        run_reload(&ctx(false), &args).expect("reload ok");

        let after = fs::metadata(&cfg).unwrap().modified().unwrap();
        assert!(after >= before, "mtime should not go backwards");

        // Content preserved bit-for-bit.
        assert_eq!(fs::read_to_string(&cfg).unwrap(), body);
    }

    #[test]
    fn reload_missing_config_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let args = ReloadArgs {
            config: Some(tmp.path().join("no-such-file.toml")),
        };
        let err = run_reload(&ctx(false), &args).expect_err("missing config is fatal");
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[test]
    fn logs_missing_file_is_friendly() {
        let tmp = tempfile::tempdir().unwrap();
        let args = LogsArgs {
            config: None,
            log_file: Some(tmp.path().join("no-log")),
            lines: 10,
        };
        run_logs(&ctx(false), &args).expect("missing log is informational");
        run_logs(&ctx(true), &args).expect("json mode also survives");
    }

    #[test]
    fn logs_tails_last_n_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("log");
        fs::write(&log, "line1\nline2\nline3\nline4\n").unwrap();
        let args = LogsArgs {
            config: None,
            log_file: Some(log),
            lines: 2,
        };
        run_logs(&ctx(false), &args).expect("tails log");
    }
}
