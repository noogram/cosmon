// SPDX-License-Identifier: AGPL-3.0-only

//! `cs observe` — inspect molecules and fleet.
//!
//! Without arguments, lists all active molecules in a table. With a molecule ID
//! (or prefix), shows the detailed state of that molecule. Supports filtering
//! by status, worker, and formula, and partial ID matching for convenience.
//!
//! # Coupling report (THESIS Part XVIII)
//!
//! Detail mode also emits a small **coupling report** — the same metrics
//! bundle that `cs wait` returns (`poll_count`, `transitions`, `energy`,
//! `entropy`, `temperature`), built via
//! [`cosmon_state::wait::coupling_report_snapshot`]. The two verbs share
//! the kernel so operators (human and AI via MCP) learn one vocabulary for
//! "what did this molecule cost", regardless of which read-only verb they
//! reached for. Since `cs observe` performs a single snapshot read, the
//! report hard-codes `poll_count = 1` and `transitions = 0`.
//!
//! The shape is deliberately frozen at five scalar fields to stay under the
//! Shannon cognitive-SNR ceiling (~7 decorrelated fields). Widening must go
//! through a successor ADR.

use std::path::Path;

use colored::Colorize;
use cosmon_core::auth::Subject;
use cosmon_core::id::{FormulaId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_state::ops::{detect_ghost, observe_loaded, ObserveJson};
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};

use super::Context;

/// Arguments for the `observe` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID (or prefix) to inspect. Omit to list all molecules.
    pub molecule_id: Option<String>,

    /// Filter by status (active, frozen, completed, collapsed).
    #[arg(long)]
    pub status: Option<String>,

    /// Filter by assigned worker.
    #[arg(long)]
    pub worker: Option<String>,

    /// Filter by formula.
    #[arg(long)]
    pub formula: Option<String>,

    /// Free-text search across molecule fields.
    #[arg(long)]
    pub search: Option<String>,

    /// Include completed and collapsed molecules (excluded by default in list mode).
    #[arg(long)]
    pub all: bool,

    /// Filter by tag glob pattern (repeatable, any-match).
    #[arg(long = "tag", value_name = "GLOB")]
    pub tag: Vec<String>,

    /// Number of trailing notes to show in detail mode (default: 3).
    #[arg(long = "notes", value_name = "N", default_value = "3")]
    pub notes: usize,
}

/// Execute the `observe` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.state_dir();
    let store = ctx.store();

    match &args.molecule_id {
        Some(id_or_prefix) => run_detail(ctx, store.as_ref(), &state_dir, id_or_prefix, args.notes),
        None => run_list(ctx, store.as_ref(), args),
    }
}

/// List molecules in a table, with optional filters.
fn run_list(ctx: &Context, store: &dyn StateStore, args: &Args) -> anyhow::Result<()> {
    let filter = build_filter(args)?;
    let mut molecules = store.list_molecules(&filter)?;

    // By default, exclude completed/collapsed unless --all or --status is set.
    // `&& !m.archived` enforces the `archived ⇒ off the shelf` reading
    // (idea-20260618-1b10): a legacy `{archived: true, status: Running}` row
    // is alive by `status` but has already been archived, so it must not
    // render in the default view. `--all` / `--status` still surface it.
    if !args.all && args.status.is_none() {
        molecules.retain(|m| m.status.is_alive() && !m.archived);
    }

    molecules.sort_by_key(|x| std::cmp::Reverse(x.updated_at));

    if ctx.json {
        let rows: Vec<_> = molecules.iter().map(molecule_to_row).collect();
        let json = serde_json::to_string_pretty(&rows)?;
        println!("{json}");
        return Ok(());
    }

    if molecules.is_empty() {
        println!("{}", "No molecules found.".dimmed());
        if !args.all {
            println!(
                "{}",
                "Use --all to include completed and collapsed molecules.".dimmed()
            );
        }
        return Ok(());
    }

    // Table header
    println!(
        "  {:<24} {:<20} {:<12} {:<10} {}",
        "ID".bold(),
        "FORMULA".bold(),
        "STATUS".bold(),
        "STEP".bold(),
        "WORKER".bold(),
    );
    println!("  {}", "─".repeat(80).dimmed());

    for mol in &molecules {
        let status_str = colorize_mol_status(mol.status);
        let step_str = format!("{}/{}", mol.current_step, mol.total_steps);
        let worker_str = mol
            .assigned_worker
            .as_ref()
            .map_or("-".to_owned(), ToString::to_string);
        // ADR-052 ghost suffix — appended after the worker so it catches
        // the eye without widening the table layout.
        let ghost_suffix = match detect_ghost(mol) {
            Some(g) => format!("  {}", format!("👻 {g}").red().bold()),
            None => String::new(),
        };

        println!(
            "  {:<24} {:<20} {:<12} {:<10} {}{}",
            mol.id, mol.formula_id, status_str, step_str, worker_str, ghost_suffix,
        );
    }

    println!();
    println!(
        "  {} {} molecule{}",
        "Total:".bold(),
        molecules.len(),
        if molecules.len() == 1 { "" } else { "s" },
    );

    Ok(())
}

/// Show detailed view of a single molecule, with partial ID matching.
#[allow(clippy::too_many_lines)]
fn run_detail(
    ctx: &Context,
    store: &dyn StateStore,
    state_dir: &Path,
    id_or_prefix: &str,
    notes_limit: usize,
) -> anyhow::Result<()> {
    let mol = resolve_molecule(store, id_or_prefix)?;

    // Library-first promotion (T2 — task-20260503-bbea, delib-20260502-6522
    // Form 5): both `cs observe` and cs-api's `GET /molecules/{id}` go
    // through `cosmon_state::ops::observe`. The CLI keeps prefix
    // resolution + colored rendering on top, but the read + ghost
    // detection + coupling report are now a single library call.
    //
    // T-RECTIFY (task-20260503-09c8) — observe now consumes the typed
    // `Subject` from T-SUBJECT (`task-20260503-c223`); the CLI is the
    // operator-side mint, so we hand it the wildcard-scoped operator.
    let view = observe_loaded(mol, state_dir, &Subject::operator());
    let mol = &view.data;
    let metrics = &view.metrics;
    let ghost = view.ghost;

    if ctx.json {
        let detail = ObserveJson::from_view(&view, &store.molecule_dir(&mol.id).to_string_lossy());
        let json = serde_json::to_string_pretty(&detail)?;
        println!("{json}");
        return Ok(());
    }

    // Header
    println!("{} {}", "Molecule:".bold(), mol.id.to_string().bold());
    println!();

    // ADR-052 ghost marker — surface drift immediately at the top of the
    // detail view so a pilot reading `cs observe <id>` sees the pathology
    // before any other field. Silent when the run-state is consistent.
    if let Some(g) = ghost {
        println!(
            "  {} {}",
            "👻 Ghost:".red().bold(),
            format!("{g}").red().bold(),
        );
        println!();
    }

    // Status with color
    let status_str = colorize_mol_status(mol.status);
    println!("  {:<16} {status_str}", "Status:".bold());
    println!("  {:<16} {}", "Formula:".bold(), mol.formula_id);
    println!(
        "  {:<16} {}/{}",
        "Step:".bold(),
        mol.current_step,
        mol.total_steps,
    );
    println!(
        "  {:<16} {}",
        "Worker:".bold(),
        mol.assigned_worker.as_ref().map_or("-", |w| w.as_str()),
    );
    println!("  {:<16} {}", "Created:".bold(), mol.created_at);
    println!("  {:<16} {}", "Updated:".bold(), mol.updated_at);

    // Completed steps
    if !mol.completed_steps.is_empty() {
        println!();
        println!("  {}", "Completed steps:".bold());
        for step in &mol.completed_steps {
            println!("    {} {step}", "✓".green());
        }
    }

    // Collapse info
    if mol.status == MoleculeStatus::Collapsed {
        if let Some(ref reason) = mol.collapse_reason {
            println!();
            println!("  {:<16} {}", "Collapse reason:".bold(), reason.red());
        }
        if let Some(step) = mol.collapsed_step {
            println!("  {:<16} {step}", "Collapsed at:".bold());
        }
    }

    // Variables
    if !mol.variables.is_empty() {
        println!();
        println!("  {}", "Variables:".bold());
        let mut vars: Vec<_> = mol.variables.iter().collect();
        vars.sort_by_key(|(k, _)| *k);
        for (k, v) in vars {
            println!("    {k} = {v}");
        }
    }

    // Links
    if !mol.links.is_empty() {
        println!();
        println!("  {}", "Links:".bold());
        for link in &mol.links {
            println!("    {link}");
        }
    }

    // Tags
    if !mol.tags.is_empty() {
        println!();
        println!(
            "  {} {}",
            "Tags:".bold(),
            mol.tags
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        );
    }

    // Recent notes (append-only audit trail)
    if notes_limit > 0 {
        let notes_dir = store.molecule_dir(&mol.id).join("notes");
        let recent = super::note::load_recent(&notes_dir, notes_limit);
        if !recent.is_empty() {
            println!();
            println!("  {} (last {})", "Notes:".bold(), recent.len());
            for n in &recent {
                let header = format!(
                    "#{seq} {author} {ts}",
                    seq = n.seq,
                    author = n.author,
                    ts = n.timestamp
                );
                println!("    {}", header.dimmed());
                for line in n.body.lines() {
                    println!("      {line}");
                }
            }
        }
    }

    // Model attribution (delib-20260704-b476 C3) — which model was pinned
    // for this molecule at `cs tackle`, and where the choice came from
    // (flag / formula-pin / env / config / floor). Read from the latest
    // `ModelSelected` event; silent (omit-if-none) for a molecule that was
    // never tackled or predates C2's typed event.
    if let Some(model) = &view.model {
        println!();
        println!("  {}", "Model:".bold());
        println!(
            "    {} {}",
            model.model_label().cyan(),
            format!("← {} ({})", model.source_detail(), model.adapter_name).dimmed(),
        );
    }

    // Coupling report — mirrors the bundle that `cs wait` prints. Same
    // vocabulary across read-only verbs (THESIS Part XVIII).
    if let Some(energy) = &metrics.energy {
        println!();
        println!("  {}", "Energy:".bold());
        println!(
            "    {} in / {} out tokens — ${:.4}",
            energy.input_tokens, energy.output_tokens, energy.cost_usd,
        );
    }

    // Per-molecule API token totals (task-20260625-d1fa). Summed from the
    // canonical token-meter sink keyed by `molecule_id`; silent when no
    // LLM call was recorded for the molecule (omit-if-none).
    if let Some(tokens) = &view.api_tokens {
        #[allow(clippy::cast_precision_loss)]
        let cost_usd = tokens.cost_micros_estimated as f64 / 1_000_000.0;
        println!();
        println!("  {}", "API tokens:".bold());
        println!(
            "    {} in / {} out ({} total) — {} call{} — ${:.4} est.",
            tokens.tokens_in,
            tokens.tokens_out,
            tokens.total_tokens(),
            tokens.invocations,
            if tokens.invocations == 1 { "" } else { "s" },
            cost_usd,
        );
    }

    Ok(())
}

/// Resolve a molecule by exact ID or prefix match.
///
/// If the input parses as a valid `MoleculeId`, attempts an exact load first.
/// Otherwise, lists all molecules and finds those whose ID starts with the input.
/// Returns an error if zero or more than one match.
fn resolve_molecule(store: &dyn StateStore, id_or_prefix: &str) -> anyhow::Result<MoleculeData> {
    // Try exact match first
    if let Ok(exact_id) = MoleculeId::new(id_or_prefix) {
        if let Ok(mol) = store.load_molecule(&exact_id) {
            return Ok(mol);
        }
    }

    // Prefix search: list all and filter
    let all = store.list_molecules(&MoleculeFilter::default())?;

    let matches: Vec<_> = all
        .into_iter()
        .filter(|m| m.id.as_str().starts_with(id_or_prefix))
        .collect();

    match matches.len() {
        0 => Err(anyhow::anyhow!("no molecule matching \"{id_or_prefix}\"")),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => {
            let ids: Vec<_> = matches.iter().map(|m| m.id.as_str()).collect();
            Err(anyhow::anyhow!(
                "ambiguous prefix \"{id_or_prefix}\" matches {n} molecules: {}",
                ids.join(", ")
            ))
        }
    }
}

/// Build a `MoleculeFilter` from CLI arguments.
fn build_filter(args: &Args) -> anyhow::Result<MoleculeFilter> {
    let status = args
        .status
        .as_deref()
        .map(str::parse::<MoleculeStatus>)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid status: {e}"))?;

    let worker = args
        .worker
        .as_deref()
        .map(WorkerId::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid worker: {e}"))?;

    let formula = args
        .formula
        .as_deref()
        .map(FormulaId::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid formula: {e}"))?;

    Ok(MoleculeFilter {
        fleet: None,
        kind: None,
        status,
        worker,
        formula,
        search_text: args.search.clone(),
        project: None,
        tag_globs: args.tag.clone(),
    })
}

// ---------------------------------------------------------------------------
// JSON output types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct MoleculeRow {
    id: String,
    formula: String,
    status: String,
    current_step: usize,
    total_steps: usize,
    worker: Option<String>,
    /// ADR-052 ghost marker — `None` when the row's run-state is
    /// internally consistent.
    #[serde(skip_serializing_if = "Option::is_none")]
    ghost: Option<String>,
}

fn molecule_to_row(mol: &MoleculeData) -> MoleculeRow {
    MoleculeRow {
        id: mol.id.to_string(),
        formula: mol.formula_id.to_string(),
        status: mol.status.to_string(),
        current_step: mol.current_step,
        total_steps: mol.total_steps,
        worker: mol.assigned_worker.as_ref().map(ToString::to_string),
        ghost: detect_ghost(mol).map(|g| g.as_str().to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

fn colorize_mol_status(status: MoleculeStatus) -> String {
    let s = status.to_string();
    match status {
        MoleculeStatus::Pending => s.cyan().to_string(),
        MoleculeStatus::Queued => s.blue().to_string(),
        MoleculeStatus::Running => s.green().to_string(),
        MoleculeStatus::Frozen => s.yellow().to_string(),
        // ADR-062: Starved is a "wait or rotate" hint — magenta to
        // distinguish it from Frozen (operator-suspended).
        MoleculeStatus::Starved => s.magenta().to_string(),
        MoleculeStatus::Completed => s.dimmed().to_string(),
        MoleculeStatus::Collapsed => s.red().to_string(),
        _ => s,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use cosmon_core::id::{FormulaId, MoleculeId, StepId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, MoleculeFilter, StateStore};
    use tempfile::TempDir;

    use super::*;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        (tmp, store)
    }

    fn make_molecule(
        id: &str,
        status: MoleculeStatus,
        step: usize,
        total: usize,
        worker: Option<&str>,
    ) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: cosmon_core::id::FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("mol-polecat-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: worker.map(|w| WorkerId::new(w).unwrap()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            total_steps: total,
            current_step: step,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
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
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn test_observe_list_empty() {
        let (tmp, _store) = make_store();
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            molecule_id: None,
            status: None,
            worker: None,
            formula: None,
            search: None,
            all: false,
            tag: Vec::new(),
            notes: 3,
        };
        let result = run(&ctx, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_observe_list_with_molecules() {
        let (tmp, store) = make_store();

        let m1 = make_molecule(
            "cs-20260401-aaaa",
            MoleculeStatus::Running,
            1,
            3,
            Some("ruby"),
        );
        let m2 = make_molecule("cs-20260401-bbbb", MoleculeStatus::Frozen, 0, 2, None);
        let m3 = make_molecule("cs-20260401-cccc", MoleculeStatus::Completed, 3, 3, None);
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();
        store.save_molecule(&m3.id, &m3).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };

        // Default: only active + frozen
        let args = Args {
            molecule_id: None,
            status: None,
            worker: None,
            formula: None,
            search: None,
            all: false,
            tag: Vec::new(),
            notes: 3,
        };
        let result = run(&ctx, &args);
        assert!(result.is_ok());

        // Verify via store that filtering works conceptually
        let all = store.list_molecules(&MoleculeFilter::default()).unwrap();
        assert_eq!(all.len(), 3);
        let alive: Vec<_> = all.iter().filter(|m| m.status.is_alive()).collect();
        assert_eq!(alive.len(), 2);
    }

    #[test]
    fn test_observe_detail() {
        let (tmp, store) = make_store();

        let mut mol = make_molecule(
            "cs-20260401-dddd",
            MoleculeStatus::Running,
            1,
            3,
            Some("obsidian"),
        );
        mol.completed_steps = vec![StepId::new("load-context").unwrap()];
        mol.variables
            .insert("base_branch".to_owned(), "main".to_owned());
        mol.links = vec!["https://example.com".to_owned()];
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = Args {
            molecule_id: Some("cs-20260401-dddd".to_owned()),
            status: None,
            worker: None,
            formula: None,
            search: None,
            all: false,
            tag: Vec::new(),
            notes: 3,
        };
        let result = run(&ctx, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_observe_filter_by_status() {
        let (tmp, store) = make_store();

        let m1 = make_molecule("cs-20260401-eeee", MoleculeStatus::Running, 0, 2, None);
        let m2 = make_molecule("cs-20260401-ffff", MoleculeStatus::Collapsed, 1, 3, None);
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };

        // Filter for collapsed only
        let args = Args {
            molecule_id: None,
            status: Some("collapsed".to_owned()),
            worker: None,
            formula: None,
            search: None,
            all: true,
            tag: Vec::new(),
            notes: 3,
        };
        let result = run(&ctx, &args);
        assert!(result.is_ok());

        // Verify via store
        let filter = MoleculeFilter {
            status: Some(MoleculeStatus::Collapsed),
            ..MoleculeFilter::default()
        };
        let filtered = store.list_molecules(&filter).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id.as_str(), "cs-20260401-ffff");
    }

    #[test]
    fn test_observe_partial_id_match() {
        let (_tmp, store) = make_store();

        let m1 = make_molecule("cs-20260401-abcd", MoleculeStatus::Running, 0, 2, None);
        let m2 = make_molecule("cs-20260401-abef", MoleculeStatus::Running, 0, 2, None);
        let m3 = make_molecule("cs-20260401-zzzz", MoleculeStatus::Running, 0, 2, None);
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();
        store.save_molecule(&m3.id, &m3).unwrap();

        // Unique prefix match
        let mol = resolve_molecule(&store, "cs-20260401-z");
        assert!(mol.is_ok());
        assert_eq!(mol.unwrap().id.as_str(), "cs-20260401-zzzz");

        // Ambiguous prefix
        let mol = resolve_molecule(&store, "cs-20260401-ab");
        assert!(mol.is_err());
        let err = mol.unwrap_err();
        assert!(err.to_string().contains("ambiguous"));

        // No match
        let mol = resolve_molecule(&store, "xx-20260401-nope");
        assert!(mol.is_err());
        let err = mol.unwrap_err();
        assert!(err.to_string().contains("no molecule"));
    }
}
