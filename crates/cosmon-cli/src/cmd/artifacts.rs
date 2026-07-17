// SPDX-License-Identifier: AGPL-3.0-only

//! `cs artifacts` — read-only operator view over the artifact map (ADR-057).
//!
//! Today this binds one subcommand: `audit`. Walks `git ls-files` from
//! the galaxy root, classifies every tracked path through
//! `ArtifactMap`, and reports per-genre counts plus any paths that
//! fail to classify.
//!
//! Exit codes:
//! - `0` — invariants hold (every tracked path classifies).
//! - `1` — I1 totality violation (one or more paths unclassified).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::inspect::{load_map, render_audit};
use super::Context;

/// Arguments for `cs artifacts`.
#[derive(clap::Args)]
pub struct Args {
    /// Subcommand under `cs artifacts`.
    #[command(subcommand)]
    pub sub: Sub,
}

/// `cs artifacts <subcommand>`.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// Audit — walk `git ls-files` and report per-genre counts +
    /// unclassified paths.
    Audit(AuditArgs),
}

/// Arguments for `cs artifacts audit`.
#[derive(clap::Args)]
pub struct AuditArgs {}

/// Execute `cs artifacts <sub>`.
///
/// # Errors
///
/// Returns an error if `.cosmon/artifact-map.toml` exists but fails to
/// parse, or if `git ls-files` returns a non-zero status (not a git
/// repo).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.sub {
        Sub::Audit(audit) => run_audit(ctx, audit),
    }
}

fn run_audit(ctx: &Context, _args: &AuditArgs) -> anyhow::Result<()> {
    let map = load_map()?;
    let root = find_git_root().ok_or_else(|| anyhow::anyhow!("not inside a git repository"))?;
    let files = git_ls_files(&root)?;
    let paths: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let report = map.audit(&paths);

    render_audit(ctx, &report);

    if !report.invariants_hold() {
        std::process::exit(1);
    }
    Ok(())
}

fn find_git_root() -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    Some(PathBuf::from(s.trim()))
}

fn git_ls_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("ls-files")
        .current_dir(root)
        .output()
        .map_err(|e| anyhow::anyhow!("git ls-files failed: {e}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-files exit {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}
