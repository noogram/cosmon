// SPDX-License-Identifier: AGPL-3.0-only

//! `cs project` / `cs reconcile` — project internal state onto surface files.
//!
//! Reads `.cosmon/surfaces.toml` and projects fleet/molecule state onto
//! standard files (STATUS.md, ISSUES.md, etc.) that any developer can read.
//!
//! See THESIS.md Part XVI (Surface Observability) and ADR-013.
//!
//! ADR-052 §D3 renames `cs reconcile → cs project`: the new name reads as
//! *"materialize views from the ledger"* while `reconcile` read as *"patch
//! something that drifted"*, which is the framing ADR-052 retires. The old
//! verb is kept as a deprecated alias for one release cycle (see
//! [`run_reconcile_alias`]).
//!
//! Surfaces are **derived views**, not human-editable documents
//! (CLAUDE.md: *"Source of truth: `.cosmon/state/`. Surfaces are derived
//! views"*). A divergence between the on-disk surface and a fresh projection
//! therefore never means *"preserve the human's edit"* or *"stage a 3-way
//! merge"* — it means the view is stale and must be regenerated from
//! authoritative state. `cs reconcile` always atomically overwrites every
//! surface; when the on-disk content diverged it logs a warning first so an
//! operator who *did* hand-edit a surface sees the change being replaced.
//!
//! The 3-way snapshot (`surfaces.snapshot.json`) is still computed — it
//! powers the `--check` dry-run report and the divergence warnings — but it
//! no longer gates writes, never writes git-style conflict blocks into the
//! auto-generated file, and never nucleates a resolver molecule.
//!
//! **History (2026-05-09 fix).** The retired escalation path treated
//! `cs done`'s out-of-band merge of STATUS.md / ISSUES.md as a true 3-way
//! conflict. It wrote `<<<<<<< human` blocks into the file (which re-wrapped
//! on every subsequent run — the observed 4-level marker stacking) and
//! nucleated a `task-work` resolver per run (the spurious "`decay_product`
//! children"). Surface files are not a legitimate cause of cognitive
//! escalation; see the 2026-05-09 chronicle entry and ADR-052 §D3.

use std::path::{Path, PathBuf};

use cosmon_core::declaration::MoleculeDeclaration;
use cosmon_core::formula::Formula;
use cosmon_state::{MoleculeFilter, StateStore};
use cosmon_surface::escalation::{classify_surface, SurfaceDecision};
use cosmon_surface::{DeclarationMap, FormulaMap, SurfaceConfig};

use super::Context;

/// Load every `.formula.toml` in `<cosmon_dir>/formulas/` into a [`FormulaMap`].
///
/// The map is consumed by [`cosmon_surface::project_surfaces`] so surface
/// renderers can resolve a molecule's formula declaration — step titles,
/// formula description, variable types — without re-reading TOML from disk.
///
/// Malformed or unreadable formulas are skipped silently: `cs reconcile`
/// must not hard-fail on a single bad formula file. The renderers are
/// already required to handle missing entries gracefully (legacy molecules,
/// deleted formulas), so a "best-effort load" is the right default here.
fn load_formulas(cosmon_dir: &Path) -> FormulaMap {
    let formulas_dir = cosmon_dir.join("formulas");
    let mut map = FormulaMap::new();
    let Ok(entries) = std::fs::read_dir(&formulas_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".formula.toml") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(formula) = Formula::parse(&content) {
            map.insert(formula.name.clone(), formula);
        }
    }
    map
}

/// Load every `*.toml` in `<cosmon_dir>/molecules/` into a [`DeclarationMap`],
/// keyed by the declaration's `id_prefix`.
///
/// Declarations are git-trackable intent files: each captures *which
/// instance* of the work a formula describes. Surface renderers use this
/// map to pull the most specific human-legible title for a molecule — more
/// precise than the formula's generic description and more structured than
/// free-form variables. See [`cosmon_surface::DeclarationMap`] for the
/// fallback chain wired around it.
///
/// Consistent with [`load_formulas`], malformed or unreadable declarations
/// are skipped silently: `cs reconcile` must not hard-fail because an
/// operator has left a stray `.toml` file in `.cosmon/molecules/`. A
/// missing `molecules/` directory is also fine (operators who do not use
/// the declarations pattern simply get an empty map).
///
/// Declarations whose `id_prefix` is empty are skipped — the key is
/// required, and an empty prefix would collide with every empty lookup.
/// Collisions between two declarations with the same prefix resolve
/// last-wins, which is acceptable because the renderers fall back cleanly
/// when the lookup misses or the chosen description is empty.
fn load_declarations(cosmon_dir: &Path) -> DeclarationMap {
    let molecules_dir = cosmon_dir.join("molecules");
    let mut map = DeclarationMap::new();
    let Ok(entries) = std::fs::read_dir(&molecules_dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.to_ascii_lowercase().ends_with(".toml") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(declaration) = MoleculeDeclaration::parse(&content) else {
            continue;
        };
        if declaration.id_prefix.is_empty() {
            continue;
        }
        map.insert(declaration.id_prefix.clone(), declaration);
    }
    map
}

/// Arguments for the `reconcile` subcommand.
///
/// The clippy `struct_excessive_bools` lint would fire here — CLI argument
/// structs are an exception: every flag clap sees *must* be a field, and
/// collapsing them to a config-object-of-options would just move the
/// booleans one layer down without actually simplifying the interface.
#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args)]
pub struct Args {
    /// Dry-run: check if surfaces are up to date without writing.
    #[arg(long)]
    check: bool,
    /// Fetch current GitHub Issue state before comparing (detect remote edits).
    #[arg(long)]
    fetch: bool,
    /// Deprecated no-op. Surfaces are always overwritten from authoritative
    /// state (derived-view semantics), so there is no longer a non-force
    /// mode to override. Accepted for backward compatibility.
    #[arg(long)]
    force: bool,
    /// Deprecated no-op. Surface conflicts no longer escalate or write
    /// git-style conflict blocks — surfaces are derived views and are always
    /// regenerated. Accepted for backward compatibility.
    #[arg(long = "no-escalate")]
    no_escalate: bool,
    /// Deprecated no-op. Reconcile never nucleates resolver molecules, so
    /// there is nothing to wait for. Accepted for backward compatibility.
    #[arg(long)]
    wait: bool,
    /// Heal the `archived ⇒ status.is_terminal()` invariant on disk.
    ///
    /// Default reconcile is a *pure projection* onto surfaces and never
    /// mutates molecule state (architectural-invariants.md). This flag
    /// opts into a one-shot migration: every molecule that is archived
    /// but carries a non-terminal status (a *ghost*, e.g.
    /// `{archived: true, status: running}`) is rewritten to
    /// `status = Collapsed` with reason `archived-but-alive heal`, and a
    /// `MoleculeStatusChanged` + `MoleculeCollapsed` event pair is
    /// appended so the heal survives a cache rebuild from `events.jsonl`.
    ///
    /// Idempotent: once healed, a second `--heal-invariants` pass finds
    /// nothing to do. Detect the violations first with
    /// `cs verify --invariants`.
    #[arg(long = "heal-invariants")]
    heal_invariants: bool,
}

/// Classified surface with all the inputs needed to apply the decision
/// downstream (write the file, record a conflict, or escalate).
///
/// `new_content` is retained so the snapshot update after a write uses the
/// exact bytes `project_surfaces` rendered — cheaper and safer than
/// re-rendering a second time. The `Escalate` variant of `decision` carries
/// the human-edited content, so we don't stash it separately.
struct SurfacePlan<'a> {
    surface: &'a cosmon_surface::Surface,
    new_content: String,
    decision: SurfaceDecision,
}

/// Execute the `reconcile` command.
///
/// # Errors
///
/// Returns an error if surfaces.toml is missing or files cannot be written.
///
/// Wired to both `cs project` (canonical) and `cs reconcile` (deprecated
/// alias, via [`run_reconcile_alias`]).
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    // Find the .cosmon/ directory (walk-up).
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);

    // The project root is the parent of .cosmon/ (state_dir is .cosmon/state/).
    let project_root = state_dir
        .parent()
        .and_then(|p| p.parent())
        .map_or_else(|| PathBuf::from("."), PathBuf::from);

    let cosmon_dir = project_root.join(".cosmon");
    let surfaces_path = cosmon_dir.join("surfaces.toml");

    // Ghost A (delib-20260704-b476 C4) — a config `[adapters.<name>]
    // .default_model` that resolves to a *strong* model (a member of that
    // adapter's `strong` cost-class set) is the original sticky-`/model` bug
    // in a config costume: it would silently dispatch strong with zero
    // per-molecule intent. Config may only *downgrade* (pin a non-strong
    // model); strong is reachable only from a positive per-molecule act
    // (`--model` / a formula-step pin). `cs reconcile --check` is the CI gate
    // that catches it. Runs *before* the surfaces.toml gate so it fires even
    // in galaxies that declare no surfaces, and independently of the surface
    // projection (it is a config-validity check, not a projection).
    let strong_default_violations = check_no_strong_config_default(&cosmon_dir);
    if !strong_default_violations.is_empty() {
        if ctx.json {
            let output = serde_json::json!({
                "status": "strong_config_default",
                "violations": strong_default_violations,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            eprintln!(
                "cs reconcile: safe-default violation — config default_model resolves \
                 to a strong model (delib-20260704-b476 C4, Ghost A):"
            );
            for v in &strong_default_violations {
                eprintln!("  ✗ {v}");
            }
            eprintln!(
                "\nConfig may only downgrade to a non-strong model; strong is reachable \
                 only from `cs tackle --model` or a formula-step pin. Remove the strong \
                 default_model, or drop the id from [adapters.<name>].strong."
            );
        }
        // Fail closed only in the CI dry-run (`--check`); a plain `cs reconcile`
        // (projection) reports but does not abort, so it can never wedge a
        // surface sync on a config lint.
        if args.check {
            std::process::exit(1);
        }
    }

    // ADR-147 tier a (C3) — a `[provider_bias]` committee whose *resolved*
    // endpoints collapse below its own add-only floor is a diversity downgrade
    // achieved through the `[adapters]` base_url layer (the proxy-costume), not
    // through editing the — inexpressibly-add-only — committee baseline. Same
    // shape and same `--check` fail-closed contract as the Ghost-A lint above;
    // runs here so it fires in every galaxy independently of surface
    // projection, and compares resolved endpoint tuples, never section names.
    let requirement_downgrades = check_no_profile_requirement_downgrade(&cosmon_dir);
    if !requirement_downgrades.is_empty() {
        if ctx.json {
            let output = serde_json::json!({
                "status": "provider_requirement_downgrade",
                "violations": requirement_downgrades,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            eprintln!(
                "cs reconcile: provider-committee diversity violation — the \
                 [provider_bias] committee resolves below its own floor \
                 (ADR-147 tier a):"
            );
            for v in &requirement_downgrades {
                eprintln!("  ✗ {v}");
            }
            eprintln!(
                "\nDiversity is measured on the RESOLVED endpoint (provider, base_url, \
                 model-family), never the adapter name. Point the colliding seats at \
                 distinct providers, or raise the committee so its resolved endpoints \
                 meet min_distinct_provider_endpoints. NB: the model-family label is \
                 derived from config, not attested (tier b / SameFamilyRefusal is the \
                 attested follow-on)."
            );
        }
        // Same fail-closed-under-`--check` contract as the Ghost-A lint.
        if args.check {
            std::process::exit(1);
        }
    }

    // Invariant heal pass (opt-in via `--heal-invariants`,
    // idea-20260618-1b10). Runs *first*, before the surfaces.toml gate
    // and the surface projection — the heal is a state-coherence
    // migration that is logically independent of surface rendering, so
    // it must also work in galaxies that declare no surfaces. Skipped
    // entirely by default — the default reconcile is a pure projection
    // and must not mutate molecule state. Under `--check` it is
    // detect-only (dry-run), consistent with the rest of the command.
    if args.heal_invariants {
        let store = ctx.store();
        heal_archived_terminal(ctx, store.as_ref(), &state_dir, args.check)?;
    }

    if !surfaces_path.exists() {
        if ctx.json {
            let output = serde_json::json!({
                "status": "no_config",
                "message": "No .cosmon/surfaces.toml found. Create one to enable surface projection.",
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("No .cosmon/surfaces.toml found.");
            println!();
            println!("Create one to enable surface projection. Example:");
            println!();
            println!("  [[surface]]");
            println!("  referent = \"project.status\"");
            println!("  kind = \"markdown\"");
            println!("  path = \"STATUS.md\"");
            println!();
            println!("  [[surface]]");
            println!("  referent = \"project.issues\"");
            println!("  kind = \"markdown\"");
            println!("  path = \"ISSUES.md\"");
        }
        return Ok(());
    }

    let config = SurfaceConfig::load(&surfaces_path)
        .map_err(|e| anyhow::anyhow!("failed to load surfaces.toml: {e}"))?;

    let store = ctx.store();

    // Cache-rebuild pass (ADR-052 R4): before projecting surfaces, ensure
    // every molecule's `state.json` is present and parsable. The events.jsonl
    // log is the source of truth; state.json is a derivable hot cache.
    // Missing / corrupt caches are rebuilt from the log in place. Up-to-date
    // caches are left alone so a healthy galaxy sees no write churn.
    let events_path = state_dir.join("events.jsonl");
    let fleets_root = state_dir.join("fleets");
    let rebuild_results = cosmon_state::rebuild_all_missing(&events_path, &fleets_root)
        .unwrap_or_else(|e| {
            eprintln!("  ⚠ state.json cache-rebuild skipped: {e}");
            Vec::new()
        });
    report_cache_rebuild(ctx, &rebuild_results);

    let fleet = store.load_fleet()?;
    let molecules = store.list_molecules(&MoleculeFilter::default())?;
    let formulas = load_formulas(&cosmon_dir);
    let declarations = load_declarations(&cosmon_dir);

    // Load projection snapshot for 3-way divergence detection.
    let snap = cosmon_surface::snapshot::load_snapshot(&state_dir);

    // --fetch: pull current GitHub Issue state to detect remote edits.
    if args.fetch {
        fetch_github_remote_state(&config, &state_dir);
    }

    if args.check {
        run_check(
            &config,
            &project_root,
            &state_dir,
            &fleet,
            &molecules,
            &formulas,
            &declarations,
            &snap,
        );
        return Ok(());
    }

    // Classify every markdown surface against the 3-way snapshot. The
    // classification is now used only to *warn* about divergence — surfaces
    // are derived views and are always overwritten from authoritative state
    // (see the module header for the 2026-05-09 conflict-marker-stacking
    // fix). GitHub surfaces keep their own sync path.
    let plans = classify_all(&config, &project_root, &fleet, &molecules, &formulas, &snap);

    // Warn — but do not block — when the on-disk surface diverged from the
    // last projection. An operator who hand-edited STATUS.md/ISSUES.md (or
    // a `cs done` that merged a stale copy from a feature branch) sees the
    // overwrite announced rather than silently swallowed. `Preserve` and
    // `Escalate` decisions both collapse to "overwrite with a warning"
    // because a derived view has no authority to preserve.
    let diverged: Vec<String> = plans
        .iter()
        .filter(|p| !matches!(p.decision, SurfaceDecision::Write))
        .map(|p| p.surface.path.clone())
        .collect();
    for path in &diverged {
        eprintln!(
            "  ⚠ {path}: on-disk surface diverged from authoritative state — \
             overwriting (surfaces are derived views, never merged)"
        );
    }

    if args.no_escalate || args.wait || args.force {
        eprintln!(
            "cs project: --force / --no-escalate / --wait are deprecated no-ops — \
             surfaces are always regenerated from state and never escalate."
        );
    }

    // Overwrite every surface from authoritative state. `force = true` makes
    // `project_filtered` ignore the per-surface decision and write all of
    // them — exactly the derived-view "always regenerate" contract. The
    // write is an atomic tempfile + rename inside `project_surfaces`, so a
    // surface file is never left half-written or merged.
    let written = project_filtered(
        &project_root,
        &fleet,
        &molecules,
        &formulas,
        &declarations,
        &plans,
        true,
    )?;

    // Atomic frontier projection (ADR-041) — collapsed ready ∧ merged state.
    // Rebuilt here so `cs reconcile` is the canonical "reproject everything
    // from authoritative state" command, and any stale `frontier.json`
    // left by an aborted `cs done` gets refreshed.
    match cosmon_state::frontier::compute(store.as_ref()) {
        Ok(f) => {
            if let Err(e) = cosmon_state::frontier::save(&state_dir, &f) {
                eprintln!("  ⚠ frontier.json write failed: {e}");
            }
        }
        Err(e) => eprintln!("  ⚠ frontier compute failed: {e}"),
    }

    // Record the projection snapshot for the next run's divergence report.
    // Every surface we wrote (i.e. all of them) gets a fresh baseline so the
    // next reconcile only warns about edits made *after* this projection.
    let written_set: std::collections::HashSet<&str> = written.iter().map(String::as_str).collect();
    let mut new_snap = snap.clone();
    for plan in &plans {
        if plan.surface.kind == cosmon_surface::SurfaceKind::GithubIssues {
            continue;
        }
        if written_set.contains(plan.surface.path.as_str()) {
            cosmon_surface::snapshot::record_projection(
                &mut new_snap,
                &plan.surface.path,
                &plan.new_content,
            );
        }
    }
    cosmon_surface::snapshot::save_snapshot(&state_dir, &new_snap)
        .map_err(|e| anyhow::anyhow!("failed to save snapshot: {e}"))?;

    // JSON / human report.
    if ctx.json {
        let output = serde_json::json!({
            "status": "projected",
            "written": written,
            "overwritten_diverged": diverged,
            "molecules": molecules.len(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Projected {} surfaces:", written.len());
        for path in &written {
            println!("  {path}");
        }
        if !diverged.is_empty() {
            println!(
                "Overwrote {} diverged surface(s) (derived views, never merged):",
                diverged.len()
            );
            for path in &diverged {
                println!("  ⚠️  {path}");
            }
        }
    }

    Ok(())
}

/// Print a short human / JSON-friendly summary of the cache-rebuild pass.
///
/// Up-to-date molecules are counted but not listed — the noise-to-signal on
/// a healthy galaxy would be high. Anything that required a write (missing
/// or corrupt cache) is named explicitly so operators see the recovery
/// happen.
fn report_cache_rebuild(
    ctx: &Context,
    results: &[(cosmon_core::id::MoleculeId, cosmon_state::RebuildOutcome)],
) {
    if results.is_empty() {
        return;
    }
    let mut created = Vec::new();
    let mut recovered = Vec::new();
    let mut ok = 0usize;
    for (id, outcome) in results {
        match outcome {
            cosmon_state::RebuildOutcome::CreatedFromEvents => created.push(id.as_str().to_owned()),
            cosmon_state::RebuildOutcome::RecoveredFromCorruption => {
                recovered.push(id.as_str().to_owned());
            }
            cosmon_state::RebuildOutcome::UpToDate
            | cosmon_state::RebuildOutcome::NoEventsForMolecule => ok += 1,
        }
    }
    if created.is_empty() && recovered.is_empty() {
        return;
    }
    if ctx.json {
        let payload = serde_json::json!({
            "cache_rebuild": {
                "created": created,
                "recovered": recovered,
                "up_to_date": ok,
            }
        });
        // stderr so it doesn't pollute the main projection JSON payload.
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        if !created.is_empty() {
            println!(
                "Rebuilt {} missing state.json from events.jsonl:",
                created.len()
            );
            for id in &created {
                println!("  🧬 {id}");
            }
        }
        if !recovered.is_empty() {
            println!(
                "Recovered {} corrupt state.json (archived as .broken):",
                recovered.len()
            );
            for id in &recovered {
                println!("  🩹 {id}");
            }
        }
    }
}

/// Execute the deprecated `cs reconcile` alias (ADR-052 §D3).
///
/// Emits a stderr deprecation notice, then delegates to [`run`] so output
/// is byte-identical to the canonical `cs project` command. The alias will
/// be removed after one release cycle.
pub fn run_reconcile_alias(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    eprintln!(
        "cs reconcile: deprecated — use `cs project` instead (ADR-052 §D3). \
         This alias will be removed after one release cycle."
    );
    run(ctx, args)
}

/// Dry-run branch: classify every surface and report the action that would
/// be taken. Never writes disk.
/// Scan `<cosmon_dir>/config.toml` for Ghost-A safe-default violations
/// (delib-20260704-b476 C4): a `[adapters.<name>].default_model` that is a
/// member of that same adapter's `strong` cost-class set.
///
/// Returns one human-readable message per violating adapter (empty when the
/// config is clean or absent). Best-effort: a missing or unparseable config
/// yields no violations — a lint must never abort on a config it cannot read
/// (the strong-set is fail-open by construction).
fn check_no_strong_config_default(cosmon_dir: &Path) -> Vec<String> {
    let config_path = cosmon_dir.join("config.toml");
    let Ok(cfg) = cosmon_filestore::load_project_config(&config_path) else {
        return Vec::new();
    };
    let Some(adapters) = cfg.adapters.as_ref() else {
        return Vec::new();
    };
    let mut violations = Vec::new();
    for name in adapters.available_names() {
        if let Some(entry) = adapters.entry(&name) {
            if cosmon_core::model_budget::config_default_is_strong(
                entry.default_model.as_deref(),
                &entry.strong,
            ) {
                violations.push(format!(
                    "[adapters.{name}].default_model = \"{}\" is in \
                     [adapters.{name}].strong (a strong default is forbidden)",
                    entry.default_model.as_deref().unwrap_or_default(),
                ));
            }
        }
    }
    violations
}

/// Scan `<cosmon_dir>/config.toml` for provider-committee requirement
/// **downgrades** (ADR-147 tier a, C3) — the sibling of
/// [`check_no_strong_config_default`].
///
/// Where the strong-default lint is a *value predicate over one field*, this
/// one is a *relation over the resolved committee*: it takes the effective
/// requirement-set (`[provider_bias]` baseline ∪ ⋃ profiles — a monotone union,
/// so no *declared* number can drop) and checks its **resolved** consequence
/// still holds. It reddens when the committee's seats resolve to the same
/// `(provider, base_url, model-family)` endpoint (an echo, not an independent
/// reader) or when the distinct-endpoint count falls below the declared
/// `min_distinct_provider_endpoints` floor. The comparison is on **resolved
/// requirement-ids + endpoint tuples, never config-section names** (ADR-147):
/// an `[adapters.openai]` seat whose `base_url` fronts Claude is unmasked, not
/// blessed by its label.
///
/// Returns one human-readable message per violation (empty when the committee
/// is diverse enough or absent). Best-effort, fail-open on a config it cannot
/// read — a lint must never abort on an unparseable config, and the whole
/// mechanism inherits the §8b trace-visibility ceiling: it is a CI dry-run that
/// makes a mono-family committee *loud*, not impossible.
fn check_no_profile_requirement_downgrade(cosmon_dir: &Path) -> Vec<String> {
    let config_path = cosmon_dir.join("config.toml");
    let Ok(cfg) = cosmon_filestore::load_project_config(&config_path) else {
        return Vec::new();
    };
    cosmon_core::provider_diversity::requirement_downgrade_violations(
        &cfg.provider_bias,
        cfg.adapters.as_ref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_check(
    config: &SurfaceConfig,
    project_root: &Path,
    state_dir: &Path,
    fleet: &cosmon_state::Fleet,
    molecules: &[cosmon_state::MoleculeData],
    formulas: &FormulaMap,
    declarations: &DeclarationMap,
    snap: &cosmon_surface::snapshot::ProjectionSnapshot,
) {
    let mut action_count = 0;

    for surface in &config.surface {
        if surface.kind == cosmon_surface::SurfaceKind::GithubIssues {
            // Real dry-run for the github leg (delib-20260721-f0b1 Tier-2):
            // GitHub has no native preview, so render the exact issue bodies
            // to local files the operator can review before any irreversible
            // API call. Filter molecules by the surface's kind filter to
            // match what a live sync would consider.
            let filtered: Vec<cosmon_state::MoleculeData> =
                cosmon_surface::filter_by_surface_kinds(surface, molecules)
                    .into_iter()
                    .cloned()
                    .collect();
            let state_ref = if state_dir.is_dir() {
                Some(state_dir)
            } else {
                None
            };
            action_count +=
                report_github_preview(surface, &filtered, state_ref, formulas, declarations);
            continue;
        }

        let new_content = render_for_surface(surface, fleet, molecules, formulas);
        if new_content.is_none() {
            continue;
        }
        let new_content = new_content.unwrap();

        let target = project_root.join(&surface.path);
        let current_file = std::fs::read_to_string(&target).unwrap_or_default();
        let snapshot_hash = snap
            .surfaces
            .get(&surface.path)
            .map(|s| s.content_hash.as_str());

        let divergence =
            cosmon_surface::snapshot::detect_divergence(snapshot_hash, &current_file, &new_content);

        match &divergence {
            cosmon_surface::snapshot::SurfaceDivergence::UpToDate => {
                println!("  {} {} — up to date", divergence.emoji(), surface.path);
            }
            cosmon_surface::snapshot::SurfaceDivergence::SourceChanged => {
                action_count += 1;
                println!(
                    "  {} {} — source changed (safe to overwrite)",
                    divergence.emoji(),
                    surface.path
                );
            }
            cosmon_surface::snapshot::SurfaceDivergence::SurfaceEdited => {
                action_count += 1;
                println!(
                    "  {} {} — edited on disk (derived view, will be overwritten)",
                    divergence.emoji(),
                    surface.path
                );
            }
            cosmon_surface::snapshot::SurfaceDivergence::Conflict => {
                action_count += 1;
                println!(
                    "  {} {} — diverged on both sides (derived view, will be overwritten)",
                    divergence.emoji(),
                    surface.path
                );
                // Show git diff so the human sees what the overwrite replaces.
                let diff = std::process::Command::new("git")
                    .args(["diff", "HEAD", "--", &surface.path])
                    .current_dir(project_root)
                    .output();
                if let Ok(output) = diff {
                    let diff_text = String::from_utf8_lossy(&output.stdout);
                    if !diff_text.is_empty() {
                        println!("        On-disk edits to be replaced (git diff):");
                        for line in diff_text.lines().take(20) {
                            println!("        {line}");
                        }
                        if diff_text.lines().count() > 20 {
                            println!(
                                "        ... ({} more lines)",
                                diff_text.lines().count() - 20
                            );
                        }
                    }
                }
                println!("        Run `cs reconcile` to regenerate it from authoritative state.");
            }
            cosmon_surface::snapshot::SurfaceDivergence::NeverProjected => {
                action_count += 1;
                println!(
                    "  {} {} — NEW ({} lines)",
                    divergence.emoji(),
                    surface.path,
                    new_content.lines().count()
                );
            }
        }
    }

    println!();
    if action_count == 0 {
        println!("All surfaces up to date.");
    } else {
        println!("{action_count} surface(s) need attention.");
        println!("Run `cs reconcile` (without --check) to apply.");
    }

    if action_count > 0 {
        std::process::exit(1);
    }
}

/// Render every issue a `github-issues` surface would publish to local files
/// under `<state_dir>/surfaces/github/<repo>/preview/` and print a summary.
///
/// This is the previewable dry-run for the github leg: GitHub itself offers
/// no way to see what an issue create/edit would produce, so `cs project
/// --check` materializes the exact bodies (marker-suppressed on public repos)
/// for human review before any irreversible API call. Returns the number of
/// issues that would create-or-update (unchanged issues do not count as
/// "attention"), so the caller can fold it into the overall action count.
fn report_github_preview(
    surface: &cosmon_surface::Surface,
    molecules: &[cosmon_state::MoleculeData],
    state_dir: Option<&Path>,
    formulas: &FormulaMap,
    declarations: &DeclarationMap,
) -> usize {
    let repo = surface.repo.as_deref().unwrap_or("?");
    let previews = cosmon_surface::preview_github_issues(
        surface,
        molecules,
        state_dir,
        formulas,
        declarations,
    );

    let visibility = if surface.is_public() {
        "public, ID-free"
    } else {
        "private"
    };
    println!("  {} → {repo} ({visibility}):", surface.referent);

    if previews.is_empty() {
        println!("    (no projectable molecules)");
        return 0;
    }

    // Write the exact bodies to a local preview directory for human review.
    // Best-effort: a write failure must not abort the dry-run, only warn.
    let preview_dir = state_dir.map(|sd| {
        sd.join("surfaces")
            .join("github")
            .join(repo.replace('/', "-"))
            .join("preview")
    });
    if let Some(dir) = &preview_dir {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("    ⚠ could not create preview dir {}: {e}", dir.display());
        }
    }

    let (mut creates, mut updates, mut unchanged) = (0usize, 0usize, 0usize);
    for p in &previews {
        let action = match p.action {
            cosmon_surface::PreviewAction::Create => {
                creates += 1;
                "create".to_string()
            }
            cosmon_surface::PreviewAction::Update(n) => {
                updates += 1;
                format!("update #{n}")
            }
            cosmon_surface::PreviewAction::Unchanged(n) => {
                unchanged += 1;
                format!("unchanged #{n}")
            }
        };
        println!("    [{action}] {}", p.title);

        if let Some(dir) = &preview_dir {
            let file = dir.join(format!("{}.md", p.molecule_id));
            let contents = format!("<!-- {action} -->\n# {}\n\n{}", p.title, p.body);
            if let Err(e) = std::fs::write(&file, contents) {
                eprintln!("    ⚠ could not write {}: {e}", file.display());
            }
        }
    }

    if let Some(dir) = &preview_dir {
        println!("    Bodies rendered for review: {}", dir.display());
    }
    if surface.is_public() {
        println!(
            "    Public repo: re-run with COSMON_SURFACE_PUBLISH=1 to publish \
             (fail-closed until then)."
        );
    }
    println!("    {creates} create, {updates} update, {unchanged} unchanged.");

    creates + updates
}

/// Render the content of a single markdown surface, or `None` if the
/// referent is unknown / the surface is a non-markdown kind.
fn render_for_surface(
    surface: &cosmon_surface::Surface,
    fleet: &cosmon_state::Fleet,
    molecules: &[cosmon_state::MoleculeData],
    formulas: &FormulaMap,
) -> Option<String> {
    // Match the same filter + rendering used by `project_surfaces` so the
    // snapshot comparison is apples-to-apples.
    let filtered: Vec<cosmon_state::MoleculeData> =
        cosmon_surface::filter_by_surface_kinds(surface, molecules)
            .into_iter()
            .cloned()
            .collect();

    match surface.referent.as_str() {
        "project.status" => Some(cosmon_surface::render_status_content(
            fleet,
            &filtered,
            formulas,
            surface.branding,
        )),
        "project.issues" => Some(cosmon_surface::render_issues_content(
            &filtered,
            formulas,
            surface.branding,
        )),
        "project.ideas" => Some(cosmon_surface::render_ideas_content(
            &filtered,
            formulas,
            surface.branding,
        )),
        "project.deliberations" => Some(cosmon_surface::render_deliberations_content(
            &filtered,
            formulas,
            surface.branding,
        )),
        _ => None,
    }
}

/// Classify every non-GitHub surface from the config. GitHub surfaces are
/// returned with a `Write` decision so the main projection path handles
/// them identically to clean surfaces — the escalation loop only reasons
/// about markdown files.
fn classify_all<'a>(
    config: &'a SurfaceConfig,
    project_root: &Path,
    fleet: &cosmon_state::Fleet,
    molecules: &[cosmon_state::MoleculeData],
    formulas: &FormulaMap,
    snap: &cosmon_surface::snapshot::ProjectionSnapshot,
) -> Vec<SurfacePlan<'a>> {
    let mut plans = Vec::with_capacity(config.surface.len());
    for surface in &config.surface {
        if surface.kind == cosmon_surface::SurfaceKind::GithubIssues {
            plans.push(SurfacePlan {
                surface,
                new_content: String::new(),
                decision: SurfaceDecision::Write,
            });
            continue;
        }
        let Some(new_content) = render_for_surface(surface, fleet, molecules, formulas) else {
            continue;
        };
        let target = project_root.join(&surface.path);
        let current_file = std::fs::read_to_string(&target).unwrap_or_default();
        let snapshot_hash = snap
            .surfaces
            .get(&surface.path)
            .map(|s| s.content_hash.as_str());
        let decision = classify_surface(snapshot_hash, &current_file, &new_content);
        plans.push(SurfacePlan {
            surface,
            new_content,
            decision,
        });
    }
    plans
}

/// Project surfaces, honouring the per-surface decision. `force=true`
/// bypasses `Preserve` and `Escalate` decisions (legacy "always write"
/// behaviour).
#[allow(clippy::too_many_arguments)]
fn project_filtered(
    project_root: &Path,
    fleet: &cosmon_state::Fleet,
    molecules: &[cosmon_state::MoleculeData],
    formulas: &FormulaMap,
    declarations: &DeclarationMap,
    plans: &[SurfacePlan<'_>],
    force: bool,
) -> anyhow::Result<Vec<String>> {
    // Build a filtered config containing only surfaces whose decision says
    // "Write" (or any decision, if `force` is set). `project_surfaces`
    // already implements GitHub sync, directory rendering, snapshot-safe
    // writes — we just hand it the surfaces we want written.
    let mut writable = SurfaceConfig {
        surface: Vec::new(),
    };
    for plan in plans {
        let include = if force {
            true
        } else {
            matches!(plan.decision, SurfaceDecision::Write)
        };
        if include {
            writable.surface.push(plan.surface.clone());
        }
    }

    cosmon_surface::project_surfaces(
        &writable,
        project_root,
        fleet,
        molecules,
        formulas,
        declarations,
    )
    .map_err(|e| anyhow::anyhow!("surface projection failed: {e}"))
}

/// Fetch the current GitHub-Issues state for every GitHub surface and
/// warn when it diverges from the local mirror. Pure side effect (writes
/// to stderr) — state is untouched.
fn fetch_github_remote_state(config: &SurfaceConfig, state_dir: &Path) {
    for surface in &config.surface {
        if surface.kind != cosmon_surface::SurfaceKind::GithubIssues {
            continue;
        }
        let repo = surface.repo.as_deref().unwrap_or("");
        if repo.is_empty() {
            continue;
        }
        let mirrors = cosmon_surface::github_mirror::load_all_mirrors(state_dir, repo);
        let mut fetched = 0;
        for (mol_id, mirror) in &mirrors {
            // Fetch current issue state from GitHub.
            let output = std::process::Command::new("gh")
                .args([
                    "issue",
                    "view",
                    &mirror.issue_number.to_string(),
                    "--repo",
                    repo,
                    "--json",
                    "title,body,state",
                ])
                .output();
            if let Ok(out) = output {
                if let Ok(issue) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    let remote_body = issue["body"].as_str().unwrap_or("");
                    let remote_hash = cosmon_surface::github_mirror::hash_content(remote_body);
                    let remote_state = issue["state"].as_str().unwrap_or("OPEN");
                    let state_str = if remote_state == "OPEN" {
                        "open"
                    } else {
                        "closed"
                    };

                    if remote_hash != mirror.body_hash || state_str != mirror.state {
                        eprintln!(
                            "  ⚠️  GitHub #{} ({mol_id}) was edited remotely!",
                            mirror.issue_number
                        );
                        if state_str != mirror.state {
                            eprintln!("       State: {} → {state_str}", mirror.state);
                        }
                        if remote_hash != mirror.body_hash {
                            eprintln!("       Body was modified on GitHub.");
                        }
                    }
                    fetched += 1;
                }
            }
        }
        if fetched > 0 {
            eprintln!("Fetched {fetched} GitHub Issues from {repo}");
        }
    }
}

/// Heal the `archived ⇒ status.is_terminal()` invariant on disk.
///
/// Scans every molecule; for each that is archived but carries a
/// non-terminal status, rewrites `status = Collapsed` (reason
/// `archived-but-alive heal`, cause `manual`) and appends a
/// `MoleculeStatusChanged` + `MoleculeCollapsed` event pair to the
/// fleet event log so the heal is durable across a cache rebuild
/// (the reducer projects both events back to `Collapsed`).
///
/// Returns the list of healed molecule ids (empty when the galaxy is
/// already coherent — the common, idempotent case). In `dry_run` mode
/// (from `--check`) the violations are reported but nothing is mutated.
///
/// # Errors
///
/// Returns an error if molecules cannot be listed. Per-molecule save
/// failures abort the pass (the operator must see a partial heal), but
/// event-emission failures are best-effort: a failed event append is
/// logged and the state write still stands (mirrors `cs collapse`).
fn heal_archived_terminal(
    ctx: &Context,
    store: &dyn StateStore,
    state_dir: &Path,
    dry_run: bool,
) -> anyhow::Result<Vec<String>> {
    use cosmon_core::event_v2::EventV2;
    use cosmon_core::molecule::{CollapseCause, MoleculeStatus};

    let molecules = store.list_molecules(&MoleculeFilter::default())?;
    let ghosts: Vec<cosmon_state::MoleculeData> = molecules
        .into_iter()
        .filter(|m| m.archived && !m.status.is_terminal())
        .collect();

    if ghosts.is_empty() {
        if !ctx.json {
            println!("Invariant heal: no archived-but-alive molecules (already coherent).");
        }
        return Ok(Vec::new());
    }

    let events_path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    let mut healed: Vec<String> = Vec::new();

    for mol in ghosts {
        let id = mol.id.clone();
        let prev_status = mol.status;

        if dry_run {
            if !ctx.json {
                println!("  would heal {} ({} → collapsed)", id.as_str(), prev_status);
            }
            healed.push(id.as_str().to_owned());
            continue;
        }

        let mut updated = mol;
        updated.status = MoleculeStatus::Collapsed;
        updated.collapse_reason = Some("archived-but-alive heal".to_owned());
        updated.collapse_cause = Some(CollapseCause::Manual);
        updated.collapsed_step = Some(updated.current_step);
        // A ghost may still carry a phantom inline worker pointer; drop
        // it on the terminal transition (mirrors `cs collapse`).
        if updated.process.is_some() {
            updated.release_process();
        }
        updated.updated_at = chrono::Utc::now();
        store.save_molecule(&id, &updated)?;

        // Durable event pair so the heal survives a cache rebuild.
        let status_seq = cosmon_state::event_log::emit_one(
            &events_path,
            EventV2::MoleculeStatusChanged {
                molecule_id: id.clone(),
                from: prev_status.to_string(),
                to: "collapsed".to_owned(),
            },
            None,
        )
        .ok();
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            EventV2::MoleculeCollapsed {
                molecule_id: id.clone(),
                reason: "archived-but-alive heal".to_owned(),
                kind: None,
            },
            status_seq,
        );

        healed.push(id.as_str().to_owned());
    }

    if ctx.json {
        let payload = serde_json::json!({
            "invariant_heal": {
                "archived_terminal": {
                    "dry_run": dry_run,
                    "healed": healed,
                }
            }
        });
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else if dry_run {
        println!(
            "Invariant heal (dry-run): {} row(s) would be healed.",
            healed.len()
        );
    } else {
        println!(
            "Invariant heal: rewrote {} archived-but-alive row(s) → collapsed:",
            healed.len()
        );
        for id in &healed {
            println!("  🩹 {id}");
        }
    }

    Ok(healed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ghost A (delib-20260704-b476 C4): `cs reconcile --check` flags a config
    /// whose `[adapters.<name>].default_model` is in that adapter's `strong`
    /// set, and passes a config that defaults to a non-strong model.
    #[test]
    fn ghost_a_flags_a_strong_config_default() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        std::fs::create_dir_all(&cosmon_dir).unwrap();
        let config_path = cosmon_dir.join("config.toml");

        // Violation: the default model IS a declared strong id.
        std::fs::write(
            &config_path,
            r#"
[project]
name = "demo"

[adapters.claude]
default_model = "claude-fable-5"
strong = ["claude-fable-5"]
"#,
        )
        .unwrap();
        let violations = check_no_strong_config_default(&cosmon_dir);
        assert_eq!(violations.len(), 1, "one strong default flagged");
        assert!(violations[0].contains("claude-fable-5"));

        // Clean: config downgrades to a non-strong model (allowed).
        std::fs::write(
            &config_path,
            r#"
[project]
name = "demo"

[adapters.claude]
default_model = "claude-sonnet-4-6"
strong = ["claude-fable-5"]
"#,
        )
        .unwrap();
        assert!(
            check_no_strong_config_default(&cosmon_dir).is_empty(),
            "a non-strong config default is allowed (config may downgrade)"
        );
    }

    /// A missing or config-less galaxy yields no Ghost-A violations — the
    /// lint is fail-open and never aborts on an absent config.
    #[test]
    fn ghost_a_is_silent_without_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        std::fs::create_dir_all(&cosmon_dir).unwrap();
        assert!(check_no_strong_config_default(&cosmon_dir).is_empty());
    }

    /// `load_formulas` parses every `*.formula.toml` in `.cosmon/formulas/`
    /// into a [`FormulaMap`] keyed by formula id, skips files with unrelated
    /// extensions, and silently drops malformed entries so a single bad
    /// file cannot break `cs reconcile`.
    #[test]
    fn test_load_formulas_parses_valid_and_skips_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let formulas_dir = cosmon_dir.join("formulas");
        std::fs::create_dir_all(&formulas_dir).unwrap();

        // Valid formula.
        let valid = r#"
formula = "task-work"
version = 1
description = "Test formula for plumbing"

[[steps]]
id = "implement"
title = "Implement"
description = "Do the work."
"#;
        std::fs::write(formulas_dir.join("task-work.formula.toml"), valid).unwrap();

        // Another valid formula.
        let valid2 = r#"
formula = "deep-think"
version = 1

[[steps]]
id = "think"
title = "Think"
description = "Reason carefully."
"#;
        std::fs::write(formulas_dir.join("deep-think.formula.toml"), valid2).unwrap();

        // Malformed formula — must be skipped silently, not panic.
        std::fs::write(
            formulas_dir.join("broken.formula.toml"),
            "this is not valid toml { { {",
        )
        .unwrap();

        // Unrelated file — must be ignored.
        std::fs::write(formulas_dir.join("README.md"), "docs").unwrap();

        // `.toml` that is not a formula — must be ignored (no `.formula.toml`).
        std::fs::write(formulas_dir.join("settings.toml"), "key = 1").unwrap();

        let map = load_formulas(&cosmon_dir);

        assert_eq!(map.len(), 2, "should load exactly the two valid formulas");
        assert!(map.contains_key(&cosmon_core::id::FormulaId::new("task-work").unwrap()));
        assert!(map.contains_key(&cosmon_core::id::FormulaId::new("deep-think").unwrap()));
        let task = map
            .get(&cosmon_core::id::FormulaId::new("task-work").unwrap())
            .unwrap();
        assert_eq!(task.description, "Test formula for plumbing");
        assert_eq!(task.steps.len(), 1);
        assert_eq!(task.steps[0].title, "Implement");
    }

    /// When the `formulas/` directory is missing (fresh project or
    /// minimally-configured repo), `load_formulas` must return an empty
    /// map rather than erroring — surface rendering is still expected to
    /// succeed, just without formula-derived enrichment.
    #[test]
    fn test_load_formulas_missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        // NB: do not create the formulas subdirectory.
        std::fs::create_dir_all(&cosmon_dir).unwrap();

        let map = load_formulas(&cosmon_dir);
        assert!(map.is_empty());
    }
}
