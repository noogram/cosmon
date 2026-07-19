// SPDX-License-Identifier: AGPL-3.0-only

//! Project pulse command — `cs status`.
//!
//! Shows a quick DAG overview of the project state, like `git status` for
//! agent orchestration. Two modes:
//!
//! - **Compact** (default): one-line summary with attention bar
//! - **Verbose** (`--verbose`): full dashboard with molecules, sessions,
//!   contributions, and surfaces

use std::collections::{BTreeMap, HashMap};

use colored::Colorize;
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::transport::TransportBackend;
use cosmon_state::MoleculeFilter;

use super::Context;

/// Arguments for the `status` subcommand.
#[derive(clap::Args)]
pub struct Args;

/// JSON output structure for `cs status --json`.
#[derive(serde::Serialize)]
struct StatusOutput {
    molecules: MoleculeCounts,
    sessions: SessionSummary,
    contributions: Vec<ContributionInfo>,
    surfaces: SurfaceStatus,
    attention: AttentionInfo,
    /// Four-family taxonomy snapshot.
    /// Keyed by kind token (`infra | project | social-hub | editorial
    /// | nascent`) to totals, plus a flat `total` and `nascent`.
    galaxies: GalaxiesSummary,
}

/// Per-kind galaxy totals as emitted inside `cs status --json`.
#[derive(serde::Serialize)]
struct GalaxiesSummary {
    /// Kind → count. Missing entries mean zero — callers should not
    /// rely on every token being present.
    pub by_kind: BTreeMap<String, usize>,
    /// Total galaxies known to the registry (sum of `by_kind`).
    pub total: usize,
    /// Galaxies with `NULL` `galaxy_kind`. Equal to `by_kind["nascent"]`
    /// when present; surfaced as its own field so dashboards do not
    /// have to second-guess the key ordering.
    pub nascent: usize,
}

#[derive(serde::Serialize)]
struct MoleculeCounts {
    alive: usize,
    completed: usize,
    collapsed: usize,
    by_kind: HashMap<String, usize>,
    by_status: HashMap<String, usize>,
}

#[derive(serde::Serialize)]
struct SessionSummary {
    active: Vec<SessionInfo>,
    zombies: Vec<SessionInfo>,
}

#[derive(serde::Serialize)]
struct SessionInfo {
    worker: String,
    molecule: String,
    status: String,
}

#[derive(serde::Serialize)]
struct ContributionInfo {
    branch: String,
    commits_ahead: usize,
}

#[derive(serde::Serialize)]
struct SurfaceStatus {
    up_to_date: bool,
    last_reconcile: Option<String>,
    stale_count: usize,
}

#[derive(serde::Serialize)]
struct AttentionInfo {
    alive: usize,
    budget: Option<usize>,
    percent: Option<f64>,
}

/// Execute the `status` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, _args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let fleet = store.load_fleet()?;
    let molecules = store.list_molecules(&MoleculeFilter::default())?;

    // --- Molecule counts ---
    let alive = molecules.iter().filter(|m| m.status.is_alive()).count();
    let completed = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Completed)
        .count();
    let collapsed = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Collapsed)
        .count();

    let mut by_kind: HashMap<MoleculeKind, usize> = HashMap::new();
    for mol in &molecules {
        let kind = mol.kind.unwrap_or(MoleculeKind::Task);
        if mol.status.is_alive() {
            *by_kind.entry(kind).or_default() += 1;
        }
    }

    let mut by_status: HashMap<MoleculeStatus, usize> = HashMap::new();
    for mol in &molecules {
        *by_status.entry(mol.status).or_default() += 1;
    }

    // --- Sessions (tmux) ---
    let project_socket = super::tmux_socket_name(ctx);
    let backends = discover_fleet_backends(&state_dir, &project_socket);
    let live_sessions = discover_live_sessions(&backends);

    let mut active_sessions: Vec<SessionInfo> = Vec::new();
    let mut zombie_sessions: Vec<SessionInfo> = Vec::new();

    for (worker_name, _session_name) in &live_sessions {
        // Find the worker in fleet state
        let worker_data = fleet
            .workers
            .values()
            .find(|w| w.id.as_str() == worker_name);
        let mol_id = worker_data.and_then(|w| w.current_molecule.as_ref());
        let mol_status = mol_id.and_then(|mid| molecules.iter().find(|m| m.id == *mid));

        let mol_display = mol_id.map_or_else(|| "-".to_owned(), ToString::to_string);

        // Zombie: tmux session exists but molecule is terminal or missing
        let is_zombie = match mol_status {
            Some(m) => m.status.is_terminal(),
            None => mol_id.is_some(), // assigned but molecule not found
        };

        let info = SessionInfo {
            worker: worker_name.clone(),
            molecule: mol_display,
            status: if is_zombie {
                "zombie".to_owned()
            } else {
                "active".to_owned()
            },
        };

        if is_zombie {
            zombie_sessions.push(info);
        } else {
            active_sessions.push(info);
        }
    }

    // --- Contributions (git branches) ---
    let contributions = discover_contributions();

    // --- Surfaces ---
    let surface_status = check_surfaces(&state_dir);

    // --- Galaxies (neurion-backed) ---
    // Failure to read the neurion DB is non-fatal: pulse must never
    // depend on a sibling component being booted. Absent data shows
    // as total=0 rather than an error.
    let galaxies = load_galaxies_summary().unwrap_or_else(|_| GalaxiesSummary {
        by_kind: BTreeMap::new(),
        total: 0,
        nascent: 0,
    });

    // --- Attention ---
    let budget = fleet.attention_budget;
    let attention_percent = budget.map(|b| {
        if b == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let pct = (alive as f64 / b as f64) * 100.0;
            pct
        }
    });

    // --- Output ---
    if ctx.json {
        let output = StatusOutput {
            molecules: MoleculeCounts {
                alive,
                completed,
                collapsed,
                by_kind: by_kind.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                by_status: by_status.iter().map(|(s, v)| (s.to_string(), *v)).collect(),
            },
            sessions: SessionSummary {
                active: active_sessions,
                zombies: zombie_sessions,
            },
            contributions,
            surfaces: surface_status,
            attention: AttentionInfo {
                alive,
                budget,
                percent: attention_percent,
            },
            galaxies,
        };
        let json = serde_json::to_string_pretty(&output)?;
        println!("{json}");
        return Ok(());
    }

    if ctx.verbose {
        render_verbose(
            alive,
            completed,
            collapsed,
            &by_kind,
            &active_sessions,
            &zombie_sessions,
            &contributions,
            &surface_status,
            budget,
            attention_percent,
            &galaxies,
        );
    } else {
        render_compact(
            alive,
            &by_kind,
            &active_sessions,
            &zombie_sessions,
            &contributions,
            &surface_status,
            budget,
            attention_percent,
            &galaxies,
        );
    }

    Ok(())
}

/// Render compact one-line status.
#[allow(clippy::too_many_arguments)]
fn render_compact(
    alive: usize,
    by_kind: &HashMap<MoleculeKind, usize>,
    active_sessions: &[SessionInfo],
    zombie_sessions: &[SessionInfo],
    contributions: &[ContributionInfo],
    surfaces: &SurfaceStatus,
    budget: Option<usize>,
    attention_percent: Option<f64>,
    galaxies: &GalaxiesSummary,
) {
    // Line 1: molecule summary
    let mut parts: Vec<String> = Vec::new();

    // Kind breakdown (sorted for stable output)
    let mut kind_parts: Vec<String> = Vec::new();
    for kind in &[
        MoleculeKind::Idea,
        MoleculeKind::Task,
        MoleculeKind::Issue,
        MoleculeKind::Decision,
        MoleculeKind::Signal,
    ] {
        if let Some(&count) = by_kind.get(kind) {
            if count > 0 {
                kind_parts.push(format!("{}{}", count, kind.emoji()));
            }
        }
    }

    let kind_str = if kind_parts.is_empty() {
        String::new()
    } else {
        format!(": {}", kind_parts.join(" "))
    };
    parts.push(format!("{alive} alive{kind_str}"));

    // Sessions
    if !active_sessions.is_empty() {
        parts.push(format!(
            "{}{}active",
            active_sessions.len(),
            MoleculeStatus::Running.emoji()
        ));
    }
    if !zombie_sessions.is_empty() {
        parts.push(
            format!(
                "{}{}zombie",
                zombie_sessions.len(),
                " \u{1F480} " // skull emoji
            )
            .red()
            .to_string(),
        );
    }

    // Contributions
    let unmerged: usize = contributions.len();
    if unmerged > 0 {
        parts.push(format!("{unmerged}\u{1F500} to merge"));
    }

    // Surfaces
    let surfaces_str = if surfaces.up_to_date {
        "\u{2705}".to_owned() // checkmark
    } else {
        format!("\u{26A0}\u{FE0F} {} stale", surfaces.stale_count)
            .yellow()
            .to_string()
    };
    parts.push(format!("surfaces {surfaces_str}"));

    println!(
        "{} {}",
        "\u{1F9EA} cosmon status".bold(), // test tube emoji
        parts.join(" | ")
    );

    // Attention bar
    if let (Some(b), Some(pct)) = (budget, attention_percent) {
        println!(
            "  {}: {}/{} ({:.0}%) {}",
            "Attention".bold(),
            alive,
            b,
            pct,
            render_bar(pct, 20)
        );
    }

    // Galaxies one-liner — suppress entirely when the registry is
    // empty (typical for a fresh project or a session before neurion
    // has run discovery). When populated, the line is dense by design:
    // operators scan "10 galaxies: 1 infra, 7 project, 2 social-hub,
    // 1 editorial" in a single saccade.
    if galaxies.total > 0 {
        println!(
            "  {}: {}",
            "Galaxies".bold(),
            render_galaxies_line(galaxies)
        );
    }
}

/// Render verbose dashboard.
#[allow(clippy::too_many_arguments)]
fn render_verbose(
    alive: usize,
    completed: usize,
    collapsed: usize,
    by_kind: &HashMap<MoleculeKind, usize>,
    active_sessions: &[SessionInfo],
    zombie_sessions: &[SessionInfo],
    contributions: &[ContributionInfo],
    surfaces: &SurfaceStatus,
    budget: Option<usize>,
    attention_percent: Option<f64>,
    galaxies: &GalaxiesSummary,
) {
    println!("{}", "\u{1F9EA} cosmon status".bold());
    println!();

    // Molecules section
    println!(
        "  {}: {} alive, {} completed, {} collapsed",
        "Molecules".bold(),
        alive,
        completed,
        collapsed
    );

    // Kind breakdown
    for kind in &[
        MoleculeKind::Idea,
        MoleculeKind::Task,
        MoleculeKind::Issue,
        MoleculeKind::Decision,
        MoleculeKind::Signal,
    ] {
        if let Some(&count) = by_kind.get(kind) {
            if count > 0 {
                println!("    {} {} {}s", kind.emoji(), count, kind);
            }
        }
    }

    // Sessions section
    if !active_sessions.is_empty() || !zombie_sessions.is_empty() {
        println!();
        println!("  {}:", "Sessions".bold());
        for s in active_sessions {
            println!(
                "    {} {} ({}) running",
                "\u{25B6}\u{FE0F}".green(), // play button
                s.worker,
                s.molecule
            );
        }
        for s in zombie_sessions {
            println!(
                "    \u{1F480} {} ({}) {} — kill it",
                s.worker,
                s.molecule,
                "zombie".red().bold()
            );
        }
    }

    // Contributions section
    if !contributions.is_empty() {
        println!();
        println!("  {}:", "Contributions".bold());
        for c in contributions {
            println!(
                "    \u{1F500} {}  {} commits ahead of main",
                c.branch, c.commits_ahead
            );
        }
    }

    // Surfaces section
    println!();
    if surfaces.up_to_date {
        let age = surfaces.last_reconcile.as_deref().unwrap_or("unknown");
        println!(
            "  {}: \u{2705} up to date (last reconcile: {age})",
            "Surfaces".bold(),
        );
    } else {
        println!(
            "  {}: {} {} stale surfaces — run `cs reconcile`",
            "Surfaces".bold(),
            "\u{26A0}\u{FE0F}".yellow(),
            surfaces.stale_count
        );
    }

    // Attention bar
    if let (Some(b), Some(pct)) = (budget, attention_percent) {
        println!();
        println!(
            "  {}: {}/{} ({:.0}%) {}",
            "Attention".bold(),
            alive,
            b,
            pct,
            render_bar(pct, 20)
        );
    }

    // Galaxies section — shown only when neurion has real data,
    // so an empty fleet stays silent.
    if galaxies.total > 0 {
        println!();
        println!("  {}: {} total", "Galaxies".bold(), galaxies.total);
        println!("    {}", render_galaxies_line(galaxies));
        println!(
            "    {} to classify (see `cs galaxies list`)",
            if galaxies.nascent == 0 {
                "none".to_owned()
            } else {
                galaxies.nascent.to_string()
            }
        );
    }
}

/// Render a Unicode block progress bar.
///
/// Uses `\u{2588}` (full block) and `\u{2591}` (light shade) to build
/// a visual bar of `width` characters representing the given percentage.
fn render_bar(percent: f64, width: usize) -> String {
    let clamped = percent.clamp(0.0, 100.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let filled = ((clamped / 100.0) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);

    let bar = format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(empty));

    // Colorize based on utilization
    if percent >= 90.0 {
        bar.red().bold().to_string()
    } else if percent >= 70.0 {
        bar.yellow().to_string()
    } else {
        bar.green().to_string()
    }
}

/// Discover all live tmux sessions across fleet backends.
///
/// Returns `(worker_name, session_name)` pairs.
fn discover_live_sessions(backends: &[cosmon_transport::TmuxBackend]) -> Vec<(String, String)> {
    let mut sessions = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for be in backends {
        if let Ok(list) = be.list_sessions() {
            for info in list {
                let name = info.worker_id.as_str().to_owned();
                if seen.insert(name.clone()) {
                    sessions.push((name, info.session_name));
                }
            }
        }
    }
    sessions
}

/// Discover git branches not merged to main (contributions).
fn discover_contributions() -> Vec<ContributionInfo> {
    let output = std::process::Command::new("git")
        .args(["branch", "--no-merged", "main", "--format=%(refname:short)"])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut contributions = Vec::new();

    for branch in stdout.lines() {
        let branch = branch.trim();
        if branch.is_empty() {
            continue;
        }

        // Count commits ahead of main
        let ahead = std::process::Command::new("git")
            .args(["rev-list", "--count", &format!("main..{branch}")])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<usize>()
                        .ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);

        if ahead > 0 {
            contributions.push(ContributionInfo {
                branch: branch.to_owned(),
                commits_ahead: ahead,
            });
        }
    }

    contributions
}

/// Check surface freshness from the snapshot file.
fn check_surfaces(state_dir: &std::path::Path) -> SurfaceStatus {
    let snapshot = cosmon_surface::snapshot::load_snapshot(state_dir);

    if snapshot.surfaces.is_empty() {
        return SurfaceStatus {
            up_to_date: true,
            last_reconcile: None,
            stale_count: 0,
        };
    }

    // Find the most recent projection timestamp
    let last_reconcile = snapshot
        .surfaces
        .values()
        .filter_map(|s| chrono::DateTime::parse_from_rfc3339(&s.projected_at).ok())
        .max()
        .map(|ts| {
            let age = chrono::Utc::now() - ts.with_timezone(&chrono::Utc);
            format_duration(age)
        });

    // Count stale surfaces by checking if files on disk still match snapshot hashes
    let mut stale_count = 0;
    for (surface_path, snap) in &snapshot.surfaces {
        // Try to read the surface file relative to the project root
        // State dir is typically .cosmon/state/, project root is two levels up
        let project_root = state_dir.parent().and_then(|p| p.parent());
        if let Some(root) = project_root {
            let full_path = root.join(surface_path);
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let hash = sha256_hex(&content);
                if hash != snap.content_hash {
                    stale_count += 1;
                }
            }
            // Missing file = stale (surface was projected but file deleted)
            else if !full_path.exists() {
                stale_count += 1;
            }
        }
    }

    SurfaceStatus {
        up_to_date: stale_count == 0,
        last_reconcile,
        stale_count,
    }
}

/// Compute SHA-256 hex digest of a string.
fn sha256_hex(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Format a chrono Duration as a human-readable age string (e.g. "2m ago", "3h ago").
fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Render a one-line breakdown of the per-kind galaxy counts.
///
/// Iterates in the canonical family order (infra → editorial → nascent)
/// so operators see the same shape across every invocation. Missing
/// kinds are skipped, not printed as `0`, to keep the line dense.
fn render_galaxies_line(galaxies: &GalaxiesSummary) -> String {
    let order = ["infra", "project", "social-hub", "editorial", "nascent"];
    let parts: Vec<String> = order
        .iter()
        .filter_map(|token| {
            galaxies
                .by_kind
                .get(*token)
                .filter(|&&n| n > 0)
                .map(|n| format!("{n} {token}"))
        })
        .collect();
    parts.join(", ")
}

/// Load the four-family summary from neurion's registry DB.
///
/// Read-only; a missing DB is modeled as an empty summary rather than
/// an error so `cs status` works in environments where neurion has
/// never booted.
fn load_galaxies_summary() -> anyhow::Result<GalaxiesSummary> {
    let db = neurion_db_path()?;
    if !db.exists() {
        return Ok(GalaxiesSummary {
            by_kind: BTreeMap::new(),
            total: 0,
            nascent: 0,
        });
    }

    let conn = rusqlite::Connection::open(&db)?;
    // If the column is absent (pre-migration DB) we fall through to an
    // empty summary. Pragmatic: the single row-reading error path also
    // catches "no such column" without a second probe.
    let sql = "SELECT galaxy_kind FROM repos";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Ok(GalaxiesSummary {
            by_kind: BTreeMap::new(),
            total: 0,
            nascent: 0,
        });
    };
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    let mut nascent = 0usize;

    let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0))?;
    for row in rows {
        let kind_opt = row?;
        total += 1;
        let key = match kind_opt {
            Some(ref s) if !s.is_empty() && neurion_core::GalaxyKind::from_str(s).is_some() => {
                s.clone()
            }
            _ => {
                nascent += 1;
                "nascent".to_owned()
            }
        };
        *by_kind.entry(key).or_insert(0) += 1;
    }

    Ok(GalaxiesSummary {
        by_kind,
        total,
        nascent,
    })
}

/// Locate the neurion `SQLite` database. Mirrors the private `db_path`
/// in neurion-mcp — kept in lockstep by the same convention
/// (`<data_dir>/neurion/neurion.db`). No side effects.
fn neurion_db_path() -> anyhow::Result<std::path::PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine data directory"))?
        .join("neurion");
    Ok(dir.join("neurion.db"))
}

/// Discover all fleet-scoped tmux backends (same as ensemble.rs).
fn discover_fleet_backends(
    state_dir: &std::path::Path,
    project_socket: &str,
) -> Vec<cosmon_transport::TmuxBackend> {
    let mut backends = Vec::new();
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(name) = spec["name"].as_str() {
                            backends.push(cosmon_transport::TmuxBackend::new(name));
                        }
                    }
                }
            }
        }
    }
    // Always try the project socket as fallback.
    backends.push(cosmon_transport::TmuxBackend::new(project_socket));
    backends
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use chrono::Utc;

    use cosmon_core::id::{FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;

    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, MoleculeData, StateStore};
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        (tmp, store)
    }

    fn make_molecule(
        suffix: &str,
        status: MoleculeStatus,
        kind: Option<MoleculeKind>,
    ) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(format!("cs-20260401-{suffix}")).unwrap(),
            fleet_id: cosmon_core::id::FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("mol-polecat-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: 3,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: None,
            tags: std::collections::BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn test_status_empty_fleet() {
        let (tmp, store) = make_store();
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_with_molecules() {
        let (tmp, store) = make_store();
        let fleet = Fleet::default();
        store.save_fleet(&fleet).unwrap();

        // Create molecules of various kinds and statuses
        let mols = vec![
            make_molecule("idea1", MoleculeStatus::Running, Some(MoleculeKind::Idea)),
            make_molecule("task1", MoleculeStatus::Running, Some(MoleculeKind::Task)),
            make_molecule("task2", MoleculeStatus::Pending, Some(MoleculeKind::Task)),
            make_molecule("bug1", MoleculeStatus::Running, Some(MoleculeKind::Issue)),
            make_molecule("done1", MoleculeStatus::Completed, None),
            make_molecule("fail1", MoleculeStatus::Collapsed, None),
        ];
        for mol in &mols {
            store.save_molecule(&mol.id, mol).unwrap();
        }

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_json_output() {
        let (tmp, store) = make_store();
        let fleet = Fleet::default();
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("aaaa", MoleculeStatus::Running, Some(MoleculeKind::Task));
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_verbose() {
        let (tmp, store) = make_store();
        let fleet = Fleet::default();
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("bbbb", MoleculeStatus::Pending, Some(MoleculeKind::Idea));
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: true,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_render_bar() {
        let bar = render_bar(50.0, 10);
        // Should contain 5 full blocks and 5 light shade blocks (ignoring ANSI codes)
        assert!(bar.contains('\u{2588}'));
        assert!(bar.contains('\u{2591}'));
    }

    #[test]
    fn test_render_bar_boundaries() {
        let empty = render_bar(0.0, 10);
        assert!(empty.contains('\u{2591}'));

        let full = render_bar(100.0, 10);
        assert!(full.contains('\u{2588}'));

        // Over 100% clamps to 100%
        let over = render_bar(150.0, 10);
        assert!(over.contains('\u{2588}'));
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(chrono::Duration::seconds(30)), "30s ago");
        assert_eq!(format_duration(chrono::Duration::seconds(120)), "2m ago");
        assert_eq!(format_duration(chrono::Duration::seconds(7200)), "2h ago");
        assert_eq!(
            format_duration(chrono::Duration::seconds(172_800)),
            "2d ago"
        );
    }

    #[test]
    fn test_status_with_attention_budget() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        fleet.attention_budget = Some(50);
        store.save_fleet(&fleet).unwrap();

        // Create some alive molecules
        for i in 0..5 {
            let mol = make_molecule(
                &format!("attn{i}"),
                MoleculeStatus::Running,
                Some(MoleculeKind::Task),
            );
            store.save_molecule(&mol.id, &mol).unwrap();
        }

        // Compact mode
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args);
        assert!(result.is_ok());

        // Verbose mode
        let ctx = Context {
            verbose: true,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args);
        assert!(result.is_ok());
    }
}
