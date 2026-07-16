// SPDX-License-Identifier: AGPL-3.0-only

//! `cs inspect` — classify a single path into its genre (ADR-057).
//!
//! Reads `.cosmon/artifact-map.toml`, matches the path against the
//! declared globs, and reports the genre, audience, derived residence,
//! and computed `rot` (days since last commit). When the TOML file is
//! absent, falls back to [`ArtifactMap::default_code_catchall`] — every
//! path classifies as `code`, so the command never fails for lack of
//! configuration.
//!
//! This is a **read-only** operator tool. It does not move files, edit
//! git config, or mutate anything on disk. v1 may layer an enforcement
//! verb on top (`cs migrate --genre`); v0 declares and reports.
//!
//! # Output
//!
//! The default output is a four-line block:
//!
//! ```text
//! path:      docs/lore/2026-04-20-x.md
//! genre:     chronicle
//! audience:  author+agent
//! residence: team
//! rot:       2d
//! ```
//!
//! With `--verbose`, the matched glob and the residence derivation are
//! also shown. With `--json`, the same fields emit as a single NDJSON
//! line.

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::artifact_map::{ArtifactMap, AuditReport};

use super::Context;

/// Arguments for `cs inspect`.
#[derive(clap::Args)]
pub struct Args {
    /// Path to classify (relative to the galaxy root, or absolute).
    pub path: PathBuf,

    /// Explain the matched glob and residence derivation.
    #[arg(long, short)]
    pub verbose: bool,
}

/// Execute the `inspect` command.
///
/// # Errors
///
/// Returns an error if the artifact-map TOML fails to parse (a corrupt
/// file is a configuration bug worth surfacing). Returns `Ok(())` with
/// an `unclassified` report if no glob matches — this should be
/// unreachable once the `code` catch-all is in place, but the command
/// degrades gracefully rather than panicking.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let map = load_map()?;
    let rel = relativise(&args.path);
    let class = map.classify(&rel);
    let rot = compute_rot_days(&args.path);

    match class {
        Some(c) => {
            if ctx.json {
                let out = serde_json::json!({
                    "path": rel.to_string_lossy(),
                    "genre": c.genre,
                    "matched_glob": c.matched_glob,
                    "audience": c.audience.as_display(),
                    "residence": match c.residence {
                        cosmon_core::artifact_map::Residence::Solo => "solo",
                        cosmon_core::artifact_map::Residence::Team => "team",
                    },
                    "rot_days": rot,
                });
                println!("{out}");
            } else {
                println!("path:      {}", rel.display());
                println!("genre:     {}", c.genre);
                println!("audience:  {}", c.audience.as_display());
                println!(
                    "residence: {}",
                    match c.residence {
                        cosmon_core::artifact_map::Residence::Solo => "solo",
                        cosmon_core::artifact_map::Residence::Team => "team",
                    }
                );
                println!("rot:       {}", format_rot(rot));
                if args.verbose || ctx.verbose {
                    println!("matched:   {}", c.matched_glob);
                    println!("(residence derived from audience: see ADR-057)");
                }
            }
        }
        None => {
            if ctx.json {
                let out = serde_json::json!({
                    "path": rel.to_string_lossy(),
                    "genre": "unclassified",
                    "hint": "add a matching entry to .cosmon/artifact-map.toml or fall back via [code] catch-all",
                });
                println!("{out}");
            } else {
                println!("path:      {}", rel.display());
                println!("genre:     unclassified");
                println!(
                    "hint:      add a matching glob to .cosmon/artifact-map.toml \
                     (or rely on the `code` catch-all)"
                );
            }
        }
    }
    Ok(())
}

/// Load the artifact-map from the nearest `.cosmon/artifact-map.toml`,
/// or the code-only default.
///
/// # Errors
///
/// Returns the TOML parse error verbatim when the file exists but is
/// malformed.
pub(crate) fn load_map() -> anyhow::Result<ArtifactMap> {
    let toml_path = find_artifact_map();
    match toml_path {
        Some(p) => {
            let raw = std::fs::read_to_string(&p)
                .map_err(|e| anyhow::anyhow!("could not read {}: {e}", p.display()))?;
            ArtifactMap::parse_toml(&raw).map_err(|e| anyhow::anyhow!("{e}"))
        }
        None => Ok(ArtifactMap::default_code_catchall()),
    }
}

/// Walk up from CWD looking for `.cosmon/artifact-map.toml`.
fn find_artifact_map() -> Option<PathBuf> {
    let mut cur: PathBuf = std::env::current_dir().ok()?;
    loop {
        let candidate = cur.join(".cosmon").join("artifact-map.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Compute rot in days from `git log -n 1 --format=%ct <path>`.
///
/// Returns `None` when the path is not in a git repo, has never been
/// committed, or `git` is unavailable.
pub(crate) fn compute_rot_days(path: &Path) -> Option<u64> {
    let output = Command::new("git")
        .arg("log")
        .arg("-n")
        .arg("1")
        .arg("--format=%ct")
        .arg("--")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    let ts: u64 = s.trim().parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now.saturating_sub(ts) / 86_400)
}

fn format_rot(rot: Option<u64>) -> String {
    match rot {
        Some(d) if d < 1 => "today".to_owned(),
        Some(d) => format!("{d}d"),
        None => "unknown".to_owned(),
    }
}

fn relativise(path: &Path) -> PathBuf {
    if path.is_absolute() {
        let cwd = std::env::current_dir().unwrap_or_default();
        path.strip_prefix(&cwd)
            .map_or_else(|_| path.to_path_buf(), std::path::Path::to_path_buf)
    } else {
        path.to_path_buf()
    }
}

/// Render an [`AuditReport`] on stdout. Used by `cs artifacts audit`.
pub(crate) fn render_audit(ctx: &Context, report: &AuditReport) {
    if ctx.json {
        // Emit NDJSON: one line per genre count, one line per unclassified.
        for (g, n) in &report.per_genre {
            let line = serde_json::json!({
                "kind": "genre-count",
                "genre": g,
                "count": n,
            });
            println!("{line}");
        }
        for p in &report.unclassified {
            let line = serde_json::json!({
                "kind": "unclassified",
                "path": p,
            });
            println!("{line}");
        }
        let summary = serde_json::json!({
            "kind": "summary",
            "total": report.total,
            "unclassified_count": report.unclassified.len(),
            "invariants_hold": report.invariants_hold(),
        });
        println!("{summary}");
    } else {
        println!("artifact map audit — {} tracked paths", report.total);
        println!();
        for (g, n) in &report.per_genre {
            println!("  {g:20}  {n:>6}");
        }
        if !report.unclassified.is_empty() {
            println!();
            println!("UNCLASSIFIED ({}):", report.unclassified.len());
            for p in &report.unclassified {
                println!("  {p}");
            }
            println!();
            println!("(I1 totality violated — add a matching glob to .cosmon/artifact-map.toml)");
        }
        println!();
        println!(
            "invariants: {}",
            if report.invariants_hold() {
                "OK"
            } else {
                "VIOLATED"
            }
        );
    }
}
