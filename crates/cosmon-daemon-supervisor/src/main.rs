// SPDX-License-Identifier: AGPL-3.0-only

//! `cosmon-daemon-supervisor` binary — the resident `LaunchAgent` process.
//!
//! Two responsibilities:
//!
//! 1. **Composition root** — resolve the paths (`~/.config/cosmon/daemons.toml`,
//!    `~/.cosmon/daemon-supervisor.state.json`, `~/.cosmon/stand-down.lock`),
//!    instantiate the adapters, and call [`cosmon_daemon_supervisor::run`].
//! 2. **CLI surface** — `--check` (validate config & exit), `--config` /
//!    `--state` / `--kill-switch` overrides for tests and debugging.
//!
//! See an internal plan note (`idea-20260419-25fd`)
//! for the rollout plan; task 3 adds `cs daemons` wrappers that reach into
//! this binary's state file.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use cosmon_daemon_supervisor::config::{expand_tilde, Config};
use cosmon_daemon_supervisor::{run, Supervisor};

/// Meta-LaunchAgent supervisor for Cosmon-managed daemons.
///
/// Reads a TOML file declaring every long-running child process (tg-bot,
/// emacs, almanac, archive-service, …), supervises them with `KeepAlive`-style
/// semantics, and exposes a single `LaunchAgent` to the operator instead
/// of one plist per daemon.
#[derive(Debug, Parser)]
#[command(name = "cosmon-daemon-supervisor", version, about)]
struct Cli {
    /// Path to the config TOML. Defaults to `~/.config/cosmon/daemons.toml`.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Path to the state file. Defaults to
    /// `~/.cosmon/daemon-supervisor.state.json`.
    #[arg(long)]
    state: Option<PathBuf>,

    /// Path to the global kill-switch lockfile. Defaults to the value in
    /// the config's `[supervisor].kill_switch` field, after `~` expansion.
    #[arg(long)]
    kill_switch: Option<PathBuf>,

    /// Validate the config and exit without running the event loop.
    /// Non-zero exit iff the config cannot be parsed or fails validation.
    #[arg(long)]
    check: bool,

    /// Cadence of the throttle/policy tick, in milliseconds. Defaults to
    /// 1000 ms. Lower values are useful in integration tests where we
    /// want to observe respawn transitions faster.
    #[arg(long, default_value_t = 1_000)]
    tick_ms: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Some(home) = dirs_home() else {
        eprintln!("cosmon-daemon-supervisor: HOME is not set");
        return ExitCode::from(2);
    };

    let config_path = cli
        .config
        .unwrap_or_else(|| home.join(".config").join("cosmon").join("daemons.toml"));

    // Load once up front: we need the declared paths even before running.
    let cfg = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "cosmon-daemon-supervisor: config error ({}): {e}",
                config_path.display()
            );
            return ExitCode::from(2);
        }
    };

    let state_path = cli
        .state
        .unwrap_or_else(|| expand_tilde(&cfg.supervisor.state_file, &home));
    let kill_switch_path = cli
        .kill_switch
        .unwrap_or_else(|| expand_tilde(&cfg.supervisor.kill_switch, &home));

    if cli.check {
        println!(
            "cosmon-daemon-supervisor: config ok ({} daemon{})",
            cfg.daemons.len(),
            if cfg.daemons.len() == 1 { "" } else { "s" }
        );
        return ExitCode::SUCCESS;
    }

    // Build the supervisor (requires tokio runtime for the notify bridge).
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cosmon-daemon-supervisor: tokio runtime failed: {e}");
            return ExitCode::from(3);
        }
    };

    let supervisor =
        match Supervisor::new(config_path.clone(), &state_path, kill_switch_path.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cosmon-daemon-supervisor: init error: {e}");
                return ExitCode::from(4);
            }
        };

    let tick = Duration::from_millis(cli.tick_ms);

    match runtime.block_on(run(supervisor, tick)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cosmon-daemon-supervisor: fatal: {e}");
            ExitCode::from(5)
        }
    }
}

/// Resolve `HOME` without bringing in the `dirs` crate for one call.
/// Mirrors `cosmon-scheduler::main::default_config_path`.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
