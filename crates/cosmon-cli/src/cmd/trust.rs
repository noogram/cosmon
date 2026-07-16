// SPDX-License-Identifier: AGPL-3.0-only

//! `cs trust` — grant, inspect, or revoke this repository's permission to run
//! its own repo-supplied shell strings (B5, RCE-by-clone).
//!
//! This is cosmon's `direnv allow`. Cosmon runs shell commands a repository
//! supplies — a formula's `command`/`verification` steps and the
//! `post_merge`/`pre_done` hooks in `.cosmon/config.toml`. Until the operator
//! vouches for a repository once, cosmon refuses to run any of them (see
//! [`cosmon_cli::trust`]). This command is that one-time human gesture.
//!
//! - `cs trust` — grant trust for the current repository.
//! - `cs trust --status` — report the current status without changing it.
//! - `cs trust --revoke` — remove the grant.
//!
//! The grant is recorded in a global store (`~/.cosmon/trust/`), keyed on the
//! repository's absolute path — never inside `.cosmon/`, so a clone cannot
//! ship its own grant.

use std::path::PathBuf;

use anyhow::Result;
use cosmon_cli::trust::{self, TrustStatus};
use serde_json::json;

use super::Context;

/// Arguments for `cs trust`.
#[derive(clap::Args)]
pub struct Args {
    /// Report the current trust status without changing anything.
    #[arg(long, conflicts_with = "revoke")]
    pub status: bool,

    /// Revoke this repository's trust grant.
    #[arg(long)]
    pub revoke: bool,

    /// Operate on this directory instead of the current working directory.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

/// Execute `cs trust`.
pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    let start = match &args.dir {
        Some(d) => d.clone(),
        None => std::env::current_dir()?,
    };
    let store = trust::store_dir();

    if args.status {
        let st = trust::evaluate(&start, &store);
        let root = trust::repo_key_root(&start).unwrap_or_else(|| start.clone());
        emit(ctx, &root, status_word(&st), &status_human(&st, &root));
        return Ok(());
    }

    if args.revoke {
        let (root, removed) = trust::revoke(&start, &store)?;
        let msg = if removed {
            format!("✓ revoked trust for {}", root.display())
        } else {
            format!("· no trust grant to revoke for {}", root.display())
        };
        emit(ctx, &root, if removed { "revoked" } else { "absent" }, &msg);
        return Ok(());
    }

    // Default action: grant.
    let root = trust::grant(&start, &store)?;
    emit(
        ctx,
        &root,
        "trusted",
        &format!(
            "✓ trusted {} — cosmon may now run this repository's formulas and hooks",
            root.display()
        ),
    );
    Ok(())
}

/// Machine word for a [`TrustStatus`].
fn status_word(st: &TrustStatus) -> &'static str {
    match st {
        TrustStatus::Trusted => "trusted",
        TrustStatus::Untrusted => "untrusted",
        TrustStatus::Stale => "stale",
    }
}

/// Human sentence for a [`TrustStatus`].
fn status_human(st: &TrustStatus, root: &std::path::Path) -> String {
    match st {
        TrustStatus::Trusted => format!("✓ trusted: {}", root.display()),
        TrustStatus::Untrusted => {
            format!("✗ untrusted: {} — run `cs trust` to grant", root.display())
        }
        TrustStatus::Stale => format!(
            "⚠ stale: {} — shell surface changed since granted; run `cs trust` to re-grant",
            root.display()
        ),
    }
}

/// Emit either an NDJSON object (`--json`) or the human line.
fn emit(ctx: &Context, root: &std::path::Path, status: &str, human: &str) {
    if ctx.json {
        println!(
            "{}",
            json!({ "repository": root.display().to_string(), "status": status })
        );
    } else {
        println!("{human}");
    }
}
