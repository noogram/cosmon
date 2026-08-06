// SPDX-License-Identifier: AGPL-3.0-only

//! `cs spore` — the verb family that germinates a whole polymer from a
//! shareable `spore.toml` template (ADR-140 N5).
//!
//! A **spore** is a parameterizable mission plan: a fleet + per-node
//! formulas + a `ParamSchema` + a DAG of typed edges + an optional `.tla`
//! seal. This module is the **shell** over the pure core: it loads the
//! manifest, coerces `--var k=v` strings into the declared TOML types,
//! gates on the seal (D4), and replays the
//! [`expand`](fn@cosmon_core::spore::expand)ed call list against the live
//! state store via the canonical `cs nucleate` persistence path. It is a
//! declarative front end over an existing verb, not a new scheduler and
//! not a new molecule type.
//!
//! Three sibling verbs, one role each:
//!
//! - **`cs spore validate`** — parse (N2) + expand (N3) as a dry run. Prints
//!   the ordered `cs nucleate ... --blocked-by ...` list; germinates nothing.
//! - **`cs spore run`** — parse + expand + seal gate (N4) then germinate the
//!   real polymer against the state store. `--json` emits NDJSON, one object
//!   per germinated molecule (agent-first invariant).
//! - **`cs spore export`** — the share-time emit verb: always writes a
//!   content-addressed bundle id over the manifest + its referenced recipes,
//!   plus an ASTRA-compatible `ro-crate-metadata.json` descriptive layer
//!   (ADR-140 D6). The `[spore.astra]` stanza only customizes the output
//!   path; its absence is not an opt-out.
//!
//! ## The seal gate (ADR-140 D4), stated honestly
//!
//! `cs spore run` never claims a seal is verified when it is not. A spore
//! with no `[spore.seal]` germinates freely (`seal: none`). A *sealed*
//! spore cannot be proven on a machine without the TLC verifier wired in,
//! so the default is **fail-closed**: it refuses unless the operator opts
//! into the risk with `--allow-unchecked-seal`, in which case the status
//! line reads `seal: present, NOT verified` — never `verified`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cosmon_core::id::{FleetId, MoleculeId};
use cosmon_core::nucleate::NucleateResult;
use cosmon_core::spore::{expand, NodeKind, NucleateCall, ParamType, Spore};
use cosmon_core::tag::Tag;

use super::nucleate::{load_formula_at_path, nucleate_for_spore, SporeNucleation};
use super::Context;

/// Arguments for the `spore` subcommand family.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    cmd: Sub,
}

/// The three `cs spore` verbs.
#[derive(clap::Subcommand)]
enum Sub {
    /// Parse + expand a spore as a dry run; print the ordered nucleate
    /// call list without germinating anything.
    #[command(after_help = super::examples::SPORE_VALIDATE)]
    Validate(ValidateArgs),

    /// Germinate the polymer: parse + expand + seal gate, then replay the
    /// call list against the live state store.
    #[command(after_help = super::examples::SPORE_RUN)]
    Run(RunArgs),

    /// Emit a content-addressed bundle id and an ASTRA descriptive layer
    /// (ADR-140 D6) for sharing the spore.
    #[command(after_help = super::examples::SPORE_EXPORT)]
    Export(ExportArgs),

    /// Fetch a shareable bundle and place it into this project, installing its
    /// recipes into `.cosmon/formulas/` so their per-step pins reach dispatch.
    #[command(after_help = super::examples::SPORE_INSTALL)]
    Install(InstallArgs),
}

/// `cs spore validate <ref> --var k=v` arguments.
#[derive(clap::Args)]
pub struct ValidateArgs {
    /// Path to a `spore.toml` manifest (or a directory containing one).
    #[arg(value_name = "REF")]
    reference: PathBuf,

    /// Bind a parameter (repeatable: `--var key=value`). Values are coerced
    /// into the declared `ParamSchema` type before expansion.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,
}

/// `cs spore run <ref> --var k=v` arguments.
#[derive(clap::Args)]
pub struct RunArgs {
    /// Path to a `spore.toml` manifest (or a directory containing one).
    #[arg(value_name = "REF")]
    reference: PathBuf,

    /// Bind a parameter (repeatable: `--var key=value`).
    #[arg(long = "var", value_name = "KEY=VALUE")]
    vars: Vec<String>,

    /// Germinate a *sealed* spore even though its `.tla` proof was not
    /// verified this run (TLC unavailable). The status line stays honest:
    /// `seal: present, NOT verified` (ADR-140 D4). Without this flag a
    /// sealed spore fails closed.
    #[arg(long = "allow-unchecked-seal")]
    allow_unchecked_seal: bool,

    /// Fleet to germinate the polymer into.
    #[arg(long, default_value = "default")]
    fleet: String,

    /// State store root (default: walk-up `.cosmon`).
    #[arg(long, value_name = "DIR")]
    store_dir: Option<PathBuf>,
}

/// `cs spore export <ref>` arguments.
#[derive(clap::Args)]
pub struct ExportArgs {
    /// Path to a `spore.toml` manifest (or a directory containing one).
    #[arg(value_name = "REF")]
    reference: PathBuf,

    /// Output directory for the ASTRA descriptive layer. Defaults to the
    /// manifest directory. The crate is always written here as
    /// `ro-crate-metadata.json` unless `[spore.astra].output` names a
    /// different (manifest-relative) path.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
}

/// `cs spore install <source>` arguments.
#[derive(clap::Args)]
pub struct InstallArgs {
    /// Where the bundle comes from: a local path, `github:owner/repo[/dir][@ref]`,
    /// a GitHub tree/blob URL, or any other git remote.
    #[arg(value_name = "SOURCE")]
    source: String,

    /// Where to place the bundle. Defaults to `<project>/spores/<spore-name>/`.
    #[arg(long, value_name = "DIR")]
    dest: Option<PathBuf>,

    /// Branch, tag, or commit to fetch. Overrides a ref encoded in the source.
    #[arg(long = "git-ref", value_name = "REF")]
    git_ref: Option<String>,

    /// Path to the bundle inside the checkout. Overrides one encoded in the
    /// source; the way to install from a subdirectory of a non-GitHub remote.
    #[arg(long, value_name = "PATH")]
    subdir: Option<String>,

    /// Refuse unless the fetched bundle hashes to exactly this id (as printed
    /// by `cs spore export`). Checked before anything is written.
    #[arg(long = "expect-hash", value_name = "BLAKE3")]
    expect_hash: Option<String>,

    /// Place the bundle but do not copy its recipes into `.cosmon/formulas/`.
    /// The bundle then germinates without its per-step adapter/model pins.
    #[arg(long = "no-formulas")]
    no_formulas: bool,

    /// Overwrite a non-empty destination and replace conflicting recipes
    /// already in the registry. The copy is a merge: a file the new bundle
    /// carries replaces the one there, and a file it does not carry is left
    /// alone — nothing is deleted on your behalf.
    #[arg(long)]
    force: bool,

    /// Report what would be installed and write nothing.
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// Formula registry to install recipes into (default: walk-up
    /// `.cosmon/formulas`).
    #[arg(long, value_name = "DIR")]
    formulas_dir: Option<PathBuf>,
}

/// Dispatch a `cs spore` invocation to its verb handler.
///
/// # Errors
/// Propagates manifest-load, expansion, seal-gate, and persistence errors.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.cmd {
        Sub::Validate(a) => run_validate(ctx, a),
        Sub::Run(a) => run_run(ctx, a),
        Sub::Export(a) => run_export(ctx, a),
        Sub::Install(a) => run_install(ctx, a),
    }
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

/// `cs spore validate` — parse + expand, print the call list, germinate nothing.
fn run_validate(ctx: &Context, args: &ValidateArgs) -> anyhow::Result<()> {
    let (spore, _dir) = load_spore(&args.reference)?;
    let params = coerce_vars(&spore, &args.vars)?;
    let calls = expand(&spore, &params).map_err(|e| anyhow::anyhow!("expand failed: {e}"))?;

    if ctx.json {
        for call in &calls {
            println!("{}", call_to_json(call));
        }
    } else {
        println!(
            "spore: {} (v{}) - {} call(s)",
            spore.name,
            spore.version,
            calls.len()
        );
        println!("seal: {}", seal_label(&spore));
        for call in &calls {
            print_call_human(call);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// `cs spore run` — parse + expand + seal gate, then germinate the polymer.
fn run_run(ctx: &Context, args: &RunArgs) -> anyhow::Result<()> {
    let (spore, manifest_dir) = load_spore(&args.reference)?;
    let params = coerce_vars(&spore, &args.vars)?;
    let mut calls = expand(&spore, &params).map_err(|e| anyhow::anyhow!("expand failed: {e}"))?;

    let store_dir = cosmon_filestore::resolve_state_dir(args.store_dir.as_deref());

    // Run-scoped output home (ADR-161). Mint a germination id and hand every
    // node its output directory under `<state>/spore-runs/<germination-id>/`, so
    // a germinated worker writes its gate records where it is TOLD — inside the
    // gitignored state store — never a path it invents inside the spore
    // definition tree or at the repo root (the cosmon-dev dogfooding F9
    // anti-pattern). The id is a runtime value (date + entropy), which is why the
    // pure `expand` cannot compute it: it is resolved here, in the shell.
    //
    // Injection is pure path arithmetic, so it happens BEFORE the seal gate; the
    // directory it names is created only AFTER the gate authorizes germination
    // (below). A refused run must leave zero durable trace.
    let germination_id = mint_germination_id();
    let run_dir = cosmon_core::spore::run_dir(&store_dir, &germination_id);
    cosmon_core::spore::inject_run_outputs(&mut calls, &run_dir)?;

    // Active containment refusal, not a dormant detector: every handed
    // `output_dir` must be inside the run home and outside the two documented
    // anti-pattern homes (the spore definition tree, the repo root).
    let repo_root = store_dir
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap_or(&store_dir)
        .to_path_buf();
    for call in &calls {
        let Some(out) = call.vars.get(cosmon_core::spore::OUTPUT_DIR_VAR) else {
            continue;
        };
        if let Some(violation) = cosmon_core::spore::forbidden_gate_output(
            std::path::Path::new(out),
            &manifest_dir,
            &repo_root,
        ) {
            anyhow::bail!(
                "node \"{}\" would be handed a forbidden output home {out} ({violation:?}); \
                 refusing to germinate (ADR-161)",
                call.alias
            );
        }
    }

    // Seal gate (ADR-140 D4, N4) — fail closed BEFORE any state is written.
    // The run-output home is deliberately not created yet: a refused seal must
    // leave no durable `spore-runs/<id>` artifact behind (F7).
    // DELIVERABLE 2 (F1): wire the REAL TLC runner + persistent verdict cache so
    // a working JRE + tla2tools.jar actually verifies the seal (no longer
    // hardcoded to "unavailable"). On a JRE-less box the runner reports
    // unavailable and the gate stays fail-closed exactly as before.
    let tlc = cosmon_cli::spore_seal::RealTlcRunner::detect();
    // Persistent verdict cache under `.cosmon/cache/seal/<hash>` (sibling of
    // the state dir, which is `.cosmon/state`).
    let cache_dir = store_dir.parent().map_or_else(
        || store_dir.join("cache").join("seal"),
        |cosmon| cosmon.join("cache").join("seal"),
    );
    let cache = cosmon_cli::spore_seal::FileSealVerdictCache::new(cache_dir);
    let seal_status = gate_seal(
        &spore,
        &manifest_dir,
        args.allow_unchecked_seal,
        &tlc,
        &cache,
    )?;
    // The status note goes to stderr so `--json` stdout stays clean NDJSON.
    eprintln!("{seal_status}");

    // Germination is authorized: NOW create the run home. Eagerly, so the very
    // first worker (the always-on `trace` sidecar) finds a real directory rather
    // than racing to `mkdir -p` it.
    std::fs::create_dir_all(&run_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to create run output home {}: {e}",
            run_dir.display()
        )
    })?;
    eprintln!("run home: {}", run_dir.display());

    // REAL containment, not lexical (COSMON-DEV #21 defect B2, iteration 2).
    // Everything above is path arithmetic on strings; a symlink planted at
    // `<run_home>/<alias>` is lexically inside the home and really outside it.
    // Create every node home ourselves with no-follow semantics and canonicalize
    // it against the real run home, so what the worker is handed is proven to be
    // inside — by the filesystem, immediately before the germination. The state
    // dir is handed in as the outer frame: `create_dir_all(run_dir)` above follows
    // symlinks on every component, so a link planted at the fixed name
    // `<state>/spore-runs` would otherwise relocate the whole home and every
    // per-node check would pass against it (defect ND2, iteration 4).
    let node_homes: Vec<(String, std::path::PathBuf)> = calls
        .iter()
        .filter_map(|call| {
            call.vars
                .get(cosmon_core::spore::OUTPUT_DIR_VAR)
                .map(|out| (call.alias.clone(), std::path::PathBuf::from(out)))
        })
        .collect();
    cosmon_cli::spore_containment::provision_contained_node_dirs(&store_dir, &run_dir, &node_homes)
        .map_err(|breach| {
            anyhow::anyhow!("{breach}; refusing to germinate (ADR-161, defect B2)")
        })?;

    let fleet_id =
        FleetId::new(&args.fleet).map_err(|e| anyhow::anyhow!("invalid fleet id: {e}"))?;
    let (project_id, energy_default) = resolve_project_context(ctx);

    let results = germinate(
        &calls,
        &spore.review,
        &manifest_dir,
        &store_dir,
        &fleet_id,
        project_id.as_ref(),
        energy_default,
    )?;

    if ctx.json {
        for (call, result) in calls.iter().zip(&results) {
            println!("{}", germinated_to_json(call, result));
        }
    } else {
        println!(
            "Germinated spore {} into {} molecule(s):",
            spore.name,
            results.len()
        );
        for result in &results {
            println!("  {} ({})", result.id, result.formula_id);
        }
    }
    Ok(())
}

/// The per-step `adapter` / `model` pins a formula declares, as
/// `(step_id, adapter, model)` — the thing that silently stops applying when
/// the recipe is not in the registry the dispatcher reads.
fn step_pins(
    formula: &cosmon_core::formula::Formula,
) -> Vec<(String, Option<String>, Option<String>)> {
    formula
        .steps
        .iter()
        .map(|s| (s.id.clone(), s.adapter.clone(), s.model.clone()))
        .collect()
}

/// Warn when a germinated node's model/adapter tiering will not survive the
/// trip to dispatch (task-20260725-eb3b).
///
/// A molecule stores its formula by **id**; `cs tackle` resolves that id
/// against the mission project's `.cosmon/formulas/`. A spore ships its
/// recipes *in the bundle*, so unless they were copied into that registry the
/// dispatcher resolves the id to nothing and every pin the spore declares is
/// inert — observed on a 23-node run where the whole documented
/// reasoning/build/mechanical tiering never applied and nothing said so.
///
/// Two distinct failures, both worth a sentence:
///
/// - **absent** — no file of that name in the registry;
/// - **shadowed** — a file of that name exists but declares *different* pins,
///   so dispatch honours the registry's recipe, not the bundle's.
///
/// Returns `None` when the formula pins nothing (there is nothing to lose) or
/// when the registry copy agrees with the bundle. This is a warning and not a
/// refusal on purpose: germinating without pins is a degraded run, not an
/// invalid one, and the operator may be installing the formulas next.
fn unreachable_pin_warning(formula: &cosmon_core::formula::Formula) -> Option<String> {
    pin_warning_against_registry(formula, &cosmon_filestore::resolve_formulas_dir(None))
}

/// [`unreachable_pin_warning`] against an explicit registry directory — the
/// whole decision, with the one process-global read (walk-up discovery)
/// hoisted out so it can be exercised directly.
fn pin_warning_against_registry(
    formula: &cosmon_core::formula::Formula,
    registry_dir: &Path,
) -> Option<String> {
    let bundle_pins = step_pins(formula);
    if bundle_pins
        .iter()
        .all(|(_, adapter, model)| adapter.is_none() && model.is_none())
    {
        return None;
    }

    let registry_path = registry_dir.join(format!("{}.formula.toml", formula.name));
    let remedy = format!(
        "copy it to {} (or re-run from a project whose registry holds it)",
        registry_path.display()
    );

    match std::fs::read_to_string(&registry_path) {
        Err(_) => Some(format!(
            "formula `{}` is not in the mission registry {}; the per-step \
             adapter/model pins it declares will NOT apply at dispatch — every \
             node will run on the adapter default. Remedy: {remedy}",
            formula.name,
            registry_dir.display(),
        )),
        Ok(text) => match cosmon_core::formula::Formula::parse(&text) {
            Ok(registry_formula) if step_pins(&registry_formula) == bundle_pins => None,
            Ok(_) => Some(format!(
                "formula `{}` exists in the mission registry ({}) but declares \
                 different per-step adapter/model pins than the bundle's copy; \
                 dispatch honours the registry's recipe, not the spore's. \
                 Remedy: {remedy}",
                formula.name,
                registry_path.display(),
            )),
            Err(err) => Some(format!(
                "formula `{}` in the mission registry ({}) failed to parse \
                 ({err}); the per-step adapter/model pins the bundle declares \
                 will NOT apply at dispatch. Remedy: {remedy}",
                formula.name,
                registry_path.display(),
            )),
        },
    }
}

/// Mint a per-run germination id: `germ-<YYYYMMDD>-<hex>`.
///
/// The date segment makes a run human-sortable; the entropy suffix makes two
/// runs of the *same* params land in distinct output homes (namespacing —
/// the seal's `NoResourceCollision` across runs). Same shape as a
/// [`MoleculeId`], a wall-clock read the pure core deliberately cannot make,
/// which is why it is minted here in the shell.
fn mint_germination_id() -> String {
    let date = chrono::Utc::now().format("%Y%m%d");
    let suffix: u32 = rand::random();
    format!("germ-{date}-{suffix:08x}")
}

/// Replay the expanded call list against the live state store, mapping each
/// node alias to its freshly-minted [`MoleculeId`] so later calls can wire
/// `blocked_by` to the real molecules. The list is ordered so every
/// `blocked_by` alias is already germinated when its dependent is reached.
fn germinate(
    calls: &[NucleateCall],
    review: &cosmon_core::fleet::CrossProviderReview,
    manifest_dir: &Path,
    store_dir: &Path,
    fleet_id: &FleetId,
    project_id: Option<&cosmon_core::id::ProjectId>,
    energy_budget_cap: u32,
) -> anyhow::Result<Vec<NucleateResult>> {
    let mut warm =
        vec![Tag::new("temp:warm").map_err(|e| anyhow::anyhow!("internal tag error: {e}"))?];
    if review.cross_provider {
        warm.push(Tag::new("needs-review").expect("static review tag is valid"));
        warm.push(
            Tag::new("needs-review-cross-provider")
                .expect("static cross-provider review tag is valid"),
        );
        if let Some(adapter) = &review.reviewer_adapter {
            warm.push(
                Tag::new(format!("reviewer-adapter:{adapter}")).map_err(|e| {
                    anyhow::anyhow!("invalid spore reviewer_adapter `{adapter}`: {e}")
                })?,
            );
        }
    }
    let mut alias_to_id: BTreeMap<String, MoleculeId> = BTreeMap::new();
    let mut results = Vec::with_capacity(calls.len());
    // Formula names already checked for reachable pins — a spore typically
    // binds the same recipe on many nodes, and the operator needs the warning
    // once, not twenty-three times.
    let mut unpinnable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for call in calls {
        // Resolve this call's blocked_by aliases to already-germinated IDs.
        // The expand() ordering guarantee makes every lookup succeed.
        let mut blocked_by = Vec::with_capacity(call.blocked_by.len());
        for alias in &call.blocked_by {
            let id = alias_to_id.get(alias).ok_or_else(|| {
                anyhow::anyhow!(
                    "internal: blocked_by alias `{alias}` of call `{}` not yet germinated",
                    call.alias
                )
            })?;
            blocked_by.push(id.clone());
        }

        let formula_path = manifest_dir.join(&call.formula);
        let formula = load_formula_at_path(&formula_path)?;
        // A germinated molecule stores its formula **by id**, and `cs tackle`
        // resolves that id against the mission project's `.cosmon/formulas/` —
        // not against this spore directory (task-20260725-eb3b). If the
        // bundle's formulas were never installed there, every per-step
        // adapter/model pin the spore ships is unreachable at dispatch and the
        // run falls back to the adapter default. Say so here, once per
        // formula, while an operator is still watching the terminal.
        if unpinnable.insert(formula.name.to_string()) {
            if let Some(reason) = unreachable_pin_warning(&formula) {
                eprintln!("warning: {reason}");
            }
        }
        let result = nucleate_for_spore(SporeNucleation {
            formula: &formula,
            variables: call.vars.clone().into_iter().collect(),
            kind: None,
            blocked_by: &blocked_by,
            fleet_id,
            store_dir,
            project_id,
            tags: &warm,
            energy_budget_cap,
        })?;
        alias_to_id.insert(call.alias.clone(), result.id.clone());
        results.push(result);
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// `cs spore export` — content-addressed bundle id + ASTRA descriptive layer.
fn run_export(ctx: &Context, args: &ExportArgs) -> anyhow::Result<()> {
    let (spore, manifest_dir) = load_spore(&args.reference)?;
    let covered = bundle_files(&spore, &manifest_dir);
    let bundle = bundle_hash(&covered, &manifest_dir);

    let out_dir = args.out.clone().unwrap_or_else(|| manifest_dir.clone());
    let astra = build_astra(&spore, &bundle, &covered);

    // `cs spore export` is the explicit *share-time* emit verb (ADR-140 D6):
    // invoking it always writes the RO-Crate. The `[spore.astra]` stanza only
    // customizes the output path; its absence is not an opt-out (that would
    // make an `export` that exports nothing). When no `output` is declared we
    // default to the RO-Crate standard filename in the out dir. The `emit`
    // flag governs *automatic* emission on run/mission-completion, not this
    // hand-invoked verb.
    let astra_rel = spore
        .astra
        .as_ref()
        .and_then(|a| a.output.clone())
        .unwrap_or_else(|| "ro-crate-metadata.json".to_string());
    let astra_path = out_dir.join(astra_rel);

    if let Some(parent) = astra_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&astra)
        .map_err(|e| anyhow::anyhow!("serialize ASTRA: {e}"))?;
    std::fs::write(&astra_path, body)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", astra_path.display()))?;

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "spore": spore.name,
                "bundle_hash": bundle,
                "covered_files": covered,
                "astra": astra_path.display().to_string(),
            })
        );
    } else {
        println!("spore: {}", spore.name);
        println!("bundle: {bundle}");
        println!("covered ({} file(s)):", covered.len());
        for rel in &covered {
            println!("  - {rel}");
        }
        println!("astra: {}", astra_path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------

/// One recipe's fate at install time, resolved against the live registry.
struct PlannedFormula {
    /// The name the recipe declares (`formula = "..."`), which is the name
    /// `cs tackle` resolves at dispatch — not the file name in the bundle.
    name: String,
    /// Where it will live: `<registry>/<name>.formula.toml`.
    target: PathBuf,
    /// The bundle's bytes for it.
    body: String,
    /// Install, no-op, or refusal.
    placement: cosmon_core::spore::FormulaPlacement,
}

/// `cs spore install` — fetch a bundle, place it, and register its recipes.
///
/// The verb exists because `cs spore run` reads a bundle by path and germinates
/// molecules that store their formula **by id**, which `cs tackle` later
/// resolves against the mission project's `.cosmon/formulas/`. Sharing a spore
/// therefore had two steps and a trap: get the files (no verb), and copy the
/// recipes into the registry (no verb, and skipping it silently drops every
/// per-step adapter/model pin at dispatch — task-20260725-eb3b). This verb is
/// both steps, and it is called `install` because the registration is the
/// load-bearing half.
///
/// Order matters and is fail-closed: fetch into a temporary checkout, parse the
/// manifest, hash the bundle and check `--expect-hash`, plan the registry
/// writes, and only then write anything into the project. A refused install
/// leaves the project byte-identical to how it started.
fn run_install(ctx: &Context, args: &InstallArgs) -> anyhow::Result<()> {
    let source = cosmon_core::spore::parse_source(&args.source)
        .map_err(|e| anyhow::anyhow!("invalid spore source: {e}"))?;
    let fetched =
        cosmon_cli::spore_install::fetch(&source, args.git_ref.as_deref(), args.subdir.as_deref())?;

    // Parse before copying: a bundle that is not a spore never reaches the
    // project tree.
    let (spore, bundle_dir) = load_spore(&fetched.root)?;

    // Integrity, when the operator brought an expected id. Same content
    // addressing as `cs spore export`, so the id an author publishes is the id
    // a recipient checks (ADR-039).
    let covered = bundle_files(&spore, &bundle_dir);
    let bundle = bundle_hash(&covered, &bundle_dir);
    if let Some(expected) = &args.expect_hash {
        if expected.trim() != bundle {
            anyhow::bail!(
                "bundle hash mismatch: expected {expected}, fetched {bundle}; \
                 refusing to install (nothing was written)"
            );
        }
    }

    // Every file the manifest declares must actually be in the bundle. A
    // missing recipe or seal would install a spore that parses and then fails
    // at germination, which is a worse place to learn it.
    for rel in &covered {
        if !bundle_dir.join(rel).is_file() {
            anyhow::bail!(
                "bundle declares `{rel}` but the fetched source does not contain it; \
                 refusing to install an incomplete bundle"
            );
        }
    }

    let registry_dir = cosmon_filestore::resolve_formulas_dir(args.formulas_dir.as_deref());
    let dest = resolve_install_dest(args.dest.as_deref(), &registry_dir, &spore.name)?;
    let planned = plan_formulas(&spore, &bundle_dir, &registry_dir)?;

    if !args.force {
        if cosmon_cli::spore_install::is_occupied(&dest)? {
            anyhow::bail!(
                "destination {} is not empty; pass --force to overwrite it",
                dest.display()
            );
        }
        let conflicts: Vec<&str> = planned
            .iter()
            .filter(|p| p.placement == cosmon_core::spore::FormulaPlacement::Conflicts)
            .map(|p| p.name.as_str())
            .collect();
        if !args.no_formulas && !conflicts.is_empty() {
            anyhow::bail!(
                "the formula registry {} already holds a DIFFERENT recipe for {}; \
                 dispatch resolves a molecule's formula id against that registry, so \
                 overwriting changes what every molecule already germinated from it \
                 runs. Pass --force to replace, or --no-formulas to keep the registry \
                 as it is",
                registry_dir.display(),
                conflicts.join(", "),
            );
        }
    }

    if args.dry_run {
        report_install(ctx, &spore, &bundle, &dest, &planned, args, None);
        return Ok(());
    }

    let copied = cosmon_cli::spore_install::copy_tree(&bundle_dir, &dest)?;

    let mut installed = Vec::new();
    if !args.no_formulas {
        std::fs::create_dir_all(&registry_dir).map_err(|e| {
            anyhow::anyhow!(
                "failed to create the formula registry {}: {e}",
                registry_dir.display()
            )
        })?;
        for p in &planned {
            if p.placement == cosmon_core::spore::FormulaPlacement::Identical {
                continue;
            }
            std::fs::write(&p.target, &p.body)
                .map_err(|e| anyhow::anyhow!("failed to install {}: {e}", p.target.display()))?;
            installed.push(p.name.clone());
        }
    }

    write_provenance(&dest, args, &source, &fetched, &bundle, &spore, &installed)?;
    report_install(
        ctx,
        &spore,
        &bundle,
        &dest,
        &planned,
        args,
        Some(copied.len()),
    );
    Ok(())
}

/// Where the bundle lands: `--dest` verbatim, else `<project>/spores/<name>/`.
///
/// The project root is derived from the resolved registry (`<root>/.cosmon/
/// formulas`) rather than the process CWD, so `cs spore install` run from a
/// subdirectory installs into the same project whose registry it writes — the
/// two halves of the verb cannot end up in different trees.
fn resolve_install_dest(
    explicit: Option<&Path>,
    registry_dir: &Path,
    spore_name: &str,
) -> anyhow::Result<PathBuf> {
    if let Some(dir) = explicit {
        return Ok(dir.to_path_buf());
    }
    let slug = cosmon_core::spore::default_dest_slug(spore_name).map_err(|e| {
        anyhow::anyhow!("spore name `{spore_name}` cannot be a directory name ({e}); pass --dest")
    })?;
    let root = registry_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    Ok(root.join(cosmon_core::spore::SPORES_DIR).join(slug))
}

/// Resolve every `[spore.formulas.*]` recipe against the live registry.
///
/// The registry file name comes from the recipe's declared `formula = "..."`
/// name, because that is what a molecule stores and what `cs tackle` looks up.
/// Installing under the bundle's file name instead would put the bytes in the
/// registry and leave the pins exactly as unreachable as before.
fn plan_formulas(
    spore: &Spore,
    bundle_dir: &Path,
    registry_dir: &Path,
) -> anyhow::Result<Vec<PlannedFormula>> {
    let mut planned: Vec<PlannedFormula> = Vec::new();
    for f in spore.formulas.values() {
        let path = bundle_dir.join(&f.path);
        let body = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let formula = cosmon_core::formula::Formula::parse(&body)
            .map_err(|e| anyhow::anyhow!("bundle recipe {} does not parse: {e}", f.path))?;
        let name = formula.name.to_string();
        if planned.iter().any(|p| p.name == name) {
            continue;
        }
        let target = registry_dir.join(cosmon_core::spore::registry_file_name(&name));
        let existing = std::fs::read_to_string(&target).ok();
        let placement = cosmon_core::spore::plan_formula(&body, existing.as_deref());
        planned.push(PlannedFormula {
            name,
            target,
            body,
            placement,
        });
    }
    Ok(planned)
}

/// Record what an installed bundle is a copy *of*.
///
/// A branch name does not answer that question a week later, so the commit is
/// recorded next to it. The file is dot-prefixed and deliberately outside the
/// coverage set [`bundle_files`] binds: writing it never changes the hash of
/// the bundle it describes.
fn write_provenance(
    dest: &Path,
    args: &InstallArgs,
    source: &cosmon_core::spore::SporeSource,
    fetched: &cosmon_cli::spore_install::FetchedBundle,
    bundle: &str,
    spore: &Spore,
    installed_formulas: &[String],
) -> anyhow::Result<()> {
    let mut doc = toml::map::Map::new();
    let mut install = toml::map::Map::new();
    install.insert("source".into(), toml::Value::String(args.source.clone()));
    install.insert(
        "resolved".into(),
        toml::Value::String(match source {
            cosmon_core::spore::SporeSource::Local(p) => p.clone(),
            cosmon_core::spore::SporeSource::Git { remote, .. } => remote.clone(),
            // `SporeSource` is `#[non_exhaustive]`; a later kind records what
            // the operator typed rather than nothing at all.
            _ => args.source.clone(),
        }),
    );
    if let Some(r) = &args.git_ref {
        install.insert("git_ref".into(), toml::Value::String(r.clone()));
    }
    if let Some(s) = &args.subdir {
        install.insert("subdir".into(), toml::Value::String(s.clone()));
    }
    if let Some(commit) = &fetched.commit {
        install.insert("commit".into(), toml::Value::String(commit.clone()));
    }
    install.insert(
        "bundle_hash".into(),
        toml::Value::String(bundle.to_string()),
    );
    install.insert("spore".into(), toml::Value::String(spore.name.clone()));
    install.insert(
        "installed_at".into(),
        toml::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    install.insert(
        "formulas".into(),
        toml::Value::Array(
            installed_formulas
                .iter()
                .map(|n| toml::Value::String(n.clone()))
                .collect(),
        ),
    );
    doc.insert("install".into(), toml::Value::Table(install));

    let body = format!(
        "# Written by `cs spore install`. Install metadata, not bundle content:\n\
         # no bundle hash covers this file.\n{}",
        toml::to_string_pretty(&toml::Value::Table(doc))
            .map_err(|e| anyhow::anyhow!("failed to render install provenance: {e}"))?
    );
    let path = dest.join(cosmon_core::spore::PROVENANCE_FILE);
    std::fs::write(&path, body)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))
}

/// Print the install outcome (or, for `--dry-run`, the plan).
///
/// `copied` is the number of files written, and `None` says nothing was: the
/// dry run is the only caller that has no count, so the two facts are one
/// argument rather than two that could disagree.
fn report_install(
    ctx: &Context,
    spore: &Spore,
    bundle: &str,
    dest: &Path,
    planned: &[PlannedFormula],
    args: &InstallArgs,
    copied: Option<usize>,
) {
    let dry_run = copied.is_none();
    let formulas: Vec<serde_json::Value> = planned
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "target": p.target.display().to_string(),
                "action": placement_label(p.placement, args.no_formulas),
            })
        })
        .collect();

    if ctx.json {
        println!(
            "{}",
            serde_json::json!({
                "spore": spore.name,
                "version": spore.version,
                "bundle_hash": bundle,
                "dest": dest.display().to_string(),
                "files": copied,
                "formulas": formulas,
                "dry_run": dry_run,
            })
        );
        return;
    }

    let verb = if dry_run {
        "would install"
    } else {
        "installed"
    };
    println!("spore: {} (v{})", spore.name, spore.version);
    println!("bundle: {bundle}");
    match copied {
        Some(n) => println!("{verb} {n} file(s) into {}", dest.display()),
        None => println!("{verb} into {}", dest.display()),
    }
    if planned.is_empty() {
        println!("formulas: none declared");
    } else {
        println!("formulas:");
        for p in planned {
            println!(
                "  - {} → {} [{}]",
                p.name,
                p.target.display(),
                placement_label(p.placement, args.no_formulas)
            );
        }
    }
    if !dry_run {
        println!("next: cs spore validate {}", dest.display());
    }
}

/// The reported action for one recipe. `--no-formulas` is shown as `skipped`
/// rather than hidden: an operator who opted out still needs to see which pins
/// will not reach dispatch.
fn placement_label(
    placement: cosmon_core::spore::FormulaPlacement,
    no_formulas: bool,
) -> &'static str {
    use cosmon_core::spore::FormulaPlacement;
    if no_formulas {
        return "skipped (--no-formulas)";
    }
    match placement {
        FormulaPlacement::Install => "installed",
        FormulaPlacement::Identical => "already identical",
        FormulaPlacement::Conflicts => "replaced (--force)",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Resolve a `<ref>` to a parsed [`Spore`] and its manifest directory (the
/// base for relative formula and seal paths). `<ref>` may be a `spore.toml`
/// file or a directory containing one.
fn load_spore(reference: &Path) -> anyhow::Result<(Spore, PathBuf)> {
    let manifest = if reference.is_dir() {
        reference.join("spore.toml")
    } else {
        reference.to_path_buf()
    };
    let text = std::fs::read_to_string(&manifest).map_err(|e| {
        anyhow::anyhow!("failed to read spore manifest {}: {e}", manifest.display())
    })?;
    let spore = Spore::parse(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse spore {}: {e}", manifest.display()))?;
    let dir = manifest
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to resolve the current directory: {e}"))?;
    Ok((spore, anchor_manifest_dir(dir, &cwd)))
}

/// Anchor a manifest directory to an absolute path so downstream containment
/// checks compare like with like.
///
/// A RELATIVE reference (`cs spore run spore-starter.toml` from inside the
/// spore directory) yields a manifest parent of `""` or `.`, and the lexical
/// [`forbidden_gate_output`](cosmon_core::spore::forbidden_gate_output) guard
/// compares that against ABSOLUTE run-home paths: an un-anchored dir either
/// matches everything (empty prefix) or nothing, both wrong. Anchoring is
/// lexical (`cwd.join`), not `canonicalize`: symlinks are the province of the
/// real-containment provisioning step, and resolving them here would make the
/// reported spore dir differ from what the operator typed.
fn anchor_manifest_dir(dir: PathBuf, cwd: &Path) -> PathBuf {
    if dir.as_os_str().is_empty() || dir == Path::new(".") {
        cwd.to_path_buf()
    } else if dir.is_absolute() {
        dir
    } else {
        cwd.join(dir)
    }
}

/// Coerce raw `--var key=value` strings into the declared `ParamSchema`
/// TOML types so the pure [`expand`](fn@expand) sees properly-typed values. An
/// undeclared key is passed through as a string and rejected by `expand`
/// (which owns the unknown-param error), keeping a single source of truth.
fn coerce_vars(spore: &Spore, raw: &[String]) -> anyhow::Result<BTreeMap<String, toml::Value>> {
    let mut out = BTreeMap::new();
    for kv in raw {
        let (key, value) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid --var (expected key=value): {kv}"))?;
        let coerced = coerce_one(spore, key, value)?;
        out.insert(key.to_string(), coerced);
    }
    Ok(out)
}

/// Coerce one value against its declared type. List values split on `,`.
fn coerce_one(spore: &Spore, key: &str, value: &str) -> anyhow::Result<toml::Value> {
    let ty = spore.params.get(key).map(|s| s.ty);
    Ok(match ty {
        Some(ParamType::Int) => toml::Value::Integer(
            value
                .trim()
                .parse::<i64>()
                .map_err(|e| anyhow::anyhow!("--var {key}: expected int, got `{value}`: {e}"))?,
        ),
        Some(ParamType::Bool) => toml::Value::Boolean(
            value
                .trim()
                .parse::<bool>()
                .map_err(|e| anyhow::anyhow!("--var {key}: expected bool, got `{value}`: {e}"))?,
        ),
        Some(ParamType::ListString) => toml::Value::Array(
            value
                .split(',')
                .map(|s| toml::Value::String(s.trim().to_string()))
                .collect(),
        ),
        // String, Enum, or an undeclared key: keep the raw string and let
        // `expand` enforce schema membership / unknown-param rejection.
        _ => toml::Value::String(value.to_string()),
    })
}

/// The seal gate (ADR-140 D4, N4). Verifies the spore's `.tla` proof through
/// the injected [`TlcRunner`](cosmon_core::spore::TlcRunner) +
/// [`SealVerdictCache`](cosmon_core::spore::SealVerdictCache), then returns the
/// honest status line or fails closed.
///
/// This is the DELIVERABLE 2 (F1) fix: the gate used to be hardcoded to bail
/// "TLC unavailable" for *every* sealed spore, so a seal could never be
/// verified even on a machine with a working JRE + `tla2tools.jar`. Now the
/// gate actually runs the pure orchestration
/// ([`verify_seal`](cosmon_core::spore::verify_seal) →
/// [`gate`](cosmon_core::spore::gate)):
///
/// * TLC available + proof passes → `seal: verified <hash>`, germinate WITHOUT
///   `--allow-unchecked-seal`.
/// * TLC unavailable → `seal: present, NOT verified`, fail-closed unless
///   `allow_unchecked` (the existing, correct behaviour on a JRE-less box).
/// * TLC available + proof rejected → refuse **unconditionally** (the opt-in
///   flag does not rescue a known-unsafe proof).
///
/// The proof bytes are read relative to `manifest_dir`. The honesty invariant
/// lives entirely in the pure core; this shell only reads files and threads the
/// I/O seams. Returns the honest report line on germinate, or an error carrying
/// the honest refusal line.
///
/// # Errors
/// Bails when the gate refuses (fail-closed seal / rejected proof) or the
/// verdict cache backend errors.
fn gate_seal(
    spore: &Spore,
    manifest_dir: &Path,
    allow_unchecked: bool,
    tlc: &dyn cosmon_core::spore::TlcRunner,
    cache: &dyn cosmon_core::spore::SealVerdictCache,
) -> anyhow::Result<String> {
    use cosmon_core::spore::{gate, verify_seal, ResolvedSeal};

    // Read the proof bytes off disk so the core can hash them (cache key) and
    // hand the paths to TLC. A seal whose files cannot be read resolves to
    // `None` here; `verify_seal` then reports it honestly as unchecked, never
    // as verified.
    let module_path;
    let config_path;
    let module_bytes;
    let config_bytes;
    let resolved = match &spore.seal {
        Some(seal) => {
            module_path = manifest_dir.join(&seal.module);
            config_path = seal.config.as_ref().map(|c| manifest_dir.join(c));
            match std::fs::read(&module_path) {
                Ok(bytes) => {
                    module_bytes = bytes;
                    config_bytes = match &config_path {
                        Some(p) => std::fs::read(p).ok(),
                        None => None,
                    };
                    Some(ResolvedSeal {
                        module_path: module_path.as_path(),
                        config_path: config_path.as_deref(),
                        module_bytes: module_bytes.as_slice(),
                        config_bytes: config_bytes.as_deref(),
                    })
                }
                // Proof module unreadable — pass `None` so the core reports it
                // as unchecked (fail-closed by default), never verified.
                Err(_) => None,
            }
        }
        None => None,
    };

    let status = verify_seal(spore.seal.as_ref(), resolved.as_ref(), cache, tlc)
        .map_err(|e| anyhow::anyhow!("seal verification failed: {e}"))?;
    let decision = gate(&status, allow_unchecked);
    if decision.germinates() {
        Ok(decision.report().to_string())
    } else {
        anyhow::bail!("{} (fail-closed, ADR-140 D4)", decision.report())
    }
}

/// The non-gating seal label used by `validate` (read-only, no refusal).
fn seal_label(spore: &Spore) -> &'static str {
    if spore.seal.is_some() {
        "present, NOT verified"
    } else {
        "none"
    }
}

/// Resolve project id and the default step budget from `.cosmon/config.toml`,
/// falling back to defaults for legacy projects.
fn resolve_project_context(ctx: &Context) -> (Option<cosmon_core::id::ProjectId>, u32) {
    let config_path = super::resolve_config_from_context(ctx);
    let loaded = cosmon_filestore::load_project_config(&config_path).ok();
    let project_id = loaded.as_ref().and_then(|c| c.project.project_id.clone());
    let energy = loaded
        .as_ref()
        .map_or(100, |c| c.energy.default_step_budget);
    (project_id, energy)
}

/// The conventional crew constitution shipped alongside a spore manifest.
const FLEET_MANIFEST: &str = "fleet.toml";

/// Enumerate every bundle file the recipient receives, as manifest-relative
/// paths in sorted, deduplicated order. This is the *coverage set* the
/// [`bundle_hash`] binds — and, just as importantly, the list an integrity
/// audit can read to see exactly what the advertised id does and does not
/// cover.
///
/// The set is the union of:
///
/// - the manifest itself (`spore.toml`);
/// - every per-node recipe (`[spore.formulas.*].path`);
/// - the seal module and its `.cfg`, when a `[spore.seal]` is present;
/// - the crew constitution `fleet.toml`, when it ships in the bundle, **plus
///   every `file:` fleet it includes** — the integrity gap this closes
///   (sporarium recette v3.2 prise n°5): a bundle used to advertise a hash
///   over `spore.toml` + formulas + seal while shipping a `fleet.toml` the
///   hash never covered, so the crew could be altered without changing the id.
///
/// A fleet manifest that fails to parse still has its own bytes covered (the
/// file is in the bundle); only its declared includes are then skipped, since
/// their paths cannot be read from an unparseable manifest. Export never fails
/// closed on a malformed `fleet.toml` — it binds what it can and lists it.
fn bundle_files(spore: &Spore, manifest_dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = vec!["spore.toml".to_string()];
    for f in spore.formulas.values() {
        files.push(f.path.clone());
    }
    if let Some(seal) = &spore.seal {
        files.push(seal.module.clone());
        if let Some(cfg) = &seal.config {
            files.push(cfg.clone());
        }
    }

    // The crew constitution ships in the bundle even though the `[spore.fleet]`
    // stanza only names it — a physically present file must be covered, or the
    // advertised id lies about what the recipient received.
    let fleet_path = manifest_dir.join(FLEET_MANIFEST);
    if let Ok(text) = std::fs::read_to_string(&fleet_path) {
        files.push(FLEET_MANIFEST.to_string());
        // Fold in each `file:` include the crew constitution composes from.
        // A parse failure is not fatal here: the fleet.toml bytes are already
        // covered above; we simply cannot enumerate includes we cannot read.
        if let Ok(spec) = cosmon_core::fleet::FleetSpec::parse(&text) {
            for inc in &spec.includes {
                if inc.scheme == "file" {
                    files.push(inc.path.clone());
                }
            }
        }
    }

    files.sort();
    files.dedup();
    files
}

/// Compute a content-addressed bundle hash over every file the recipient
/// receives (see [`bundle_files`]). The hash binds each file's relative path
/// and bytes in sorted order, so the same bundle content always yields the
/// same id (content-addressing is the registry, ADR-039).
fn bundle_hash(files: &[String], manifest_dir: &Path) -> String {
    let mut buf: Vec<u8> = Vec::new();
    for rel in files {
        let path = manifest_dir.join(rel);
        let bytes = std::fs::read(&path).unwrap_or_default();
        buf.extend_from_slice(rel.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(&bytes);
    }
    format!("blake3:{}", cosmon_hash::Hash::of_bytes(&buf).to_hex())
}

/// Build a minimal RO-Crate / ASTRA-compatible descriptive layer (ADR-140
/// D6). The spore header, params, and DAG map onto ASTRA's method /
/// provenance fields; the seal verdict is attached as the proof artifact —
/// honestly marked unverified, never claiming a proof that did not run. The
/// `covered` file list is recorded verbatim so a recipient can audit exactly
/// which bundle files the `spore:bundleHash` binds (integrity transparency).
fn build_astra(spore: &Spore, bundle: &str, covered: &[String]) -> serde_json::Value {
    let params: Vec<_> = spore
        .params
        .iter()
        .map(|(name, spec)| {
            serde_json::json!({
                "name": name,
                "required": spec.required,
            })
        })
        .collect();
    let nodes: Vec<_> = spore
        .nodes
        .iter()
        .map(|n| serde_json::json!({ "id": n.id, "formula": n.formula }))
        .collect();
    let edges: Vec<_> = spore
        .edges
        .iter()
        .map(|e| serde_json::json!({ "from": e.from, "to": e.to }))
        .collect();

    serde_json::json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": [
            {
                "@type": "CreativeWork",
                "@id": "ro-crate-metadata.json",
                "conformsTo": { "@id": "https://w3id.org/ro/crate/1.1" },
                "about": { "@id": "./" }
            },
            {
                "@id": "./",
                "@type": "Dataset",
                "name": spore.name,
                "version": spore.version,
                "description": spore.description,
                "spore:bundleHash": bundle,
                "spore:bundleFiles": covered,
                "spore:params": params,
                "spore:nodes": nodes,
                "spore:edges": edges,
                "spore:seal": {
                    "present": spore.seal.is_some(),
                    "verified": false
                }
            }
        ]
    })
}

/// Render one expanded call as a JSON line (`validate --json`).
fn call_to_json(call: &NucleateCall) -> serde_json::Value {
    serde_json::json!({
        "alias": call.alias,
        "formula": call.formula,
        "kind": node_kind_label(call.kind),
        "blocked_by": call.blocked_by,
        "vars": call.vars,
        "mission": call.mission,
        "for_each": call.for_each,
        "bounds": call.bounds.as_ref().map(|b| serde_json::json!({
            "output_type": b.output_type,
            "max_instances": b.max_instances,
            "stop_condition": b.stop_condition,
            "instances_var": b.instances_var,
        })),
    })
}

/// Render one germinated molecule as a JSON line (`run --json`).
fn germinated_to_json(call: &NucleateCall, result: &NucleateResult) -> serde_json::Value {
    serde_json::json!({
        "alias": call.alias,
        "id": result.id.as_str(),
        "formula": result.formula_id.as_str(),
        "blocked_by": call.blocked_by,
        "status": "active",
    })
}

/// Human-readable one-block rendering of a call for `validate`.
fn print_call_human(call: &NucleateCall) {
    println!("  • {} [{}]", call.alias, node_kind_label(call.kind));
    println!("      formula: {}", call.formula);
    if let Some(mission) = &call.mission {
        println!("      mission: {mission}");
    }
    if !call.blocked_by.is_empty() {
        println!("      blocked-by: {}", call.blocked_by.join(", "));
    }
    for (k, v) in &call.vars {
        println!("      var {k} = {v}");
    }
}

/// The wire label for a [`NodeKind`].
fn node_kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Fixed => "fixed",
        NodeKind::Fanout => "fanout",
        NodeKind::Emergent => "emergent",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPORE: &str = r#"
[spore]
name = "demo"
version = 1

[spore.params.subject]
type = "string"
required = true

[spore.params.axes]
type = "list<string>"
default = ["a", "b"]

[spore.formulas.work]
path = "work.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"
[spore.node.vars]
subject = "${params.subject}"

[[spore.node]]
id = "analyse"
kind = "fanout"
for_each = "${params.axes}"
formula = "work"
[spore.node.vars]
axis = "${item}"

[[spore.edge]]
from = "frame"
to = "analyse"
type = "feeds"
"#;

    const SEALED: &str = r#"
[spore]
name = "sealed"

[spore.seal]
module = "spore.tla"
config = "spore.cfg"

[spore.formulas.work]
path = "work.formula.toml"

[[spore.node]]
id = "frame"
kind = "fixed"
formula = "work"
"#;

    const FORMULA: &str = r#"
formula = "work"
version = 1
id_prefix = "task"

[[steps]]
id = "do"
title = "Do the work"
description = "the body"
acceptance = "any evidence"
"#;

    /// Write a spore + its referenced formula into a temp dir, return the dir.
    fn fixture(spore: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("spore.toml"), spore).unwrap();
        std::fs::write(dir.path().join("work.formula.toml"), FORMULA).unwrap();
        dir
    }

    #[test]
    fn coerce_vars_respects_declared_types() {
        let spore = Spore::parse(SPORE).unwrap();
        let got = coerce_vars(
            &spore,
            &["subject=octopus".to_string(), "axes=x,y,z".to_string()],
        )
        .unwrap();
        assert_eq!(got["subject"], toml::Value::String("octopus".into()));
        assert_eq!(
            got["axes"],
            toml::Value::Array(vec!["x".into(), "y".into(), "z".into()])
        );
    }

    #[test]
    fn coerce_vars_rejects_missing_equals() {
        let spore = Spore::parse(SPORE).unwrap();
        let err = coerce_vars(&spore, &["bogus".to_string()]).unwrap_err();
        assert!(format!("{err}").contains("expected key=value"));
    }

    /// Write a sealed-spore fixture: manifest + formula + a `.tla`/`.cfg` pair
    /// so `gate_seal` can read the proof bytes off disk.
    fn sealed_fixture() -> tempfile::TempDir {
        let dir = fixture(SEALED);
        std::fs::write(
            dir.path().join("spore.tla"),
            b"---- MODULE spore ----\n====",
        )
        .unwrap();
        std::fs::write(dir.path().join("spore.cfg"), b"INVARIANT Termination\n").unwrap();
        dir
    }

    /// Review finding F7, frozen as a red-first regression — and the composed
    /// test the earlier suites lacked (they exercised `gate_seal` and the
    /// output-home injection in isolation, never their *ordering*).
    ///
    /// A sealed spore whose `.tla` cannot be read resolves to
    /// `UncheckedToolUnavailable`, so the gate refuses — deterministically, with
    /// or without a JRE on the box. The contract is that this refusal costs
    /// **nothing durable**: before the fix `run_run` created
    /// `<state>/spore-runs/<germination-id>/` at line 195 and only *then* ran
    /// the gate, leaving an orphan empty directory behind and falsifying the
    /// comment that promised a fail-close "before any state is written".
    #[test]
    fn a_refused_seal_leaves_no_run_output_directory_behind() {
        let dir = fixture(SEALED); // sealed manifest, but NO spore.tla written
        let state = tempfile::tempdir().unwrap();
        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = RunArgs {
            reference: dir.path().join("spore.toml"),
            vars: vec![],
            allow_unchecked_seal: false,
            fleet: "default".to_string(),
            store_dir: Some(state.path().to_path_buf()),
        };

        let err = run_run(&ctx, &args).expect_err("an unreadable proof must fail closed");
        assert!(
            format!("{err}").contains("NOT verified"),
            "the refusal must be the seal gate's, not an incidental error: {err}"
        );

        let runs = state.path().join(cosmon_core::spore::SPORE_RUNS_DIR);
        assert!(
            !runs.exists(),
            "a refused germination must write ZERO state, but {} exists",
            runs.display()
        );
    }

    #[test]
    fn seal_gate_absent_proceeds() {
        let spore = Spore::parse(SPORE).unwrap();
        let cache = cosmon_core::spore::InMemorySealVerdictCache::new();
        let tlc = cosmon_core::spore::FakeTlcRunner::unavailable();
        let status = gate_seal(&spore, Path::new("."), false, &tlc, &cache).unwrap();
        assert_eq!(status, "seal: none");
    }

    #[test]
    fn seal_gate_present_fails_closed_by_default_when_tlc_unavailable() {
        let dir = sealed_fixture();
        let spore = Spore::parse(SEALED).unwrap();
        let cache = cosmon_core::spore::InMemorySealVerdictCache::new();
        let tlc = cosmon_core::spore::FakeTlcRunner::unavailable();
        let err = gate_seal(&spore, dir.path(), false, &tlc, &cache).unwrap_err();
        // Fail-closed refusal names the missing verifier, not a stale hardcode.
        assert!(format!("{err}").contains("NOT verified"));
    }

    #[test]
    fn seal_gate_present_proceeds_under_flag_but_never_claims_verified() {
        let dir = sealed_fixture();
        let spore = Spore::parse(SEALED).unwrap();
        let cache = cosmon_core::spore::InMemorySealVerdictCache::new();
        let tlc = cosmon_core::spore::FakeTlcRunner::unavailable();
        let status = gate_seal(&spore, dir.path(), true, &tlc, &cache).unwrap();
        assert!(status.contains("NOT verified"));
        // The honest line never claims a bare "verified".
        assert!(!status.contains("verified (") || status.contains("NOT verified"));
    }

    /// DELIVERABLE 2 (F1): the seal-gate REGRESSION. When TLC is actually
    /// available and the proof passes, `cs spore run` must VERIFY the seal and
    /// germinate WITHOUT `--allow-unchecked-seal`. The old `gate_seal` was
    /// hardcoded to bail "TLC unavailable" no matter what — this is the root
    /// cause of Jesse's seal-gate observation.
    #[test]
    fn seal_gate_verifies_when_tlc_available_and_proof_passes() {
        let dir = sealed_fixture();
        let spore = Spore::parse(SEALED).unwrap();
        let cache = cosmon_core::spore::InMemorySealVerdictCache::new();
        let tlc = cosmon_core::spore::FakeTlcRunner::available_with(
            cosmon_core::spore::TlcOutcome::Passed,
        );
        // allow_unchecked = false: a real TLC pass must NOT need the escape hatch.
        let status = gate_seal(&spore, dir.path(), false, &tlc, &cache).unwrap();
        assert!(
            status.contains("verified") && !status.contains("NOT verified"),
            "an available TLC pass must report a verified seal, got: {status}"
        );
    }

    /// The other honest branch: TLC available but the proof FAILS → refuse
    /// unconditionally, even under the opt-in flag (a rejected proof is
    /// known-unsafe).
    #[test]
    fn seal_gate_refuses_failed_proof_even_under_flag() {
        let dir = sealed_fixture();
        let spore = Spore::parse(SEALED).unwrap();
        let cache = cosmon_core::spore::InMemorySealVerdictCache::new();
        let tlc = cosmon_core::spore::FakeTlcRunner::available_with(
            cosmon_core::spore::TlcOutcome::Failed {
                detail: "Invariant Termination violated".to_string(),
            },
        );
        let err = gate_seal(&spore, dir.path(), true, &tlc, &cache).unwrap_err();
        assert!(format!("{err}").contains("FAILED"));
    }

    #[test]
    fn load_spore_accepts_directory() {
        let dir = fixture(SPORE);
        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();
        assert_eq!(spore.name, "demo");
        assert_eq!(manifest_dir, dir.path());
    }

    /// Over-refusal task-20260724-07bc: a relative spore reference from inside
    /// the spore directory produced a manifest dir of `""`/`.`, which the
    /// lexical containment guard treats as a prefix of EVERY path — refusing
    /// every germination. The dir must leave `load_spore` anchored to the cwd.
    #[test]
    fn anchor_manifest_dir_resolves_relative_references_against_the_cwd() {
        let cwd = Path::new("/repo/spores/cosmon-dev");
        // `cs spore run spore-starter.toml` — a bare filename's parent is "".
        assert_eq!(anchor_manifest_dir(PathBuf::from(""), cwd), cwd);
        // `cs spore run ./spore-starter.toml` — the parent is ".".
        assert_eq!(anchor_manifest_dir(PathBuf::from("."), cwd), cwd);
        // A relative subdirectory reference anchors under the cwd.
        assert_eq!(
            anchor_manifest_dir(PathBuf::from("nested"), cwd),
            Path::new("/repo/spores/cosmon-dev/nested")
        );
        // An absolute reference is already anchored and passes through.
        assert_eq!(
            anchor_manifest_dir(PathBuf::from("/abs/spores/x"), cwd),
            Path::new("/abs/spores/x")
        );
    }

    /// G2 frozen red (germ-20260802-037fc8ec): a bare relative spore reference
    /// and an absolute reference to the same manifest must yield the same
    /// containment decision for the ordinary run-home destination, and that
    /// decision must be `Allowed` (`None`). Contract P5 + P10.
    ///
    /// The assertion target is the composed resolver
    /// `forbidden_gate_output(node_output_dir(run_dir(..)), load_spore(ref).dir, repo_root)`
    /// — the real archived `load_spore` plus the real core path primitives. The
    /// oracle is the literal constant `None`; nothing under test computes the
    /// expected value.
    #[test]
    fn adr161_relative_and_absolute_references_decide_containment_identically() {
        use cosmon_core::spore::{forbidden_gate_output, node_output_dir, run_dir};

        // A repository fixture: `.cosmon/state` at the root, the manifest
        // beneath `spores/starter/` — the repro layout from task-20260724-07bc.
        let repo = tempfile::tempdir().unwrap();
        let repo_root = repo.path().canonicalize().unwrap();
        let spore_dir = repo_root.join("spores").join("starter");
        std::fs::create_dir_all(&spore_dir).unwrap();
        std::fs::create_dir_all(repo_root.join(".cosmon").join("state")).unwrap();
        std::fs::write(spore_dir.join("spore-starter.toml"), SPORE).unwrap();
        std::fs::write(spore_dir.join("work.formula.toml"), FORMULA).unwrap();

        // The ordinary run-home candidate every germination composes.
        let state_root = repo_root.join(".cosmon").join("state");
        let candidate =
            node_output_dir(&run_dir(&state_root, "germ-adr161-fixture"), "decompose").unwrap();

        // Absolute reference: the resolver hands back the manifest's parent.
        let (_, abs_dir) = load_spore(&spore_dir.join("spore-starter.toml")).unwrap();
        let abs_decision = forbidden_gate_output(&candidate, &abs_dir, &repo_root);

        // Bare relative reference resolved from inside the spore directory —
        // exactly `cd spores/starter && cs spore run spore-starter.toml`.
        // (Run with --test-threads=1: the cwd is process-global.)
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&spore_dir).unwrap();
        let rel = load_spore(Path::new("spore-starter.toml"));
        std::env::set_current_dir(previous).unwrap();
        let (_, rel_dir) = rel.unwrap();
        let rel_decision = forbidden_gate_output(&candidate, &rel_dir, &repo_root);

        // P10: the run home is an allowed destination (literal oracle).
        assert_eq!(abs_decision, None, "absolute reference must be Allowed");
        assert_eq!(rel_decision, None, "relative reference must be Allowed");
        // P5: two spellings of one manifest must decide identically.
        assert_eq!(
            rel_decision, abs_decision,
            "relative and absolute references must yield one decision"
        );
    }

    #[test]
    fn germinate_replays_calls_into_the_store_with_blocked_by() {
        let dir = fixture(SPORE);
        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();
        let params = coerce_vars(&spore, &["subject=octopus".to_string()]).unwrap();
        let calls = expand(&spore, &params).unwrap();

        let store = tempfile::tempdir().unwrap();
        let fleet = FleetId::new("default").unwrap();
        let results = germinate(
            &calls,
            &cosmon_core::fleet::CrossProviderReview::default(),
            &manifest_dir,
            store.path(),
            &fleet,
            None,
            100,
        )
        .unwrap();

        // frame + two fanout instances (default axes = ["a","b"]).
        assert_eq!(results.len(), 3);

        // The two analyse molecules must each carry a BlockedBy edge to the
        // frame molecule — proof the alias→id wiring landed on disk.
        use cosmon_state::StateStore;
        let fs = cosmon_filestore::FileStore::new(store.path());
        let frame_id = &results[0].id;
        let downstream = &results[1];
        let mol = fs.load_molecule(&downstream.id).unwrap();
        let blocks_on_frame = mol.typed_links.iter().any(|l| {
            matches!(
                l,
                cosmon_core::interaction::MoleculeLink::BlockedBy { source } if source == frame_id
            )
        });
        assert!(blocks_on_frame, "fanout instance must be blocked by frame");
    }

    /// ADR-161: a germinated node is HANDED its run-scoped output home. After
    /// `inject_run_outputs`, every call carries an `output_dir` var under the
    /// state store's `spore-runs/<germ>/` tree — never a path the worker must
    /// invent inside the spore definition dir. This mirrors what `run_run` does
    /// between `expand` and `germinate`.
    #[test]
    fn germination_hands_each_node_a_run_scoped_output_dir() {
        let dir = fixture(SPORE);
        let (spore, _manifest_dir) = load_spore(dir.path()).unwrap();
        let params = coerce_vars(&spore, &["subject=octopus".to_string()]).unwrap();
        let mut calls = expand(&spore, &params).unwrap();

        let state = tempfile::tempdir().unwrap();
        let run_dir = cosmon_core::spore::run_dir(state.path(), "germ-20260723-0000abcd");
        cosmon_core::spore::inject_run_outputs(&mut calls, &run_dir).unwrap();

        let run_str = run_dir.to_string_lossy().into_owned();
        assert!(
            run_str.contains("spore-runs/germ-20260723-0000abcd"),
            "run home must be namespaced under spore-runs/: {run_str}"
        );
        for call in &calls {
            let out = call
                .vars
                .get(cosmon_core::spore::OUTPUT_DIR_VAR)
                .expect("every node handed an output_dir");
            assert!(out.starts_with(&run_str), "output_dir {out} under run home");
            // Never the spore definition dir nor the repo root.
            assert!(
                cosmon_core::spore::forbidden_gate_output(
                    std::path::Path::new(out),
                    dir.path(),
                    state.path(),
                )
                .is_none(),
                "the handed output home must not be a forbidden destination: {out}"
            );
        }
    }

    #[test]
    fn bundle_hash_is_stable_and_prefixed() {
        let dir = fixture(SPORE);
        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();
        let files = bundle_files(&spore, &manifest_dir);
        let a = bundle_hash(&files, &manifest_dir);
        let b = bundle_hash(&files, &manifest_dir);
        assert_eq!(a, b);
        assert!(a.starts_with("blake3:"));
    }

    /// The integrity gap this molecule closes (sporarium recette v3.2 prise
    /// n°5): the falsifier is *two bundles differing only in `fleet.toml`
    /// sharing the same bundle id*. Covering the crew constitution in the
    /// hash makes that impossible — a fleet.toml edit shifts the id.
    #[test]
    fn bundle_hash_covers_fleet_toml() {
        let dir = fixture(SPORE);
        std::fs::write(
            dir.path().join("fleet.toml"),
            "fleet = \"crew-a\"\nversion = 1\n",
        )
        .unwrap();
        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();

        let files = bundle_files(&spore, &manifest_dir);
        assert!(
            files.iter().any(|f| f == "fleet.toml"),
            "fleet.toml must be in the coverage set; got {files:?}"
        );
        let before = bundle_hash(&files, &manifest_dir);

        // Alter ONLY the crew constitution — nothing else in the bundle.
        std::fs::write(
            dir.path().join("fleet.toml"),
            "fleet = \"crew-b\"\nversion = 1\n",
        )
        .unwrap();
        let after = bundle_hash(&bundle_files(&spore, &manifest_dir), &manifest_dir);

        assert_ne!(
            before, after,
            "editing fleet.toml must change the bundle id (integrity gap)"
        );
    }

    /// A `fleet.toml` composing from `file:` fleets must fold those includes
    /// into the coverage set too — otherwise the same gap reappears one level
    /// down (a sub-crew altered without moving the id).
    #[test]
    fn bundle_hash_covers_fleet_includes() {
        let dir = fixture(SPORE);
        std::fs::write(
            dir.path().join("fleet.toml"),
            "[fleet]\nschema_version = 1\nid = \"master\"\n\n\
             [[fleet.include]]\nsource = \"file:./crew/wiki.toml\"\nas = \"wiki\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("crew")).unwrap();
        let included = dir.path().join("crew/wiki.toml");
        std::fs::write(&included, "fleet = \"wiki\"\nversion = 1\n").unwrap();

        let (spore, manifest_dir) = load_spore(dir.path()).unwrap();
        let files = bundle_files(&spore, &manifest_dir);
        // The include path is folded in verbatim as the fleet resolver joins
        // it (`./`-prefix preserved) — what matters is that its bytes are bound.
        assert!(
            files.iter().any(|f| f.ends_with("crew/wiki.toml")),
            "a file: fleet include must be covered; got {files:?}"
        );
        let before = bundle_hash(&files, &manifest_dir);

        // Alter ONLY the included sub-crew.
        std::fs::write(&included, "fleet = \"wiki-v2\"\nversion = 1\n").unwrap();
        let after = bundle_hash(&bundle_files(&spore, &manifest_dir), &manifest_dir);
        assert_ne!(before, after, "editing an included fleet must move the id");
    }

    #[test]
    fn astra_marks_seal_unverified_and_lists_covered_files() {
        let spore = Spore::parse(SEALED).unwrap();
        let covered = vec!["spore.toml".to_string(), "work.formula.toml".to_string()];
        let astra = build_astra(&spore, "blake3:abc", &covered);
        let dataset = &astra["@graph"][1];
        assert_eq!(dataset["spore:seal"]["present"], serde_json::json!(true));
        assert_eq!(dataset["spore:seal"]["verified"], serde_json::json!(false));
        assert_eq!(
            dataset["spore:bundleFiles"],
            serde_json::json!(["spore.toml", "work.formula.toml"]),
            "ASTRA must record the coverage set for integrity audit"
        );
    }

    // -----------------------------------------------------------------
    // Unreachable per-step pins (task-20260725-eb3b)
    // -----------------------------------------------------------------

    const PINNED: &str = r#"
formula = "tiered"
version = 1
description = "A recipe that tiers its steps by model"
id_prefix = "ti"

[[steps]]
id = "think"
title = "Think"
description = "Reasoning step."
acceptance = "Done"
model = "claude-opus-5"
"#;

    const UNPINNED: &str = r#"
formula = "tiered"
version = 1
description = "Same recipe, no tiering"
id_prefix = "ti"

[[steps]]
id = "think"
title = "Think"
description = "Reasoning step."
acceptance = "Done"
"#;

    fn parse_formula(text: &str) -> cosmon_core::formula::Formula {
        cosmon_core::formula::Formula::parse(text).unwrap()
    }

    /// The observed failure: the bundle tiers its nodes, the mission registry
    /// has never heard of the recipe, and dispatch would run flat on the
    /// adapter default without saying so.
    #[test]
    fn pins_absent_from_registry_are_reported_with_the_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let warning = pin_warning_against_registry(&parse_formula(PINNED), dir.path())
            .expect("an absent recipe with pins must warn");
        assert!(warning.contains("not in the mission registry"), "{warning}");
        assert!(
            warning.contains("tiered"),
            "must name the recipe: {warning}"
        );
        assert!(
            warning.contains("tiered.formula.toml"),
            "must name the path to install to: {warning}"
        );
    }

    /// A recipe that pins nothing loses nothing — warning here would be pure
    /// noise on every ordinary spore.
    #[test]
    fn a_recipe_without_pins_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(pin_warning_against_registry(&parse_formula(UNPINNED), dir.path()).is_none());
    }

    /// Installed, byte-for-byte in agreement on its pins: nothing to say.
    #[test]
    fn a_registry_copy_with_the_same_pins_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tiered.formula.toml"), PINNED).unwrap();
        assert!(pin_warning_against_registry(&parse_formula(PINNED), dir.path()).is_none());
    }

    /// The subtler harm: a recipe of the same name *is* installed, so nothing
    /// looks missing, but it declares different pins — dispatch honours the
    /// registry's copy and the spore's tiering silently does not happen.
    #[test]
    fn a_shadowing_registry_copy_with_different_pins_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tiered.formula.toml"), UNPINNED).unwrap();
        let warning = pin_warning_against_registry(&parse_formula(PINNED), dir.path())
            .expect("a shadowing copy with different pins must warn");
        assert!(
            warning.contains("different per-step adapter/model pins"),
            "{warning}"
        );
    }
}
