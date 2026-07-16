// SPDX-License-Identifier: AGPL-3.0-only

//! `cs archive` — read-only inspection + retention of the durable archive
//! (ADR-030 M3 + M4).
//!
//! Terminal transitions (`cs done`, `cs collapse`, `cs freeze`, `cs stuck`)
//! drop a canonical snapshot under `.cosmon/state/archive/YYYY/MM/<id>/`
//! when `[archive] enabled = true` in the project's `config.toml`. This
//! module surfaces the operator's tools over that tree:
//!
//! - `cs archive list [--year YYYY] [--month MM]` — table of archived
//!   molecules. Optional filters narrow the scan to a year or a month.
//! - `cs archive show <id>` — print the molecule's manifest, edges, and
//!   the list of artifacts captured alongside them. Accepts a molecule-id
//!   prefix (unique match).
//! - `cs archive verify <id>` — recompute the SHA-256 of every file listed
//!   in `manifest.json`'s `response_hashes` and compare against the sealed
//!   value. Exits `1` if any file is missing, has been altered, or the
//!   manifest itself is unreadable; exits `0` on a clean match.
//! - `cs archive prune [--dry-run]` — apply the `[archive.retention]`
//!   policy. `--dry-run` prints the plan and exits 0 without touching
//!   disk; without it, the plan executes. Hash-chain integrity is
//!   enforced: parents of kept molecules are never deleted.
//!
//! All subcommands accept the global `--json` flag for scripting.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use cosmon_state::archive::retention;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::Context;

/// Arguments for the `archive` subcommand.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: ArchiveCommand,
}

/// Archive subcommands.
#[derive(clap::Subcommand)]
pub enum ArchiveCommand {
    /// List archived molecules (optionally filtered by year / month)
    List(ListArgs),
    /// Show the manifest + artifact inventory for one archived molecule
    Show(ShowArgs),
    /// Verify artifact hashes — exits non-zero if tampered
    Verify(VerifyArgs),
    /// Apply the [archive.retention] policy; `--dry-run` shows the plan
    Prune(PruneArgs),
}

/// Arguments for `cs archive list`.
#[derive(clap::Args)]
pub struct ListArgs {
    /// Restrict the scan to a single year (e.g. `2026`).
    #[arg(long, value_name = "YYYY")]
    pub year: Option<i32>,
    /// Restrict the scan to a single month (01..=12). Must be used with
    /// `--year` to be meaningful; standalone `--month` still filters
    /// across whichever years contain that month.
    #[arg(long, value_name = "MM")]
    pub month: Option<u32>,
    /// Keep only entries whose directory was modified within the last
    /// `N` days. `0` means no limit. Combines with `--year`/`--month`.
    /// Used by the CI gate (`.github/workflows/archive-verify.yml`).
    #[arg(long, value_name = "N")]
    pub since_days: Option<u64>,
    /// Emit one molecule id per line, skipping the table header / JSON
    /// envelope. Intended for shell pipelines (`xargs cs archive verify`).
    #[arg(long)]
    pub ids_only: bool,
}

/// Arguments for `cs archive show`.
#[derive(clap::Args)]
pub struct ShowArgs {
    /// Molecule id or unique prefix.
    pub molecule: String,
}

/// Arguments for `cs archive verify`.
#[derive(clap::Args)]
pub struct VerifyArgs {
    /// Molecule id or unique prefix.
    pub molecule: String,
}

/// Arguments for `cs archive prune`.
#[derive(clap::Args)]
pub struct PruneArgs {
    /// Show what would be deleted and exit — never touch disk.
    #[arg(long)]
    pub dry_run: bool,
}

/// Dispatch entry point.
///
/// # Errors
///
/// Propagates I/O and lookup errors from the individual subcommands.
/// `cs archive verify` additionally calls `std::process::exit(1)` when
/// the archive has been tampered with.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        ArchiveCommand::List(a) => run_list(ctx, a),
        ArchiveCommand::Show(a) => run_show(ctx, a),
        ArchiveCommand::Verify(a) => run_verify(ctx, a),
        ArchiveCommand::Prune(a) => run_prune(ctx, a),
    }
}

/// A single archived-molecule row, shared by `list` and `show`.
#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
struct Entry {
    year: i32,
    month: u32,
    molecule_id: String,
    path: PathBuf,
    manifest: Option<Manifest>,
}

/// The manifest layout written by `cosmon_state::archive` (M3).
///
/// Kept as a local mirror so this command never imports the writer just
/// to reach the type — the read side should survive changes to the
/// writer's internals as long as the on-disk shape is stable.
#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    schema_version: String,
    formula_pin: String,
    molecule_id: String,
    status: String,
    #[serde(default)]
    response_hashes: BTreeMap<String, String>,
    /// Optional top-level sha-256 of `synthesis.md`. Sealed by the
    /// archive writer (`cosmon-state::archive`) when synthesis exists;
    /// used by `cs archive verify` to detect post-archive edits of the
    /// synthesis file.
    #[serde(default)]
    synthesis_hash: Option<String>,
}

/// Resolve the archive root for the current project.
///
/// Walks up from the current working directory (or the explicit
/// `--config` override) to find `.cosmon/state/`, then appends `archive/`.
fn archive_root(ctx: &Context) -> PathBuf {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    state_dir.join("archive")
}

/// List every archive entry under `root`, honoring optional `year`/`month`
/// filters. Returns entries sorted by (year, month, `molecule_id`) for a
/// deterministic display.
fn scan_entries(root: &Path, year: Option<i32>, month: Option<u32>) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for year_entry in fs::read_dir(root)? {
        let year_entry = year_entry?;
        let year_path = year_entry.path();
        if !year_path.is_dir() {
            continue;
        }
        let year_name = year_entry.file_name();
        let Some(year_str) = year_name.to_str() else {
            continue;
        };
        // Skip the fleet-level `events/` sibling of the YYYY/ dirs —
        // it is not a year.
        let Ok(y) = year_str.parse::<i32>() else {
            continue;
        };
        if let Some(yf) = year {
            if yf != y {
                continue;
            }
        }
        for month_entry in fs::read_dir(&year_path)? {
            let month_entry = month_entry?;
            let month_path = month_entry.path();
            if !month_path.is_dir() {
                continue;
            }
            let Some(month_str) = month_entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(m) = month_str.parse::<u32>() else {
                continue;
            };
            if let Some(mf) = month {
                if mf != m {
                    continue;
                }
            }
            for mol_entry in fs::read_dir(&month_path)? {
                let mol_entry = mol_entry?;
                let mol_path = mol_entry.path();
                if !mol_path.is_dir() {
                    continue;
                }
                let molecule_id = mol_entry.file_name().to_string_lossy().into_owned();
                let manifest = read_manifest(&mol_path.join("manifest.json")).ok();
                out.push(Entry {
                    year: y,
                    month: m,
                    molecule_id,
                    path: mol_path,
                    manifest,
                });
            }
        }
    }
    out.sort_by(|a, b| (a.year, a.month, &a.molecule_id).cmp(&(b.year, b.month, &b.molecule_id)));
    Ok(out)
}

fn read_manifest(path: &Path) -> anyhow::Result<Manifest> {
    let bytes = fs::read(path)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    Ok(manifest)
}

/// Locate a single archive entry by molecule id (or unique prefix).
fn resolve_entry(root: &Path, needle: &str) -> anyhow::Result<Entry> {
    let entries = scan_entries(root, None, None)?;
    let exact: Vec<&Entry> = entries.iter().filter(|e| e.molecule_id == needle).collect();
    if let [hit] = exact.as_slice() {
        return Ok((*hit).clone());
    }
    let prefix: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.molecule_id.starts_with(needle))
        .collect();
    match prefix.as_slice() {
        [hit] => Ok((*hit).clone()),
        [] => anyhow::bail!("no archived molecule matches '{needle}'"),
        many => anyhow::bail!(
            "ambiguous prefix '{needle}' ({} archived matches)",
            many.len()
        ),
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn run_list(ctx: &Context, args: &ListArgs) -> anyhow::Result<()> {
    let root = archive_root(ctx);
    let mut entries = scan_entries(&root, args.year, args.month)?;

    // --since-days filter: drop entries whose directory mtime is older than the cutoff.
    if let Some(days) = args.since_days {
        if days > 0 {
            let cutoff = std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(days * 86_400))
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            entries.retain(|e| {
                fs::metadata(&e.path)
                    .and_then(|m| m.modified())
                    .map(|t| t >= cutoff)
                    .unwrap_or(true)
            });
        }
    }

    // --ids-only: one molecule_id per line, nothing else. CI-friendly.
    if args.ids_only {
        for e in &entries {
            println!("{}", e.molecule_id);
        }
        return Ok(());
    }

    if ctx.json {
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let (formula, status, schema) = e.manifest.as_ref().map_or_else(
                    || (String::new(), String::new(), String::new()),
                    |m| {
                        (
                            m.formula_pin.clone(),
                            m.status.clone(),
                            m.schema_version.clone(),
                        )
                    },
                );
                serde_json::json!({
                    "year": e.year,
                    "month": e.month,
                    "molecule_id": e.molecule_id,
                    "formula": formula,
                    "status": status,
                    "schema_version": schema,
                    "path": e.path.display().to_string(),
                })
            })
            .collect();
        let out = serde_json::json!({
            "archive_root": root.display().to_string(),
            "count": entries.len(),
            "filters": {
                "year": args.year,
                "month": args.month,
            },
            "entries": rows,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if entries.is_empty() {
        let scope = match (args.year, args.month) {
            (Some(y), Some(m)) => format!(" for {y:04}-{m:02}"),
            (Some(y), None) => format!(" for {y:04}"),
            (None, Some(m)) => format!(" for month {m:02}"),
            (None, None) => String::new(),
        };
        println!("(no archived molecules{scope})");
        return Ok(());
    }

    println!(
        "{:<6} {:<5} {:<28} {:<12} {:<20}",
        "YEAR", "MONTH", "MOLECULE", "STATUS", "FORMULA"
    );
    for e in &entries {
        let (status, formula) = e.manifest.as_ref().map_or_else(
            || ("?".to_owned(), "?".to_owned()),
            |m| (m.status.clone(), m.formula_pin.clone()),
        );
        println!(
            "{:<6} {:<5} {:<28} {:<12} {:<20}",
            format!("{:04}", e.year),
            format!("{:02}", e.month),
            e.molecule_id,
            status,
            formula,
        );
    }
    println!("\n{} archived molecule(s).", entries.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

fn run_show(ctx: &Context, args: &ShowArgs) -> anyhow::Result<()> {
    let root = archive_root(ctx);
    let entry = resolve_entry(&root, &args.molecule)?;
    let manifest = entry.manifest.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "manifest.json missing or unreadable in {}",
            entry.path.display()
        )
    })?;

    // Inventory: every non-directory file under the entry dir.
    let files = list_files(&entry.path)?;

    if ctx.json {
        let file_rows: Vec<serde_json::Value> = files
            .iter()
            .map(|(rel, size)| {
                serde_json::json!({
                    "path": rel,
                    "size": size,
                })
            })
            .collect();
        let out = serde_json::json!({
            "molecule_id": manifest.molecule_id,
            "year": entry.year,
            "month": entry.month,
            "entry_dir": entry.path.display().to_string(),
            "manifest": {
                "schema_version": manifest.schema_version,
                "formula_pin": manifest.formula_pin,
                "molecule_id": manifest.molecule_id,
                "status": manifest.status,
                "response_hashes": manifest.response_hashes,
            },
            "artifacts": file_rows,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("Archive entry: {}", entry.path.display());
    println!("Molecule:      {}", manifest.molecule_id);
    println!("Year/Month:    {:04}-{:02}", entry.year, entry.month);
    println!("Status:        {}", manifest.status);
    println!("Formula:       {}", manifest.formula_pin);
    println!("Schema:        v{}", manifest.schema_version);

    if manifest.response_hashes.is_empty() {
        println!("\nResponse hashes: (none)");
    } else {
        println!("\nResponse hashes ({}):", manifest.response_hashes.len());
        for (name, hash) in &manifest.response_hashes {
            println!("  responses/{name:<24} sha256:{hash}");
        }
    }

    println!("\nArtifacts ({}):", files.len());
    for (rel, size) in &files {
        println!("  {rel:<36} {size:>10} bytes");
    }
    Ok(())
}

/// Recursively list every file under `root`, sorted for determinism.
/// Returns tuples of `(relative/path, size_bytes)`.
fn list_files(root: &Path) -> std::io::Result<Vec<(String, u64)>> {
    let mut out = Vec::new();
    walk_files(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk_files(root: &Path, cur: &Path, out: &mut Vec<(String, u64)>) -> std::io::Result<()> {
    for entry in fs::read_dir(cur)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_files(root, &path, out)?;
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).map_or_else(
                |_| path.display().to_string(),
                |p| p.to_string_lossy().replace('\\', "/"),
            );
            let size = entry.metadata()?.len();
            out.push((rel, size));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn run_verify(ctx: &Context, args: &VerifyArgs) -> anyhow::Result<()> {
    let root = archive_root(ctx);
    let entry = resolve_entry(&root, &args.molecule)?;
    let manifest = entry.manifest.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "manifest.json missing or unreadable in {}",
            entry.path.display()
        )
    })?;

    let responses_dir = entry.path.join("responses");
    let mut checks: Vec<(String, Status, String)> = Vec::new();

    // Synthesis seal — top-level synthesis.md, hashed independently by
    // the archive writer. Checked before responses so the CI diff shows
    // it first when both drift.
    if let Some(sealed) = manifest.synthesis_hash.as_deref() {
        let path = entry.path.join("synthesis.md");
        match fs::read(&path) {
            Ok(bytes) => {
                let got = sha256_hex(&bytes);
                if got == sealed {
                    checks.push((
                        "synthesis.md".to_owned(),
                        Status::Pass,
                        format!("sha256 {}", short(&got)),
                    ));
                } else {
                    checks.push((
                        "synthesis.md".to_owned(),
                        Status::Fail,
                        format!("sealed {}, current {}", short(sealed), short(&got)),
                    ));
                }
            }
            Err(e) => checks.push((
                "synthesis.md".to_owned(),
                Status::Fail,
                format!("archived file missing or unreadable: {e}"),
            )),
        }
    }

    for (name, sealed_hash) in &manifest.response_hashes {
        let path = responses_dir.join(name);
        match fs::read(&path) {
            Ok(bytes) => {
                let got = sha256_hex(&bytes);
                if &got == sealed_hash {
                    checks.push((
                        name.clone(),
                        Status::Pass,
                        format!("sha256 {}", short(&got)),
                    ));
                } else {
                    checks.push((
                        name.clone(),
                        Status::Fail,
                        format!("sealed {}, current {}", short(sealed_hash), short(&got)),
                    ));
                }
            }
            Err(e) => checks.push((
                name.clone(),
                Status::Fail,
                format!("archived file missing or unreadable: {e}"),
            )),
        }
    }

    // Files that live on disk but are not covered by the manifest are
    // reported informationally (never as a failure) so verify stays
    // strict about sealed hashes without flagging legitimate helpers
    // that the archive writer did not hash.
    if responses_dir.is_dir() {
        for ent in fs::read_dir(&responses_dir)?.flatten() {
            if ent.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let name = ent.file_name().to_string_lossy().into_owned();
                if !manifest.response_hashes.contains_key(&name) {
                    checks.push((
                        name,
                        Status::Info,
                        "present on disk, not in manifest".to_owned(),
                    ));
                }
            }
        }
    }

    let any_fail = checks.iter().any(|(_, s, _)| *s == Status::Fail);

    if ctx.json {
        let rows: Vec<serde_json::Value> = checks
            .iter()
            .map(|(name, status, detail)| {
                serde_json::json!({
                    "name": name,
                    "status": status.label(),
                    "detail": detail,
                })
            })
            .collect();
        let out = serde_json::json!({
            "molecule_id": manifest.molecule_id,
            "entry_dir": entry.path.display().to_string(),
            "status": if any_fail { "fail" } else { "pass" },
            "checks": rows,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        if checks.is_empty() {
            println!(
                "verify: no hashes recorded for {} (nothing to check)",
                manifest.molecule_id
            );
        }
        for (name, status, detail) in &checks {
            println!("[{}] responses/{name}: {detail}", status.label());
        }
        println!();
        if any_fail {
            println!("verify: FAIL ({})", manifest.molecule_id);
        } else {
            println!("verify: PASS ({})", manifest.molecule_id);
        }
    }

    if any_fail {
        std::process::exit(1);
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---------------------------------------------------------------------------
// prune
// ---------------------------------------------------------------------------

fn run_prune(ctx: &Context, args: &PruneArgs) -> anyhow::Result<()> {
    let root = archive_root(ctx);
    // Load project config to pick up [archive.retention]. A missing
    // config is not fatal — a fresh project with no config.toml has
    // nothing to prune and the dry-run prints an empty plan.
    let config_path = super::resolve_config_from_context(ctx);
    let cfg = cosmon_filestore::load_project_config(&config_path)
        .unwrap_or_else(|_| cosmon_core::config::ProjectConfig::default());
    let policy = &cfg.archive.retention;

    let entries = retention::scan_entries(&root)
        .map_err(|e| anyhow::anyhow!("scan archive at {}: {e}", root.display()))?;
    let plan = retention::plan(&entries, policy, Utc::now());

    if ctx.json {
        return emit_prune_json(&root, args, policy, &plan);
    }
    emit_prune_text(&root, args, policy, &plan);
    Ok(())
}

fn emit_prune_json(
    root: &Path,
    args: &PruneArgs,
    policy: &cosmon_core::config::RetentionConfig,
    plan: &retention::Plan,
) -> anyhow::Result<()> {
    let rows: Vec<serde_json::Value> = plan
        .rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "molecule_id": r.entry.molecule_id,
                "year": r.entry.year,
                "month": r.entry.month,
                "size_bytes": r.entry.size_bytes,
                "archived_at": r.entry.archived_at.to_rfc3339(),
                "kind": r.entry.kind.map(|k| k.to_string()),
                "fate": fate_tag(r.fate),
                "reason": r.reason,
                "path": r.entry.path.display().to_string(),
            })
        })
        .collect();
    let executed = !args.dry_run && plan.deletions > 0;
    let out = serde_json::json!({
        "archive_root": root.display().to_string(),
        "dry_run": args.dry_run,
        "executed": executed,
        "policy": {
            "keep_all": policy.keep_all,
            "max_age_days": policy.max_age_days,
            "max_total_mb": policy.max_total_mb,
            "keep_kinds": policy.keep_kinds,
        },
        "plan": {
            "scanned": plan.rows.len(),
            "kept": plan.kept,
            "promoted": plan.promoted,
            "deletions": plan.deletions,
            "bytes_before": plan.bytes_before,
            "bytes_after": plan.bytes_after,
        },
        "rows": rows,
    });
    if executed {
        retention::execute(plan, |id, e| {
            eprintln!("prune: failed to delete {id}: {e}");
        });
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn emit_prune_text(
    root: &Path,
    args: &PruneArgs,
    policy: &cosmon_core::config::RetentionConfig,
    plan: &retention::Plan,
) {
    if plan.rows.is_empty() {
        println!("(no archived molecules to prune)");
        return;
    }

    println!("Archive root: {}", root.display());
    println!(
        "Policy: keep_all={} max_age_days={} max_total_mb={} keep_kinds={:?}",
        policy.keep_all, policy.max_age_days, policy.max_total_mb, policy.keep_kinds
    );
    println!(
        "Scanned {} entries — {} kept, {} promoted (integrity), {} to delete.",
        plan.rows.len(),
        plan.kept - plan.promoted,
        plan.promoted,
        plan.deletions,
    );
    println!(
        "Size: {} → {} ({} reclaimable)",
        format_bytes(plan.bytes_before),
        format_bytes(plan.bytes_after),
        format_bytes(plan.bytes_before.saturating_sub(plan.bytes_after)),
    );
    println!();
    for row in &plan.rows {
        let tag = match row.fate {
            retention::Fate::KeptByPolicy => "KEEP",
            retention::Fate::KeptByIntegrity => "KEEP*",
            retention::Fate::Delete => "DEL ",
        };
        println!(
            "  [{tag}] {:<28} {:>9} {} — {}",
            row.entry.molecule_id,
            format_bytes(row.entry.size_bytes),
            row.entry.archived_at.format("%Y-%m-%d"),
            row.reason,
        );
    }
    println!();

    if args.dry_run {
        println!("(dry-run) no files were deleted. Re-run without --dry-run to execute.");
        return;
    }
    if plan.deletions == 0 {
        println!("Nothing to delete — policy kept every entry.");
        return;
    }

    let deleted = retention::execute(plan, |id, e| {
        eprintln!("prune: failed to delete {id}: {e}");
    });
    println!("Deleted {} entries.", deleted.len());
}

fn fate_tag(fate: retention::Fate) -> &'static str {
    match fate {
        retention::Fate::KeptByPolicy => "kept_by_policy",
        retention::Fate::KeptByIntegrity => "kept_by_integrity",
        retention::Fate::Delete => "delete",
    }
}

fn format_bytes(b: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    // Display-only rounding; archive totals never approach 2^53 bytes.
    #[allow(clippy::cast_precision_loss)]
    if b >= MIB {
        format!("{:.1} MiB", b as f64 / MIB as f64)
    } else if b >= KIB {
        format!("{:.1} KiB", b as f64 / KIB as f64)
    } else {
        format!("{b} B")
    }
}

fn short(hex: &str) -> String {
    hex.chars().take(12).collect::<String>() + "…"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Info,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Info => "INFO",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(entry_dir: &Path, hashes: &[(&str, &str)]) {
        let mut response_hashes = serde_json::Map::new();
        for (name, hash) in hashes {
            response_hashes.insert(
                (*name).to_owned(),
                serde_json::Value::String((*hash).to_owned()),
            );
        }
        let json = serde_json::json!({
            "schema_version": "1",
            "formula_pin": "task-work",
            "molecule_id": entry_dir.file_name().unwrap().to_string_lossy(),
            "status": "collapsed",
            "response_hashes": serde_json::Value::Object(response_hashes),
        });
        fs::write(
            entry_dir.join("manifest.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();
    }

    fn make_entry(root: &Path, year: &str, month: &str, mol: &str, responses: &[(&str, &[u8])]) {
        let dir = root.join(year).join(month).join(mol);
        fs::create_dir_all(dir.join("responses")).unwrap();
        let mut hashes = Vec::new();
        for (name, bytes) in responses {
            fs::write(dir.join("responses").join(name), bytes).unwrap();
            hashes.push((*name, sha256_hex(bytes)));
        }
        let pairs: Vec<(&str, &str)> = hashes.iter().map(|(n, h)| (*n, h.as_str())).collect();
        write_manifest(&dir, &pairs);
    }

    #[test]
    fn list_is_empty_when_no_archive_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        let entries = scan_entries(&root, None, None).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_surfaces_two_entries_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        make_entry(&root, "2026", "04", "task-b", &[("knuth.md", b"hello")]);
        make_entry(&root, "2026", "04", "task-a", &[]);

        let all = scan_entries(&root, None, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].molecule_id, "task-a");
        assert_eq!(all[1].molecule_id, "task-b");
    }

    #[test]
    fn list_year_filter_narrows_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        make_entry(&root, "2025", "12", "task-old", &[]);
        make_entry(&root, "2026", "04", "task-new", &[]);

        let only_2026 = scan_entries(&root, Some(2026), None).unwrap();
        assert_eq!(only_2026.len(), 1);
        assert_eq!(only_2026[0].molecule_id, "task-new");
    }

    #[test]
    fn verify_passes_on_unchanged_responses() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        make_entry(
            &root,
            "2026",
            "04",
            "task-ok",
            &[("torvalds.md", b"response body")],
        );
        let entry = resolve_entry(&root, "task-ok").unwrap();
        let manifest = entry.manifest.clone().unwrap();
        let dir = entry.path.clone();
        let bytes = fs::read(dir.join("responses/torvalds.md")).unwrap();
        let got = sha256_hex(&bytes);
        assert_eq!(got, manifest.response_hashes["torvalds.md"]);
    }

    #[test]
    fn verify_detects_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        make_entry(&root, "2026", "04", "task-tamp", &[("a.md", b"original")]);
        let dir = root.join("2026/04/task-tamp");
        fs::write(dir.join("responses/a.md"), b"TAMPERED").unwrap();
        let entry = resolve_entry(&root, "task-tamp").unwrap();
        let manifest = entry.manifest.clone().unwrap();
        let bytes = fs::read(dir.join("responses/a.md")).unwrap();
        let got = sha256_hex(&bytes);
        assert_ne!(got, manifest.response_hashes["a.md"]);
    }

    #[test]
    fn resolve_entry_matches_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        make_entry(&root, "2026", "04", "task-20260419-abcd", &[]);
        let e = resolve_entry(&root, "task-20260419").unwrap();
        assert_eq!(e.molecule_id, "task-20260419-abcd");
    }

    #[test]
    fn resolve_entry_rejects_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        make_entry(&root, "2026", "04", "task-20260419-aaaa", &[]);
        make_entry(&root, "2026", "04", "task-20260419-bbbb", &[]);
        let err = resolve_entry(&root, "task-20260419").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn list_files_walks_responses_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        make_entry(
            &root,
            "2026",
            "04",
            "task-walk",
            &[("a.md", b"x"), ("b.md", b"y")],
        );
        let dir = root.join("2026/04/task-walk");
        let files = list_files(&dir).unwrap();
        let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert!(names.contains(&"manifest.json"));
        assert!(names.contains(&"responses/a.md"));
        assert!(names.contains(&"responses/b.md"));
    }
}
