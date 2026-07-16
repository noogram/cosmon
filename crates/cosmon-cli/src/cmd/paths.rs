// SPDX-License-Identifier: AGPL-3.0-only

//! `cs paths --writes` — project the write-path taxonomy.
//!
//! This is the **derived view** that replaces the old hand-maintained "P1"
//! list of paths cosmon writes. It reads
//! **nothing** from disk and keeps **no index** on disk: it renders
//! [`cosmon_core::paths::CosmonPathKind::all`], the enum that the writers
//! themselves decode their paths from. A path the code never writes is a dead
//! variant; a path the taxonomy omits has no [`cosmon_core::paths::CosmonPath`]
//! constructor — so this projection cannot fall stale relative to the code.
//!
//! Like `cs reconcile`, it is a pure projection: stateless, idempotent, one
//! decision per invocation, zero coupling to a running fleet. Consumers that
//! used to hand-type the path set (the `.gitignore` stanza, the ADR-030
//! archive manifest, backup scripts) should *generate* from this output
//! instead.

use serde_json::json;

use super::Context;
use cosmon_core::paths::{CosmonPathKind, Persistence};

/// Arguments for the `paths` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Emit the set of paths cosmon **writes** under the state root.
    ///
    /// Currently the only projection mode; the flag is explicit so future
    /// projections (e.g. `--reads`) can be added without changing the default
    /// behaviour. Omitting it is equivalent to passing it.
    #[arg(long)]
    pub writes: bool,
}

/// Human-readable label for a [`Persistence`] class.
fn persistence_label(p: Persistence) -> &'static str {
    match p {
        Persistence::Gitignored => "gitignored",
        Persistence::Lock => "lock",
        Persistence::ArchiveTracked => "archive-tracked",
    }
}

/// Execute the `paths` command.
///
/// With `--json`, emits one NDJSON object per write-path kind (agent-first
/// interface). Without it, emits an aligned human table. Both are pure
/// renders of [`CosmonPathKind::all`].
///
/// # Errors
///
/// Infallible in practice (no I/O); returns `anyhow::Result` for signature
/// uniformity with the other subcommands (the dispatch match in `main.rs`
/// requires every arm to share the `anyhow::Result<()>` return type).
#[allow(clippy::unnecessary_wraps)]
pub fn run(ctx: &Context, _args: &Args) -> anyhow::Result<()> {
    if ctx.json {
        for kind in CosmonPathKind::all() {
            let obj = json!({
                "kind": format!("{kind:?}"),
                "template": kind.template(),
                "owner": kind.owner(),
                "persistence": persistence_label(kind.persistence()),
                "description": kind.description(),
            });
            println!("{obj}");
        }
        return Ok(());
    }

    // Human table: template ▸ persistence ▸ owner / description.
    let width = CosmonPathKind::all()
        .map(|k| k.template().len())
        .max()
        .unwrap_or(0);
    println!("paths cosmon writes (derived from cosmon_core::paths::CosmonPath):");
    for kind in CosmonPathKind::all() {
        println!(
            "  {:<width$}  [{}]  {}",
            kind.template(),
            persistence_label(kind.persistence()),
            kind.description(),
            width = width,
        );
    }
    Ok(())
}
