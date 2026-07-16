// SPDX-License-Identifier: AGPL-3.0-only

//! `cs galaxies` — inspect the four families of galaxies.
//!
//! Reads the canonical four-family taxonomy directly from the neurion
//! registry DB (`repos.galaxy_kind`). The pilot view groups
//! every repo by kind and reports the `nascent` count so operators can
//! decide what to classify next.
//!
//! The command is read-only: classification is applied by neurion's
//! discovery pass and (later) by the drift-detection formula.
//!
//! `cs galaxies registry` exposes the stateless galaxy-name index
//! (see `cosmon-registry`) — the name→path lookup that the `cs ask`
//! conversational ingress leans on (ADR-070).

use std::collections::BTreeMap;

use colored::Colorize;
use cosmon_registry::{GalaxyIndex, TomlGalaxyIndex};
use neurion_core::GalaxyKind;
use rusqlite::Connection;

use super::Context;

/// Arguments for the `galaxies` subcommand (`cs galaxies list`).
#[derive(clap::Args)]
pub struct Args {
    /// Subcommand — currently only `list` is supported.
    #[command(subcommand)]
    pub cmd: Sub,
}

/// `cs galaxies` subcommands.
#[derive(clap::Subcommand)]
pub enum Sub {
    /// List every galaxy grouped by its `galaxy_kind` family.
    #[command(alias = "ls")]
    List,
    /// Inspect the stateless galaxy-name registry
    /// (`~/.config/cosmon/galaxies.toml`) used by `cs ask`.
    Registry {
        /// Registry-scoped subcommand.
        #[command(subcommand)]
        cmd: RegistrySub,
    },
}

/// `cs galaxies registry <sub>` — debug surface for the `cosmon-registry`
/// crate. Read-only; the source of truth is the TOML file.
#[derive(clap::Subcommand)]
pub enum RegistrySub {
    /// List every galaxy declared in the registry TOML.
    ///
    /// With `--json`, emits NDJSON (one entry per line) — the shape
    /// `cs ask` and other pilot agents can pipe into `jq` without
    /// extra envelope parsing.
    #[command(alias = "ls")]
    List,
    /// Resolve a single galaxy by name. Exits with status 1 if the
    /// name is not registered, so scripts can use it as a gate.
    Resolve {
        /// Galaxy name to look up (exact match, case-sensitive).
        name: String,
    },
}

/// One galaxy row in the rendered output.
#[derive(serde::Serialize)]
struct GalaxyEntry {
    /// Galaxy name (`repos.name`).
    pub name: String,
    /// `galaxy_kind` token — `infra | project | social-hub | editorial`
    /// or `null` for nascent galaxies.
    pub galaxy_kind: Option<String>,
    /// Last-activity timestamp pulled straight from `repos.updated_at`.
    /// We expose it unchanged so downstream tools (drift-detection,
    /// Mur du Matin projection) can parse a single format.
    pub last_activity: Option<String>,
}

/// `--json` envelope for `cs galaxies list`.
#[derive(serde::Serialize)]
struct ListOutput {
    /// All galaxies, flat. Shape matches the `SELECT` on `repos` —
    /// tests drive on this, not on the grouped view.
    pub galaxies: Vec<GalaxyEntry>,
    /// Per-kind totals, including a `nascent` bucket for `NULL`.
    pub by_kind: BTreeMap<String, usize>,
}

/// Dispatch `cs galaxies <sub>`.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.cmd {
        Sub::List => run_list(ctx),
        Sub::Registry { cmd } => match cmd {
            RegistrySub::List => run_registry_list(ctx),
            RegistrySub::Resolve { name } => run_registry_resolve(ctx, name),
        },
    }
}

fn run_registry_list(ctx: &Context) -> anyhow::Result<()> {
    let idx = TomlGalaxyIndex::load_default()?;
    let entries = idx.list();

    if ctx.json {
        // NDJSON, not an envelope — each line is an independently
        // parseable galaxy entry. Keeps the pipe semantics simple for
        // `cs ask` and shell one-liners.
        for g in &entries {
            println!("{}", serde_json::to_string(g)?);
        }
        return Ok(());
    }

    if entries.is_empty() {
        let src = idx
            .source_path()
            .map_or_else(|| "(no config dir)".to_owned(), |p| p.display().to_string());
        println!(
            "{} {}",
            "\u{1F30C} galaxies registry:".bold(),
            format!("empty (expected at {src})").dimmed()
        );
        return Ok(());
    }

    println!("{}", "\u{1F30C} galaxies registry".bold());
    println!();
    for g in &entries {
        println!(
            "  {}  {}",
            g.name.bold(),
            g.path.display().to_string().dimmed()
        );
        println!("    fleet: {}", g.fleet);
        if !g.default_formulas.is_empty() {
            let mut pairs: Vec<String> = g
                .default_formulas
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            pairs.sort();
            println!("    default_formulas: {}", pairs.join(", "));
        }
    }
    Ok(())
}

fn run_registry_resolve(ctx: &Context, name: &str) -> anyhow::Result<()> {
    let idx = TomlGalaxyIndex::load_default()?;
    if let Some(g) = idx.resolve(name) {
        if ctx.json {
            println!("{}", serde_json::to_string(&g)?);
        } else {
            println!("{}: {}", g.name.bold(), g.path.display());
            println!("  fleet: {}", g.fleet);
            if !g.default_formulas.is_empty() {
                let mut pairs: Vec<String> = g
                    .default_formulas
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                pairs.sort();
                println!("  default_formulas: {}", pairs.join(", "));
            }
        }
        Ok(())
    } else {
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({ "error": "not_found", "name": name })
            );
        } else {
            eprintln!("galaxy `{name}` not in registry");
        }
        std::process::exit(1);
    }
}

fn run_list(ctx: &Context) -> anyhow::Result<()> {
    let entries = load_galaxies()?;

    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for e in &entries {
        let key = e
            .galaxy_kind
            .clone()
            .unwrap_or_else(|| "nascent".to_owned());
        *by_kind.entry(key).or_insert(0) += 1;
    }

    if ctx.json {
        let out = ListOutput {
            galaxies: entries,
            by_kind,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    render_plain(&entries, &by_kind);
    Ok(())
}

fn render_plain(entries: &[GalaxyEntry], by_kind: &BTreeMap<String, usize>) {
    println!("{}", "\u{1F30C} galaxies".bold());
    println!();

    // Render in the canonical family order so the output reads like
    // the taxonomy: bits-inward → bits-through → bits-lateral → bits-outward,
    // with nascent last as the "not yet classified" bucket.
    let families: Vec<(&str, &str)> = vec![
        ("infra", "Infra (bits flow inward)"),
        ("project", "Project (bits flow through)"),
        ("social-hub", "Social hub (bits flow laterally)"),
        ("editorial", "Editorial (bits flow outward)"),
        ("nascent", "Nascent (not yet classified)"),
    ];

    for (token, heading) in &families {
        let members: Vec<&GalaxyEntry> = entries
            .iter()
            .filter(|e| e.galaxy_kind.as_deref().unwrap_or("nascent") == *token)
            .collect();
        if members.is_empty() {
            continue;
        }
        println!("  {}:", heading.bold());
        for m in members {
            let age = m.last_activity.as_deref().unwrap_or("—");
            println!("    • {:<28} {}", m.name, age.dimmed());
        }
        println!();
    }

    // Footer totals — the same summary `--json` exposes.
    let mut summary_parts: Vec<String> = Vec::new();
    for (token, _) in &families {
        if let Some(n) = by_kind.get(*token) {
            summary_parts.push(format!("{token}={n}"));
        }
    }
    println!("  {}: {}", "Totals".bold(), summary_parts.join(" | "));
}

fn load_galaxies() -> anyhow::Result<Vec<GalaxyEntry>> {
    let db = neurion_db_path()?;
    if !db.exists() {
        // Fresh environment, no neurion DB yet — return empty so
        // `--json` still shapes cleanly and the plaintext view prints
        // only the header/footer.
        return Ok(Vec::new());
    }

    let conn = Connection::open(&db)?;

    // Pre-migration DBs (repos without the galaxy_kind column) fall
    // through to the legacy schema: every galaxy is reported as
    // nascent until `neurion` (or an equivalent boot) applies the
    // ADD COLUMN migration. Same tolerance as `cs status --json`.
    let with_kind = conn
        .prepare("SELECT name, galaxy_kind, updated_at FROM repos ORDER BY name")
        .is_ok();
    let sql = if with_kind {
        "SELECT name, galaxy_kind, updated_at FROM repos ORDER BY name"
    } else {
        "SELECT name, NULL as galaxy_kind, updated_at FROM repos ORDER BY name"
    };
    let mut stmt = conn.prepare(sql)?;

    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let galaxy_kind: Option<String> = row.get(1)?;
        let updated_at: Option<String> = row.get(2)?;

        // Guard: the SQL-level enum is free text; reject any stored
        // value that doesn't round-trip through `GalaxyKind::from_str`.
        // The cost is one enum parse per row, bought once per list.
        let galaxy_kind = galaxy_kind.and_then(|s| {
            if s.is_empty() {
                None
            } else if GalaxyKind::from_str(&s).is_some() {
                Some(s)
            } else {
                None
            }
        });

        Ok(GalaxyEntry {
            name,
            galaxy_kind,
            last_activity: updated_at,
        })
    })?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// Locate the neurion `SQLite` database that `cs galaxies list` reads.
///
/// Mirrors the path logic inside `neurion-mcp` (which keeps the
/// function private): `<data_dir>/neurion/neurion.db`. The directory
/// is *not* created here — read-only access must not side-effect a
/// fresh environment.
fn neurion_db_path() -> anyhow::Result<std::path::PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine data directory"))?
        .join("neurion");
    Ok(dir.join("neurion.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plain_with_no_entries_does_not_panic() {
        let entries: Vec<GalaxyEntry> = Vec::new();
        let by_kind: BTreeMap<String, usize> = BTreeMap::new();
        render_plain(&entries, &by_kind);
    }
}
