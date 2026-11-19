// SPDX-License-Identifier: AGPL-3.0-only

//! `cs gates slot` — run a command while holding a gate slot (ADR-190 D1,
//! noogram/cosmon#214).
//!
//! The project config names the pools and their sizes (`[gate_slots]`); this
//! verb waits for a free slot of one pool, runs the command with the caller's
//! stdio, releases the slot when the command ends, and exits with the command's
//! own exit code. `scripts/run-gates-bg.sh` goes through it for the full gate
//! bundle, and a project's own heavy command can do the same under a pool name
//! of its choosing.
//!
//! The slot is a kernel `flock` held by this process for the life of the
//! command, so a `kill -9` of this process frees it. The lock file descriptor is
//! close-on-exec: the command never inherits the slot, so an orphaned command
//! does not pin it.
//!
//! # Lock order
//!
//! `slot → trunk → fleet`. This verb takes a slot and nothing else; it never
//! takes the trunk lock, and `cs done` never calls it, so a landing is not
//! queued behind a gate.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _, Result};
use cosmon_core::config::FULL_GATE_POOL;
use cosmon_filestore::gate_slots::{acquire_gate_slot_in, try_acquire_gate_slot_in};

use super::Context;

/// Arguments for `cs gates`.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    sub: Sub,
}

#[derive(clap::Subcommand)]
enum Sub {
    /// Run a command while holding a slot of a pool, waiting for one to be free
    Slot(SlotArgs),
}

#[derive(clap::Args)]
struct SlotArgs {
    /// Pool to take a slot from (sized by `[gate_slots]` in `.cosmon/config.toml`)
    #[arg(long, default_value = FULL_GATE_POOL, value_name = "NAME")]
    pool: String,

    /// Print the configured slot count of the pool and exit 0, or exit 3 when
    /// the pool is unbounded; runs no command
    #[arg(long)]
    configured: bool,

    /// Create this file once the slot is held, just before the command starts
    #[arg(long, value_name = "PATH")]
    ready_file: Option<PathBuf>,

    /// The command to run, after `--`
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

/// Execute `cs gates`.
pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    match &args.sub {
        Sub::Slot(a) => slot(ctx, a),
    }
}

fn slot(ctx: &Context, a: &SlotArgs) -> Result<()> {
    let config = cosmon_filestore::load_project_config(&ctx.config_path()).unwrap_or_default();
    let count = config.gate_slots.count_for(&a.pool);

    if a.configured {
        return match count {
            Some(n) => {
                println!("{n}");
                Ok(())
            }
            None => std::process::exit(3),
        };
    }
    let Some((program, rest)) = a.command.split_first() else {
        bail!("no command given: usage `cs gates slot [--pool NAME] -- <command…>`");
    };

    // Held for the whole command; dropped (released) before this process exits.
    let guard = match count {
        // Unbounded pool: today's behaviour, no slot.
        None => None,
        Some(n) => Some(wait_for_slot(&ctx.state_dir(), &a.pool, n)?),
    };
    if let Some(path) = &a.ready_file {
        std::fs::write(path, std::process::id().to_string())
            .with_context(|| format!("cannot write ready file {}", path.display()))?;
    }
    let status = Command::new(program)
        .args(rest)
        .status()
        .with_context(|| format!("cannot run `{program}`"))?;
    drop(guard);
    std::process::exit(exit_code(status));
}

/// Take a slot, saying once on stderr that this run is queued.
fn wait_for_slot(
    state_dir: &Path,
    pool: &str,
    count: usize,
) -> Result<cosmon_filestore::gate_slots::GateSlotGuard> {
    if let Some(guard) = try_acquire_gate_slot_in(state_dir, pool, count)? {
        return Ok(guard);
    }
    eprintln!("cs gates slot: queued for a slot of pool `{pool}` ({count} slots, all held)");
    let guard = acquire_gate_slot_in(state_dir, pool, count)?;
    eprintln!(
        "cs gates slot: slot {} of pool `{pool}` acquired",
        guard.index()
    );
    Ok(guard)
}

/// The command's exit code; `128 + signal` for a command killed by a signal.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    status.code().unwrap_or(1)
}
