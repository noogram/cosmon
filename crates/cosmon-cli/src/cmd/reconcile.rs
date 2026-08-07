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

    // ADR-153 (C4) — the dual-witness roster lint. The sibling of the two
    // above, and the one that closes the gap they left: those check the
    // *config* that a committee could be built from, while this checks the
    // roster a committee actually declared. Until it existed, the witnesses in
    // `cosmon_core::committee` had ZERO production callers — every predicate
    // passed its own unit tests while nothing consulted it, so witness (1),
    // witness (2) and the diversity floor were enforced only by a worker
    // reading prose, and a roster that failed one was contradicted by nothing.
    // Same fail-closed-under-`--check` contract as the two lints above.
    let roster = check_committee_roster_witnesses(&state_dir, &cosmon_dir);
    // Findings on committees that already finished. Printed in full — a
    // historical violation is still a violation and nobody should have to grep
    // for it — but never used to fail the gate: no action on any current work
    // could clear them, and a refusal a human cannot act on is an outage.
    // Measured 2026-07-28: one `completed` committee held `cs reconcile
    // --check` red permanently on this repository.
    if !roster.historical.is_empty() && !ctx.json {
        eprintln!(
            "cs reconcile: committee roster — HISTORICAL findings on molecules \
             that already reached a terminal state. Reported, not refused: the \
             committee is over and no current work can clear them."
        );
        for v in &roster.historical {
            eprintln!("  · {v}");
        }
    }
    // Legal-but-loud. A roster seating a non-floor-bearing reader, or resting
    // its floor on one seat, is fragile and not invalid — refusing it forbade
    // the roster the doctrine itself prescribes (measured 2026-07-28: the
    // prescribed roster had no admissible representation, on the ballot or off
    // it). Printed always; it decides nothing.
    if !roster.advisories.is_empty() && !ctx.json {
        eprintln!(
            "cs reconcile: committee roster — ADVISORIES. True, legal, and \
             load-bearing at fold time: reported, never refused."
        );
        for a in &roster.advisories {
            eprintln!("  ! {a}");
        }
    }
    let roster_violations = roster.violations;
    if !roster_violations.is_empty()
        || (ctx.json && !(roster.historical.is_empty() && roster.advisories.is_empty()))
    {
        if ctx.json {
            let output = serde_json::json!({
                "status": "committee_roster_witness",
                "violations": roster_violations,
                "historical": roster.historical,
                "advisories": roster.advisories,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if !roster_violations.is_empty() {
            eprintln!(
                "cs reconcile: committee roster violation — a declared roster \
                 fails the dual conjunctive witness (ADR-153):"
            );
            for v in &roster_violations {
                eprintln!("  ✗ {v}");
            }
            eprintln!(
                "\nA seat sits only if BOTH witnesses pass: (1) its RESOLVED endpoint \
                 tuple differs from the generator's and every peer's, and (2) it plays \
                 a distinct role_id, carries a really-injected adversarial briefing, \
                 and ships a falsification-attempt artefact. A witness-rejected seat \
                 is not a low score to outweigh — it is not on the ballot. Widen the \
                 roster, point the colliding seats at distinct providers, or deliver \
                 the missing contract/artefact, then re-run."
            );
        }
        // Only a LIVE violation fails the gate. The historical lines rode
        // along in the JSON so a reader sees the whole picture; they must not
        // decide the exit status.
        if args.check && !roster_violations.is_empty() {
            std::process::exit(1);
        }
    }

    // The verdict-door polarity lint — the sibling of the roster lint one layer
    // downstream. The roster lint checks WHO sat; this one checks whether what
    // they emitted can be READ. Same fail-closed-under-`--check` contract and
    // the same live/historical split, for the same reason.
    let polarity = check_seat_verdict_polarity(&state_dir);
    if !polarity.historical.is_empty() && !ctx.json {
        eprintln!(
            "cs reconcile: seat verdict polarity — HISTORICAL findings on molecules \
             that already reached a terminal state. Reported, not refused."
        );
        for v in &polarity.historical {
            eprintln!("  · {v}");
        }
    }
    if !polarity.violations.is_empty() || (ctx.json && !polarity.historical.is_empty()) {
        if ctx.json {
            let output = serde_json::json!({
                "status": "seat_verdict_polarity",
                "violations": polarity.violations,
                "historical": polarity.historical,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if !polarity.violations.is_empty() {
            eprintln!(
                "cs reconcile: seat verdict polarity violation — a verdict written \
                 in the RELATIVE cmb-verify door that no reader can map:"
            );
            for v in &polarity.violations {
                eprintln!("  ✗ {v}");
            }
            eprintln!(
                "\n`confirmed` does not mean \"good\" — it means THE STATED MECHANISM \
                 HOLDS, and whether that is good news depends on what the mechanism \
                 claimed. The definition is `.cosmon/formulas/cmb-verify.formula.toml`, \
                 step `verify-or-refute`; the mapping in code is \
                 `cosmon_core::committee::map_through_polarity`. State the polarity — \
                 never assume the one that makes the round pass."
            );
        }
        if args.check && !polarity.violations.is_empty() {
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

/// Scan every molecule directory for a declared committee roster
/// (`roster.json`) and return one message per **witness violation** — the
/// `cs reconcile --check` gate that a witness-failing roster is REFUSED by the
/// tool rather than merely discouraged by a recipe.
///
/// # Why this lint exists at all
///
/// `cosmon_core::committee` decides who may sit on a cross-provider jury, and
/// for a while nothing asked it. Verified by grep on 2026-07-28:
/// [`plan_committee`](cosmon_core::committee::plan_committee),
/// [`committee_requirement`](cosmon_core::committee::committee_requirement),
/// [`fold_committee`](cosmon_core::committee::fold_committee),
/// [`jury_integrity`](cosmon_core::committee::jury_integrity),
/// [`sor_may_not_resurrect`](cosmon_core::committee::sor_may_not_resurrect)
/// and
/// [`RosterPlan::floor_bearing_seats`](cosmon_core::committee::RosterPlan::floor_bearing_seats)
/// had **no production callers** — the only committee references outside the
/// module were the posture-injection plumbing in `cs evolve`, not the decision
/// kernel. Every predicate passed its own tests while changing nothing, so a
/// worker that skipped the check, or resolved an endpoint tuple wrongly,
/// produced a roster no gate contradicted. This is the boundary that
/// contradicts it.
///
/// # The roster is measured on RESOLVED tuples, not declared ones
///
/// `roster.json` is written by the convener, so every field in it is a claim.
/// Planning those claims directly answers "is this file internally
/// consistent?", which is the property next to the one that matters: a roster
/// could name two families it does not have and pass. Each seat therefore
/// names the `[adapters.<name>]` section it sits on, and
/// [`RosterSpec::resolved`](cosmon_core::committee::RosterSpec::resolved)
/// re-derives its endpoint tuple from that section's `base_url` + `model`
/// before anything is planned. A declaration that does not survive the
/// derivation is a violation; so is a seat that names no adapter, because an
/// unresolvable claim is one the gate did not check.
///
/// # Absence of a roster is not exemption
///
/// A molecule with no `roster.json` is not automatically "not a committee" —
/// that reading makes the whole gate opt-in by artefact presence, so a convener
/// who simply never writes the file is never inspected. Two shapes are
/// therefore refused on their own:
///
/// - a molecule carrying the durable `roster.md` prose but no machine-readable
///   `roster.json` — a committee described to humans and to no gate;
/// - a molecule carrying `committee-posture.md` (proof it was *seated* as a
///   cross-provider seat) whose id appears on **no** roster in the tree — a
///   seat nobody rostered, whose witnesses were therefore never counted.
///
/// A molecule with none of the three artefacts really is not a committee, and
/// that is the honest remaining scope: the gate cannot refuse what leaves no
/// trace anywhere, and says so here rather than implying otherwise.
///
/// An unreadable or malformed roster is reported as a violation rather than
/// ignored: a roster the gate cannot parse is a roster the gate did not check.
/// A missing/unreadable project config falls back to the `[provider_bias]`
/// default — the floor then comes from the stake alone, which is the
/// conservative reading.
/// Whether a molecule's story is still open, read from its own `state.json`.
///
/// # Why the two presence checks are scoped to live work
///
/// A roster governs a committee that is about to be *used*. A committee that
/// finished last week cannot grow a `roster.json` retroactively — `roster.json`
/// did not exist when it ran — so refusing it on every future invocation makes
/// the gate permanently red over history nobody can change. That is an outage
/// wearing a control's clothes: a refusal has to be one a human can act on.
///
/// This does not reopen the opt-in hole. A live convener is inspected on every
/// run — and now inspected on what it IS (its `formula_id`) rather than on what
/// it chose to write — and the only way out is to reach a terminal state, which
/// is exactly the point at which the roster stops governing anything.
///
/// # Why the CONTENTS check is scoped to live work too
///
/// It used to say "terminality excuses the absence of a roster, never its
/// contents", and that sentence turned one finished committee into a permanent
/// outage. Measured 2026-07-28 on this repository: `committee-20260728-f744`
/// is `completed`, its seat `cmbverify-20260728-6178` was never tackled so its
/// briefing carries no posture pointer, and `cs reconcile --check` was red
/// **forever** — no action on any current work could return it to green, and
/// the seat cannot be tackled because the committee is over.
///
/// A refusal nobody can act on is not a control. So a terminal molecule's
/// roster contents are reported as a HISTORICAL advisory — printed in full,
/// never silently dropped — and do not fail the gate. The evasion this admits
/// is real and bounded: a convener could reach a terminal state to stop being
/// refused. It costs a recorded, irreversible lifecycle transition in the event
/// log, and it ends the committee it was trying to convene. That is a trade
/// against an unconditional outage, and it is the same trade the absence check
/// already makes.
///
/// Unreadable or absent state is treated as live — the conservative reading,
/// since the alternative is exempting a molecule because its state file could
/// not be parsed.
fn molecule_is_live(mol_dir: &Path) -> bool {
    read_state_field(mol_dir, "status")
        .is_none_or(|s| !matches!(s.as_str(), "done" | "failed" | "collapsed" | "completed"))
}

/// One string field of a molecule's own `state.json`, or `None` when the file
/// is absent, unparseable, or carries no such field.
///
/// The state file is written by `cs nucleate` before any worker runs and is not
/// an artefact a convener chooses to produce — which is precisely why the
/// committee gate reads `formula_id` from here rather than inferring
/// committee-hood from files on disk.
fn read_state_field(mol_dir: &Path, field: &str) -> Option<String> {
    let raw = std::fs::read_to_string(mol_dir.join("state.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Whether this molecule's recorded formula is one that convenes a committee —
/// the RESOLVED answer to "is this a committee?", which no artefact can opt out
/// of.
fn molecule_convenes_a_committee(mol_dir: &Path) -> bool {
    read_state_field(mol_dir, "formula_id")
        .is_some_and(|f| cosmon_core::committee::CONVENING_FORMULA_IDS.contains(&f.as_str()))
}

/// The verdict-door polarity lint: **the reader that makes
/// `mechanism_polarity` load-bearing.**
///
/// The converge contract instructs that a seat's `verdict.json` carrying the
/// relative cmb-verify door without a `mechanism_polarity` is NOT-CLEAN, and
/// that an inconsistent `(polarity, verdict, VERDICT:)` triple is NOT-CLEAN
/// too. Until this function existed that instruction was a **declaration about
/// seats that nothing resolved**: no code read the field, so the mitigating
/// sentence "a seat convened by this loop is always `polarity: fix`" could be
/// false — or the field simply absent — and every gate stayed green. The test
/// that governs this lineage is *can the gate still pass when the constrained
/// party lies, or is simply absent?*, and the answer was yes.
///
/// Scope is decided by the VERDICT'S OWN VOCABULARY, never by the molecule's
/// kind: only a `verdict.json` whose `verdict` field parses as
/// [`SeatVerdict`](cosmon_core::committee::SeatVerdict) — `confirmed` /
/// `refuted` / `inconclusive` — is subject to the polarity rule, because only
/// that door is relative. A gate verdict written `PASS` / `BLOCKED` / `CLEAN`
/// is absolute and needs no polarity; demanding one there would be noise, and a
/// lint that cries on artefacts it does not govern is one people learn to
/// ignore.
///
/// Same live/historical split as [`check_committee_roster_witnesses`]: a
/// terminal molecule's verdict cannot be corrected by any current work, so it
/// is reported and never fails the gate.
fn check_seat_verdict_polarity(state_dir: &Path) -> RosterFindings {
    let mut out = RosterFindings::default();
    let Ok(fleets) = std::fs::read_dir(state_dir.join("fleets")) else {
        return out;
    };
    let mut fleet_dirs: Vec<PathBuf> = fleets.filter_map(Result::ok).map(|e| e.path()).collect();
    // Deterministic order so two runs on one tree report identically.
    fleet_dirs.sort();
    for fleet in fleet_dirs {
        let Ok(molecules) = std::fs::read_dir(fleet.join("molecules")) else {
            continue;
        };
        let mut mol_dirs: Vec<PathBuf> =
            molecules.filter_map(Result::ok).map(|e| e.path()).collect();
        mol_dirs.sort();
        for mol_dir in mol_dirs {
            inspect_seat_verdict(&mol_dir, &mut out);
        }
    }
    out
}

/// Judge ONE molecule's verdict emission and file every finding on the side its
/// liveness puts it.
///
/// Split out of [`check_seat_verdict_polarity`] for the same reason
/// [`inspect_roster`] was split out of its enumerator: walking the tree and
/// reading one seat are separate readings, and after absence stopped being a
/// silent `continue` the two no longer fit in one screen.
fn inspect_seat_verdict(mol_dir: &Path, out: &mut RosterFindings) {
    use cosmon_core::committee::{ConvergeVerdict, MechanismPolarity, SeatVerdict};

    let verdict_path = mol_dir.join(SEAT_VERDICT_FILE);
    let raw = std::fs::read_to_string(&verdict_path).ok();
    // Scope, and it is the whole reason absence can be judged at all.
    // Most molecules in a tree are not seats and owe no verdict; a
    // molecule that carries the durable adversarial contract, or that
    // has already written a referee report, IS one and owes both files.
    let owes_a_verdict = molecule_owes_a_seat_verdict(mol_dir);
    if raw.is_none() && !owes_a_verdict {
        return;
    }
    let label = mol_dir.file_name().map_or_else(
        || verdict_path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let live = molecule_is_live(mol_dir);
    let Some(raw) = raw else {
        // ABSENT, not malformed — and until 2026-07-28 this exited
        // through a bare `continue` while the malformed case was
        // recorded. That asymmetry WAS the bug: the contract's central
        // rule is that a missing verdict is NOT-CLEAN, and the enum
        // advertised a `NoVerdict` variant that no caller ever
        // constructed, so the rule had no code enforcement while the
        // type said it had.
        out.record(
            live,
            cosmon_core::committee::SeatReadingRefusal::NoVerdict.explain(&label),
        );
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        out.record(
            live,
            format!(
                "{label}: `{SEAT_VERDICT_FILE}` exists and is not valid JSON, so no \
                 gate can read the verdict it claims to carry. A verdict a reader \
                 cannot parse is NOT-CLEAN, never a pass"
            ),
        );
        return;
    };
    // Which door did the seat speak? The RELATIVE one is what the
    // polarity rule scopes to, and it is identified by what the file
    // says rather than by what the molecule is. The ABSOLUTE one needs
    // no polarity — but a file carrying NEITHER carries no verdict at
    // all, which is the second `continue` that used to launder absence
    // into silence.
    let spoken = json.get("verdict").and_then(serde_json::Value::as_str);
    let door = spoken.and_then(SeatVerdict::parse);
    let absolute = spoken.and_then(ConvergeVerdict::parse);
    if door.is_none() && absolute.is_none() {
        out.record(
            live,
            cosmon_core::committee::SeatReadingRefusal::NoVerdict.explain(&label),
        );
        return;
    }
    let Some(door) = door else {
        inspect_absolute_door(mol_dir, &label, live, absolute, out);
        return;
    };
    let polarity = json
        .get("mechanism_polarity")
        .and_then(serde_json::Value::as_str);
    let Some(polarity) = polarity.and_then(MechanismPolarity::parse) else {
        out.record(
            live,
            match polarity {
                Some(bad) => format!(
                    "{label}: `{SEAT_VERDICT_FILE}` declares \
                 `mechanism_polarity: \"{bad}\"`, which is neither `defect` nor \
                 `fix`. The `{}` door is unreadable without a polarity this \
                 reader recognises, and an unrecognised value may not be \
                 guessed into one",
                    door.label(),
                ),
                None => format!(
                    "{label}: `{SEAT_VERDICT_FILE}` declares `verdict: \"{}\"` — the \
                 RELATIVE cmb-verify door — with no `mechanism_polarity` field. \
                 `confirmed` means the stated mechanism HOLDS, which is CLEAN \
                 when the mechanism claimed a fix and FINDINGS when it claimed a \
                 defect, so this verdict has no reading at all. Add \
                 `\"mechanism_polarity\": \"defect\"|\"fix\"` — fail closed \
                 rather than assume the polarity that makes the round pass",
                    door.label(),
                ),
            },
        );
        return;
    };
    // Both doors present: refuse the agreeing-but-wrong pair. And an
    // ABSENT report is refused rather than skipped — the contract asks
    // for an affirmative verdict in BOTH files, so one file is one
    // file, and the third bare `continue` was the third way absence
    // passed for a pass.
    let Some(reported) = read_reported_verdict(mol_dir) else {
        out.record(
            live,
            cosmon_core::committee::SeatReadingRefusal::NoReport.explain(&label),
        );
        return;
    };
    let emission = cosmon_core::committee::SeatEmission {
        seat_id: label,
        mechanism_polarity: Some(polarity),
        verdict: Some(door),
        reported: Some(reported),
    };
    if let Err(refusal) = cosmon_core::committee::read_seat_emission(&emission) {
        out.record(live, refusal.explain(&emission.seat_id));
    }
}

/// The branch where `verdict.json` spoke the ABSOLUTE vocabulary
/// (`CLEAN` / `FINDINGS` / `INCONCLUSIVE`).
///
/// No polarity is owed — the meaning of those words does not depend on what the
/// stated mechanism claimed — but the report still is, and the two files must
/// not contradict each other. Two files agreeing is not two files being right;
/// two files disagreeing is one of them being wrong, and neither may be picked
/// by the reader.
fn inspect_absolute_door(
    mol_dir: &Path,
    label: &str,
    live: bool,
    absolute: Option<cosmon_core::committee::ConvergeVerdict>,
    out: &mut RosterFindings,
) {
    use cosmon_core::committee::SeatReadingRefusal;

    match read_reported_verdict(mol_dir) {
        None => out.record(live, SeatReadingRefusal::NoReport.explain(label)),
        Some(reported) if Some(reported) != absolute => out.record(
            live,
            SeatReadingRefusal::Incoherent {
                implied: absolute.unwrap_or(reported),
                reported,
            }
            .explain(label),
        ),
        Some(_) => {}
    }
}

/// Whether this molecule owes the two-file verdict emission at all — the scope
/// that lets an ABSENT `verdict.json` be judged instead of skipped.
///
/// # Why absence needs a scope and malformity does not
///
/// A `verdict.json` that exists names its own subject: whatever wrote it meant
/// to emit a verdict, so an unreadable one is refusable wherever it is found.
/// Absence is different — most molecules in a tree are not seats and owe
/// nothing, so "no verdict.json" is the normal state of almost everything and
/// refusing it everywhere would be an outage, not a control.
///
/// Three facts put a molecule in scope, and the FIRST is the one no seat can
/// decline:
///
/// 1. its recorded `formula_id` is a seat formula
///    ([`cosmon_core::committee::SEAT_FORMULA_IDS`]) — written by `cs nucleate`
///    before any worker ran, so it says what the molecule IS rather than what
///    its author chose to write;
/// 2. it carries the durable `committee-posture.md`. That file is written by
///    the **convening driver**, and the driver is a *worker*, not a code path:
///    the committee formula's convene step
///    (`.cosmon/formulas/cross-provider-committee.formula.toml`) instructs the
///    LLM executing it to write the contract into each seat's own molecule
///    directory, in the shape `render_committee_posture` defines. No `cs` verb
///    authors it. What the verbs do is narrower — `cs tackle`, `cs evolve` and
///    `cs complete` re-establish the *pointer* to the file in a `briefing.md`
///    they have just written
///    ([`deliver_committee_posture_reference`](super::evolve::deliver_committee_posture_reference)),
///    and that function returns early when the file is absent — so none of them
///    can create it, and its presence attests the convening, not the dispatch.
///    That last property is asserted rather than asserted-about:
///    `tackle_does_not_author_the_posture_file_it_points_at` and
///    `complete_does_not_author_the_posture_file_it_points_at` in
///    `tests/committee_seat_dispatch.rs` both write the file and use `tackle` to
///    land the pointer before deleting it. The tackle-side test then runs
///    `tackle` again; the complete-side test runs `complete` once. Each checks
///    that its final verb does not bring the file back. Note the asymmetry with
///    witness (2)'s own delivery leg, which no longer accepts mere presence:
///    SCOPE is decided by the file being there at all (a stub still makes a
///    molecule answerable), while DELIVERY is decided by
///    [`RosterSpec::with_observed_delivery`](cosmon_core::committee::RosterSpec::with_observed_delivery)
///    reading the contract and matching it against the roster's declaration. A
///    stub therefore puts a molecule in scope and fails its witness, which is
///    the order that leaves nothing unexamined; or
/// 3. it already wrote a `referee-report.md` — it spoke as a seat in one file,
///    and the contract asks for both. Without this door a seat could leave the
///    machine-readable half off and be out of scope *for having done so*, which
///    is opt-out by omission.
///
/// None is a flag a seat sets on itself, which is what keeps this from being
/// the opt-in hole one layer along. A seat still in flight is in scope from its
/// first step on purpose: the converge contract requires both files written on
/// step 1, precisely so a provider refusal or a sleeping machine cannot take
/// the account down with it.
fn molecule_owes_a_seat_verdict(mol_dir: &Path) -> bool {
    read_state_field(mol_dir, "formula_id")
        .is_some_and(|f| cosmon_core::committee::SEAT_FORMULA_IDS.contains(&f.as_str()))
        || mol_dir
            .join(cosmon_core::committee::COMMITTEE_POSTURE_FILE)
            .exists()
        || mol_dir.join(SEAT_REPORT_FILE).exists()
}

/// The absolute verdict on the first non-empty line of `referee-report.md`, or
/// `None` when the file is absent, unreadable, or carries no `VERDICT:` line.
///
/// The three collapse into one answer on purpose: each is *no affirmative
/// verdict in that file*, and the caller refuses all three identically.
fn read_reported_verdict(mol_dir: &Path) -> Option<cosmon_core::committee::ConvergeVerdict> {
    std::fs::read_to_string(mol_dir.join(SEAT_REPORT_FILE))
        .ok()
        .and_then(|r| {
            r.lines()
                .find(|l| !l.trim().is_empty())
                .and_then(cosmon_core::committee::ConvergeVerdict::from_report_line)
        })
}

/// A seat's machine-readable verdict sidecar, beside its human-readable report.
const SEAT_VERDICT_FILE: &str = "verdict.json";

/// A seat's human-readable referee report, whose FIRST non-empty line carries
/// `VERDICT: CLEAN | FINDINGS (N) | INCONCLUSIVE`.
const SEAT_REPORT_FILE: &str = "referee-report.md";

/// What the roster lint found: refusals that fail `--check`, and historical
/// lines that are printed and do not.
#[derive(Default)]
struct RosterFindings {
    /// Violations on LIVE work. A human can act on every one of these.
    violations: Vec<String>,
    /// Violations on molecules that already reached a terminal state. Reported
    /// so the history stays visible, never used to fail the gate — nothing any
    /// current work can do would clear them.
    historical: Vec<String>,
    /// True statements about rosters that are nonetheless LEGAL — the
    /// non-floor-bearing readers and the single-point-of-failure floors that
    /// [`cosmon_core::committee::RosterReport`] separates from its refusals.
    ///
    /// Printed in full and never used to fail the gate. They exist because the
    /// alternative to reporting them was refusing them, and refusing them
    /// forbade the roster the doctrine itself prescribes.
    advisories: Vec<String>,
}

impl RosterFindings {
    /// File one finding on the side its molecule's liveness puts it: a refusal
    /// while the committee can still be fixed, a historical note once it
    /// cannot.
    fn record(&mut self, live: bool, finding: String) {
        if live {
            self.violations.push(finding);
        } else {
            self.historical.push(finding);
        }
    }
}

/// Read one molecule's `roster.json`, re-derive both witnesses from disk, and
/// file every finding on the side `live` puts it.
///
/// Split out of [`check_committee_roster_witnesses`] so the enumeration of the
/// tree and the judgement of one roster are readable separately; every seat id
/// the roster claims is recorded in `rostered`, which the caller's second pass
/// consults to tell an unrostered seat from a rostered one.
#[allow(clippy::too_many_arguments)]
fn inspect_roster(
    mol_dir: &Path,
    roster_path: &Path,
    label: &str,
    live: bool,
    bias: &cosmon_core::config::ProviderBiasConfig,
    adapters: Option<&cosmon_core::config::AdaptersConfig>,
    rostered: &mut std::collections::BTreeSet<String>,
    out: &mut RosterFindings,
) {
    let spec: cosmon_core::committee::RosterSpec = match std::fs::read_to_string(roster_path)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
    {
        Ok(spec) => spec,
        Err(e) => {
            out.record(
                live,
                format!(
                    "{label}: its {} could not be read as a roster ({e}) — an \
                     unparseable roster is an UNCHECKED roster, so it is refused \
                     rather than skipped",
                    cosmon_core::committee::COMMITTEE_ROSTER_FILE,
                ),
            );
            return;
        }
    };
    rostered.insert(spec.generator.seat_id.clone());
    rostered.extend(spec.refuters.iter().map(|s| s.seat_id.clone()));

    // Witness (2)'s `injected` flag is re-derived from the seats' own
    // directories, so a roster cannot certify its own delivery any more than it
    // can certify its own family. `mol_dir`'s parent is the `molecules/`
    // directory every seat lives under.
    let molecules_root = mol_dir.parent().map(Path::to_path_buf);
    let (spec, mut violations) = spec.with_observed_delivery(|seat_id| {
        let seat_dir = molecules_root.as_ref()?.join(seat_id);
        if !seat_dir.is_dir() {
            // No directory at all. `None` does NOT mean "accept the claim" —
            // the core reports a seat that claims delivery here, because two
            // files cannot exist inside a directory that does not.
            return None;
        }
        // The contract is READ, not counted. A file that merely occupies the
        // path — `# posture\n`, a truncated copy, a stub — parses to `None` and
        // fails delivery, where a presence-only witness certified it.
        let posture_path = seat_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE);
        let posture_text = std::fs::read_to_string(&posture_path).ok();
        let pointer = std::fs::read_to_string(seat_dir.join("briefing.md"))
            .is_ok_and(|b| b.contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE));
        Some(cosmon_core::committee::ObservedDelivery {
            posture_file_exists: posture_path.exists(),
            posture: posture_text
                .as_deref()
                .and_then(cosmon_core::committee::parse_committee_posture),
            pointer,
        })
    });
    let report = spec.report(bias, adapters);
    violations.extend(report.refusals);
    // The membership check (F7). A collapsed seat is answered from its own
    // `state.json`, in the same `molecules/` directory the delivery witness
    // above reads — the core stays I/O-free and asks for the answer.
    violations.extend(spec.reconvocation_violations(&|seat_id: &str| {
        molecules_root.as_ref().is_some_and(|root| {
            read_state_field(&root.join(seat_id), "status")
                .is_some_and(|s| matches!(s.as_str(), "collapsed" | "failed"))
        })
    }));
    for v in violations {
        out.record(live, format!("{label}: {v}"));
    }
    // Advisories ride on the roster's liveness for their prefix but never for
    // their weight: they are printed whatever the molecule's state, because a
    // brittle floor is as worth knowing on a finished committee as on a live
    // one, and neither may decide an exit status.
    for a in report.advisories {
        out.advisories.push(format!("{label}: {a}"));
    }
}

fn check_committee_roster_witnesses(state_dir: &Path, cosmon_dir: &Path) -> RosterFindings {
    // A config the gate could not read is a config it did not check — the same
    // sentence an unparseable `roster.json` already earns four screens below,
    // and there is no reason the roster's inputs deserve a quieter rule than
    // the roster. `.ok()` swallowed it: the lint then ran on `[provider_bias]`
    // defaults and an EMPTY adapter inventory, silently measuring a floor
    // nobody configured against sections it could not see.
    //
    // An ABSENT config is not this case — `load_project_config` returns the
    // default for a path that does not exist — so the only way here is a file
    // that exists and cannot be parsed, which is always a human's typo and
    // always actionable. Reported alone, and the scan stops: with no inventory
    // every seat would also trip the missing-`[adapters.…]` refusal, burying
    // the one line that names the true cause under a screen of consequences.
    let cfg = match cosmon_filestore::load_project_config(&cosmon_dir.join("config.toml")) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            return RosterFindings {
                violations: vec![format!(
                    "{}: it could not be read as a project config ({e}) — the \
                     roster gate resolves every seat's family against \
                     `[adapters]` and counts its floor from `[provider_bias]`, \
                     so a config it cannot parse is a committee it did not \
                     check. Fix the TOML and re-run",
                    cosmon_dir.join("config.toml").display(),
                )],
                historical: Vec::new(),
                advisories: Vec::new(),
            };
        }
    };
    let bias = cfg
        .as_ref()
        .map(|c| c.provider_bias.clone())
        .unwrap_or_default();
    let adapters = cfg.as_ref().and_then(|c| c.adapters.as_ref());
    // Every seat id any roster in the tree claims, so an unrostered seat can be
    // told from a rostered one. Filled on the first pass, consulted on the
    // second — a seat's roster lives in its CONVENER's directory, never its own.
    let mut rostered: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Molecules that carry `committee-posture.md`, i.e. were seated.
    let mut seated: Vec<String> = Vec::new();

    let mut out = RosterFindings::default();
    // `<state_dir>/fleets/<fleet>/molecules/<id>/roster.json`. Enumerated
    // rather than globbed so a fleet or molecule dir that cannot be read is
    // skipped without aborting the whole lint.
    let Ok(fleets) = std::fs::read_dir(state_dir.join("fleets")) else {
        return out;
    };
    let mut fleet_dirs: Vec<PathBuf> = fleets.filter_map(Result::ok).map(|e| e.path()).collect();
    fleet_dirs.sort();
    for fleet in fleet_dirs {
        let Ok(molecules) = std::fs::read_dir(fleet.join("molecules")) else {
            continue;
        };
        let mut mol_dirs: Vec<PathBuf> =
            molecules.filter_map(Result::ok).map(|e| e.path()).collect();
        // Deterministic order so two runs on one tree report identically.
        mol_dirs.sort();
        for mol_dir in mol_dirs {
            let roster_path = mol_dir.join(cosmon_core::committee::COMMITTEE_ROSTER_FILE);
            let label = mol_dir.file_name().map_or_else(
                || roster_path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let live = molecule_is_live(&mol_dir);
            if live
                && mol_dir
                    .join(cosmon_core::committee::COMMITTEE_POSTURE_FILE)
                    .exists()
            {
                seated.push(label.clone());
            }
            if !roster_path.exists() {
                // Not a roster — but silence here is what makes the gate
                // opt-in. Three shapes are refused on their own, and the FIRST
                // is the one no convener can decline: what the molecule IS,
                // read from the `formula_id` `cs nucleate` recorded before any
                // worker ran. The other two rest on artefacts an author chose
                // to write, which is why they were never the whole answer.
                if live && molecule_convenes_a_committee(&mol_dir) {
                    out.violations.push(format!(
                        "{label}: its formula is a committee convener and it has no \
                         `{}`. This one is not opt-out: the gate reads `formula_id` \
                         from the molecule's own state, so a convener that writes no \
                         artefact at all is still inspected. Write the roster before \
                         handing off",
                        cosmon_core::committee::COMMITTEE_ROSTER_FILE,
                    ));
                } else if live && mol_dir.join("roster.md").exists() {
                    out.violations.push(format!(
                        "{label}: it carries the prose `roster.md` but no `{}`, so its \
                         committee is described to humans and to NO gate. A gate cannot \
                         refuse prose — write the machine-readable roster beside it",
                        cosmon_core::committee::COMMITTEE_ROSTER_FILE,
                    ));
                }
                continue;
            }
            inspect_roster(
                &mol_dir,
                &roster_path,
                &label,
                live,
                &bias,
                adapters,
                &mut rostered,
                &mut out,
            );
        }
    }

    // Second pass. A molecule that was SEATED — it carries the durable
    // adversarial contract the convening driver wrote into a cross-provider
    // seat — and appears on no roster in this tree is a seat whose two
    // witnesses were never counted by anything. That is the same opt-in shape
    // as a missing roster, arriving from the other end.
    for label in seated {
        if !rostered.contains(&label) {
            out.violations.push(format!(
                "{label}: it carries the durable `{}` — it was seated as a \
                 cross-provider seat — but its id appears on NO `{}` in this tree, \
                 so neither of its witnesses was ever counted. A seat nobody \
                 rostered is not an exempt seat, it is an unexamined one",
                cosmon_core::committee::COMMITTEE_POSTURE_FILE,
                cosmon_core::committee::COMMITTEE_ROSTER_FILE,
            ));
        }
    }
    out
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

        let Some(new_content) =
            render_for_surface(surface, project_root, fleet, molecules, formulas)
        else {
            continue;
        };

        let target = surface_target(project_root, surface);
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
///
/// `project.decisions` is here and not only in `project_surfaces` because
/// this function is the classification loop's whole view of a surface: a
/// referent it cannot render is dropped from the plan, and a surface absent
/// from the plan is never handed to `project_surfaces` at all. That is how
/// `docs/adr/INDEX.md` came to declare itself auto-generated while no
/// command on any path regenerated it.
fn render_for_surface(
    surface: &cosmon_surface::Surface,
    project_root: &Path,
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
        "project.decisions" if surface.kind == cosmon_surface::SurfaceKind::Directory => Some(
            cosmon_surface::render_adr_index_content(project_root, &surface.path, surface.branding),
        ),
        _ => None,
    }
}

/// The on-disk file a surface's content lives in.
///
/// For a markdown surface that is the declared path. For a directory
/// surface it is `INDEX.md` *inside* the directory — the same target
/// `project_surfaces` writes. Joining the declared path directly would
/// hand the classifier a directory to `read_to_string`, which fails, so
/// every run would read the current content as empty and re-classify a
/// clean index as a create.
fn surface_target(project_root: &Path, surface: &cosmon_surface::Surface) -> std::path::PathBuf {
    let target = project_root.join(&surface.path);
    if surface.kind == cosmon_surface::SurfaceKind::Directory {
        target.join("INDEX.md")
    } else {
        target
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
        let Some(new_content) =
            render_for_surface(surface, project_root, fleet, molecules, formulas)
        else {
            continue;
        };
        let target = surface_target(project_root, surface);
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
