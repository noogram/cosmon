// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor` — diagnostic and security probes.
//!
//! The `doctor` verb groups read-only probes that cosmon runs against
//! its own environment. The sprint 1 security probes are the first batch:
//!
//! - `cs doctor leaks` — scan git-tracked files for accidentally
//!   committed secrets, non-public state, dotenv files, private keys.
//!   **Exits non-zero on any finding** — this is the only blocking probe.
//! - `cs doctor worktrees` — audit `.worktrees/` for world-writable
//!   directories, outward symlinks, untracked files.
//! - `cs doctor mcp` — query the neurion registry for registered MCP
//!   servers and flag missing binaries, inline tokens, lax config perms.
//! - `cs doctor deps` — walk workspace Cargo.toml files and flag
//!   unpinned / wildcard / mutable git dependencies.
//! - `cs doctor supervision` — cross-reference the cosmon supervision
//!   roster (`patrols.toml` + `daemons.toml`) against installed macOS
//!   `LaunchAgents` and flag any binary supervised twice (the
//!   retired-but-resurrected plist, cf. mailroom-sync 2026-06-20).
//! - `cs doctor security` — umbrella: run every security probe.
//! - `cs doctor whisper <mol>` — preexisting whisper-channel scaffold.
//!
//! All probes share a single `ProbeReport` / `Finding` structure
//! (see [`findings`]). The umbrella command aggregates reports and
//! exits non-zero iff any probe produced a `Severity::Error` finding.

use std::path::PathBuf;

use super::Context;

pub mod deps;
pub mod findings;
pub mod leaks;
pub mod mcp_audit;
pub mod supervision;
pub mod whisper;
pub mod worktrees;

pub use findings::ProbeReport;

/// Arguments for the `doctor` subcommand.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: DoctorCommand,
}

/// `cs doctor` subcommands.
#[derive(clap::Subcommand)]
pub enum DoctorCommand {
    /// Probe the whisper channel of a molecule's assigned worker.
    Whisper(whisper::WhisperArgs),
    /// Scan tracked files for leaked secrets and non-public state (blocking).
    Leaks(leaks::Args),
    /// Audit `.worktrees/` for perm/symlink/untracked hazards.
    Worktrees(worktrees::Args),
    /// Audit MCP servers registered in the configured service registry.
    Mcp(mcp_audit::Args),
    /// Flag unpinned or mutable dependency declarations.
    Deps(deps::Args),
    /// Detect binaries supervised by both cosmon and a `LaunchAgent`.
    Supervision(supervision::Args),
    /// Run every security probe and aggregate findings.
    Security(SecurityArgs),
}

/// Arguments for `cs doctor security` (umbrella).
#[derive(clap::Args, Default)]
pub struct SecurityArgs {
    /// Override the workspace/git root.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Override the path to the service registry database.
    #[arg(long)]
    pub registry: Option<PathBuf>,
    /// Also include untracked files in the leak scan.
    #[arg(long)]
    pub include_untracked: bool,
}

/// Execute the `doctor` command.
///
/// # Errors
/// Propagates errors from subcommand handlers.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        DoctorCommand::Whisper(a) => whisper::run(ctx, a),
        DoctorCommand::Leaks(a) => leaks::run(ctx, a),
        DoctorCommand::Worktrees(a) => worktrees::run(ctx, a),
        DoctorCommand::Mcp(a) => mcp_audit::run(ctx, a),
        DoctorCommand::Deps(a) => deps::run(ctx, a),
        DoctorCommand::Supervision(a) => supervision::run(ctx, a),
        DoctorCommand::Security(a) => run_security(ctx, a),
    }
}

fn run_security(ctx: &Context, args: &SecurityArgs) -> anyhow::Result<()> {
    let root = match &args.root {
        Some(p) => p.clone(),
        None => leaks::git_root(&std::env::current_dir()?)?,
    };

    let leaks_args = leaks::Args {
        path: None,
        include_untracked: args.include_untracked,
        corpus: None,
    };

    let reports = vec![
        leaks::scan(&root, &leaks_args)?,
        worktrees::scan(&root)?,
        mcp_audit::scan(args.registry.as_deref())?,
        deps::scan(&root)?,
        supervision::scan(&supervision::Args::default())?,
    ];

    emit_report_and_exit(ctx, &reports)
}

/// Print one or more reports and exit with code 1 iff any have errors.
///
/// Honours `ctx.json` — JSON mode prints a single newline-terminated
/// array object; text mode prints a human-readable summary.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn emit_report_and_exit(ctx: &Context, reports: &[ProbeReport]) -> anyhow::Result<()> {
    let any_errors = if ctx.json {
        let mut any = false;
        let payload = serde_json::json!({
            "command": "doctor",
            "reports": reports,
        });
        println!("{payload}");
        for r in reports {
            if r.has_errors() {
                any = true;
            }
        }
        any
    } else {
        findings::render_text(reports)
    };

    if any_errors {
        std::process::exit(1);
    }
    Ok(())
}
