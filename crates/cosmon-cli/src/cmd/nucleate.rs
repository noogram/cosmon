// SPDX-License-Identifier: AGPL-3.0-only

//! `cs nucleate` — instantiate a new molecule from a formula template.
//!
//! Finds the formula file, parses it, generates a unique molecule ID,
//! writes state.json + briefing.md + prompt.md + log.md, and emits a creation event.
//!
//! ## Hydration from declarations
//!
//! With `--from <PATH>`, the command hydrates molecules from git-trackable
//! [`MoleculeDeclaration`]
//! TOML files. `PATH` may be a single `.toml` file or a directory (all
//! `.toml` files inside are loaded). Each declaration references a formula
//! by name; the command resolves it in the usual formulas directory.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use chrono::Utc;
use cosmon_core::agent::AgentRole;
use cosmon_core::declaration::MoleculeDeclaration;
use cosmon_core::expiry::{parse_expires_at, parse_ttl, ExpiryPolicy};
use cosmon_core::fleet::FleetSpec;
use cosmon_core::formula::Formula;
use cosmon_core::id::ProjectId;
use cosmon_core::id::{FleetId, MoleculeId, WorkerId};
use cosmon_core::interaction::{CrossGalaxyRef, MoleculeLink};
use cosmon_core::interaction_mode::{InteractionMode, INTERACTION_MODE_TAG_KEY};
use cosmon_core::nucleate::{self, NucleateRequest, NucleateResult};
use cosmon_core::tag::Tag;
use cosmon_filestore::FileStore;
use cosmon_state::{BriefingSeal, MoleculeData, StateStore};

use super::Context;

/// Arguments for the `nucleate` subcommand.
///
/// Fields are `pub(crate)` so sibling command modules (notably
/// [`super::spark`]) can build an `Args` programmatically when they
/// wrap `cs nucleate` with hard-coded defaults. External callers go
/// through clap — they never touch the fields directly.
#[derive(clap::Args)]
pub struct Args {
    /// Formula name (looks for `{name}.formula.toml` in the formulas directory).
    ///
    /// Required unless `--from` is supplied.
    #[arg(required_unless_present = "from")]
    pub(crate) formula: Option<String>,

    /// Hydrate molecule(s) from a TOML declaration file or directory.
    ///
    /// When `PATH` is a directory, every `*.toml` file inside (non-recursive,
    /// sorted) is loaded as a [`MoleculeDeclaration`]. The positional
    /// `formula` argument is ignored in this mode — each declaration
    /// carries its own `formula` field.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["formula", "vars", "assign", "kind", "blocks", "blocked_by", "decayed_from", "no_parent", "refines", "refutes"],
    )]
    pub(crate) from: Option<PathBuf>,

    /// Target molecule(s) that this new molecule blocks — each target
    /// cannot progress until this one completes.
    ///
    /// Accepts two forms:
    /// - `<molecule-id>` — same-galaxy edge. Adds `Blocks` here and a
    ///   symmetric `BlockedBy` on the target. Targets must already exist.
    /// - `<galaxy-alias>:<molecule-id>` (or `<galaxy-alias>@<molecule-id>`) —
    ///   cross-galaxy edge (Phase 1, ADR-035). The remote target is
    ///   resolved best-effort via configured galaxy aliases or the cluster
    ///   root. The reciprocal edge is **not** filed
    ///   on the target galaxy (one-writer-per-galaxy, ADR-052) — the
    ///   edge is recorded locally and a stderr warning is emitted if
    ///   the target is unreachable.
    #[arg(long = "blocks", value_name = "MOLECULE_REF")]
    pub(crate) blocks: Vec<String>,

    /// Source molecule(s) that block this new molecule — this new molecule
    /// cannot progress until each source completes.
    ///
    /// Same syntax as `--blocks`, including the `<alias>:<mol_id>`
    /// cross-galaxy form (Phase 1, ADR-035).
    #[arg(long = "blocked-by", value_name = "MOLECULE_REF")]
    pub(crate) blocked_by: Vec<String>,

    /// Declare that the new molecule decayed from `PARENT_ID` — an
    /// information edge (not a progression edge): the parent is the
    /// cognitive source this molecule emerged from, but the parent is
    /// free to keep advancing independently. Symmetric counterpart:
    /// the parent gains a `DecayProduct` link to the new molecule.
    ///
    /// This is the explicit form of the auto-parent contract: when a
    /// worker `cs tackle`s a molecule, `COSMON_PARENT_MOL_ID` is
    /// injected into its environment, and any subsequent `cs nucleate`
    /// from that worker auto-populates this flag unless one of
    /// `--blocks`, `--blocked-by`, `--decayed-from`, or `--no-parent`
    /// is already set. Passing it explicitly wins over the env var.
    #[arg(long = "decayed-from", value_name = "MOLECULE_ID")]
    pub(crate) decayed_from: Option<String>,

    /// Disable the env-driven auto-parent contract for this invocation.
    ///
    /// When set, the `COSMON_PARENT_MOL_ID` environment variable is
    /// ignored and no `DecayedFrom` edge is synthesized. Use this for
    /// legitimate orphan nucleations (e.g., a worker that intentionally
    /// spawns an unrelated top-level molecule).
    #[arg(long = "no-parent")]
    pub(crate) no_parent: bool,

    /// Cited molecule(s) that this new molecule refines (semantic citation
    /// edge — does NOT carry progression semantics, unlike `--blocks`).
    ///
    /// For every target, the new molecule gets a `Refines` link and the
    /// target gets a symmetric `RefinedBy` link. Intended for
    /// [`Constellation`](cosmon_core::kind::MoleculeKind::Constellation)
    /// molecules that name a fil-rouge across N existing molecules.
    /// Repeat the flag per citation; targets must already exist.
    ///
    /// Also auto-populated for `--kind constellation` from a
    /// comma-separated `--var citations=mol1,mol2,mol3`.
    #[arg(long = "refines", value_name = "MOLECULE_ID")]
    pub(crate) refines: Vec<String>,

    /// Diagnosis molecule(s) that this new molecule refutes (semantic
    /// refutation edge — no progression semantics, unlike `--blocks`).
    ///
    /// For every target, the new molecule gets a `Refutes` link and the
    /// target gets a symmetric `RefutedBy` link. This is the DAG-native
    /// form of the ADR-143 diagnosis-verify gate: a `cmb-verify` molecule that reproduces a
    /// relayed symptom but finds the stated *mechanism* describes a
    /// nonexistent code path records the divergence by refuting the
    /// diagnosis molecule. Repeat the flag per refuted diagnosis; targets
    /// must already exist.
    #[arg(long = "refutes", value_name = "MOLECULE_ID")]
    pub(crate) refutes: Vec<String>,

    /// Fleet to nucleate the molecule into (default: "default")
    #[arg(long, default_value = "default")]
    pub(crate) fleet: String,

    /// Molecule kind: idea, task, decision, issue, signal, deliberation,
    /// constellation (see `docs/guides/constellation-pattern.md`).
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: Option<String>,

    /// Operational class — `standard` (default), `stress-test`, or `infra`
    /// (ADR-085 §1).
    ///
    /// `stress-test` opts the molecule into the two-layer pre-commitment
    /// seal at dispatch (Layer 1 runtime precondition + Layer 2
    /// witness-quorum, ADR-085 §2-§3) and out of autopilot drain. The
    /// remaining classes are gate-equivalent to the legacy default; this
    /// flag is a marker, not a runtime mode.
    #[arg(long, value_name = "CLASS")]
    pub(crate) class: Option<String>,

    /// Assign a worker to the new molecule
    #[arg(long)]
    pub(crate) assign: Option<String>,

    /// Set a variable (repeatable: --var key=value)
    #[arg(long = "var", value_name = "KEY=VALUE")]
    pub(crate) vars: Vec<String>,

    /// Path to the formulas directory (default: ./formulas)
    #[arg(long, value_name = "DIR")]
    pub(crate) formulas_dir: Option<PathBuf>,

    /// Agent role for the worker that will tackle this molecule.
    ///
    /// Valid roles: orchestration, research, implementation, infrastructure,
    /// advisory, validation. When set, `cs tackle` uses this role instead of
    /// the default `implementation`.
    #[arg(long, value_name = "ROLE")]
    pub(crate) role: Option<String>,

    /// Path to the state store root (default: .cosmon)
    #[arg(long, value_name = "DIR")]
    pub(crate) store_dir: Option<PathBuf>,

    /// Typed label to attach to the new molecule (repeatable).
    ///
    /// Format: `key` or `key:value`. Keys are kebab-case; values exclude
    /// whitespace and `:`. Duplicate tags are deduplicated.
    #[arg(long = "tag", value_name = "TAG")]
    pub(crate) tags: Vec<String>,

    /// Static interaction-mode discriminant posed at nucleation.
    ///
    /// One of `operator-required` or `background`. Recorded as the
    /// `interaction-mode:<mode>` tag. Posed by the molecule's author —
    /// often an agent — and read at dispatch ; survives the operator's
    /// present state. *Default explicit, not implicit* : when absent,
    /// the tag is simply not set, and consumers (the graceful
    /// degradation controller, in particular) decide what to do.
    ///
    /// Conflicts with `--tag interaction-mode:*` to keep the
    /// discriminant single-sourced.
    #[arg(long = "interaction-mode", value_name = "MODE")]
    pub(crate) interaction_mode: Option<String>,

    /// Grant the operator-block capability at an irreversibility boundary
    /// (ADR-123 Q5).
    ///
    /// One of `signature`, `external-send`, `publish`,
    /// `authoritative-value`. Recorded as the `op-block:<boundary>` tag.
    /// A worker reads this single typed capability to decide whether it
    /// MAY pause for an operator (`cs await-operator`) at that boundary —
    /// or, when absent, MUST surface-and-continue. The capability is
    /// granted here at nucleation and never self-asserted by the worker.
    ///
    /// Conflicts with `--tag op-block:*` to keep the grant single-sourced.
    #[arg(long = "may-block-on-operator", value_name = "BOUNDARY")]
    pub(crate) may_block_on_operator: Option<String>,

    /// Relative TTL — deadline is now + duration (ADR-029).
    ///
    /// Grammar: `<N><unit>` where unit ∈ {s,m,h,d,w}. Examples: `7d`, `24h`,
    /// `2w`. Mutually exclusive with `--expires-at`.
    #[arg(long = "ttl", value_name = "DURATION", conflicts_with = "expires_at")]
    pub(crate) ttl: Option<String>,

    /// Absolute expiry instant (ADR-029).
    ///
    /// Accepts RFC3339 (`2026-07-02T00:00:00Z`) or a plain `YYYY-MM-DD`
    /// date (anchored at end-of-day 23:59:59Z). Mutually exclusive with
    /// `--ttl`.
    #[arg(long = "expires-at", value_name = "WHEN")]
    pub(crate) expires_at: Option<String>,

    /// Expiry policy — what to do when `expires_at` is in the past.
    ///
    /// One of `warn`, `collapse`, `escalate`. Defaults to the per-kind
    /// default from config (or `warn`) when unset. Only meaningful when
    /// `--ttl` or `--expires-at` is also provided.
    #[arg(long = "expiry-policy", value_name = "POLICY")]
    pub(crate) expiry_policy: Option<String>,

    /// Per-molecule step counter circuit breaker (THESIS Part XI).
    ///
    /// `cs evolve` decrements this once per step. At zero, the next attempt
    /// transitions the molecule to `Frozen` with reason `"energy-exhausted"`.
    /// Default comes from `.cosmon/config.toml` `[energy] default_step_budget`
    /// (100 if absent). Pass `0` to disable the breaker for this molecule.
    #[arg(long = "energy-budget", value_name = "N")]
    pub(crate) energy_budget: Option<u32>,

    /// Refuse to nucleate into the host-global `~/.cosmon/state` fleet.
    ///
    /// By default, running `cs nucleate` from a directory with no
    /// `.cosmon/config.toml` in cwd or any ancestor falls back to the
    /// host-global state dir (`$HOME/.cosmon/state`) and prints a warning to
    /// stderr — the molecule is born into a fleet invisible to every galaxy.
    /// `--require-galaxy` turns that warning into a hard error (exit ≠ 0),
    /// so scripts and tooling that must write galaxy-scoped state can fail
    /// fast instead of silently leaking orphans.
    #[arg(long = "require-galaxy")]
    pub(crate) require_galaxy: bool,
}

impl Args {
    /// Build an `Args` seeded with a formula name and defaults everywhere
    /// else. Programmatic callers (e.g. `cs spark`) mutate the resulting
    /// struct before passing it to [`run`] — no public `new()` because
    /// external users go through clap.
    pub(crate) fn for_formula(formula_name: &str) -> Self {
        Self {
            formula: Some(formula_name.to_owned()),
            from: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            decayed_from: None,
            no_parent: false,
            refines: Vec::new(),
            refutes: Vec::new(),
            fleet: "default".to_owned(),
            kind: None,
            class: None,
            assign: None,
            vars: Vec::new(),
            formulas_dir: None,
            role: None,
            store_dir: None,
            tags: Vec::new(),
            interaction_mode: None,
            may_block_on_operator: None,
            ttl: None,
            expires_at: None,
            expiry_policy: None,
            energy_budget: None,
            require_galaxy: false,
        }
    }
}

/// Surface the silent home-global state fallback at the moment a molecule
/// would be created.
///
/// When walk-up finds no galaxy in cwd or any ancestor, `cs nucleate` falls
/// back to `$HOME/.cosmon/state` — a host-global fleet invisible to every
/// galaxy. That fallback is intentional for host-level state (scheduler,
/// patrol, daemon supervisor; ADR-069) but births molecules into an orphan
/// trap when an operator or script runs from the wrong cwd.
///
/// Behaviour by origin (see [`cosmon_filestore::StateDirOrigin`]):
/// - [`Project`](cosmon_filestore::StateDirOrigin::Project),
///   [`Explicit`](cosmon_filestore::StateDirOrigin::Explicit),
///   [`Env`](cosmon_filestore::StateDirOrigin::Env): the destination was
///   chosen deliberately — stay silent.
/// - [`GlobalFallback`](cosmon_filestore::StateDirOrigin::GlobalFallback):
///   print a non-blocking warning to stderr; with `--require-galaxy`, fail
///   fast with a non-zero exit instead.
///
/// # Errors
/// Returns an error only when `require_galaxy` is set **and** the resolution
/// fell back to the host-global state dir.
fn guard_global_fallback(
    origin: cosmon_filestore::StateDirOrigin,
    store_dir: &Path,
    require_galaxy: bool,
) -> anyhow::Result<()> {
    if origin != cosmon_filestore::StateDirOrigin::GlobalFallback {
        return Ok(());
    }
    if require_galaxy {
        anyhow::bail!(
            "no galaxy in cwd or ancestors — refusing to nucleate into the \
             host-global fleet at {} (--require-galaxy). Run `cs init` here or \
             `cd` into a galaxy.",
            store_dir.display()
        );
    }
    eprintln!(
        "warning: no .cosmon/ found in cwd or ancestors — nucleating into the \
         host-global fleet at {} (invisible to every galaxy). Run `cs init` \
         here or `cd` into a galaxy to scope this molecule.",
        store_dir.display()
    );
    Ok(())
}

/// Execute the `nucleate` command.
///
/// # Errors
/// Returns an error string if the formula cannot be found or parsed,
/// required variables are missing, or persistence fails.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    // Fire-and-forget neurion auto-register hint. Non-fatal on failure.
    crate::neurion_hint::emit_for_cwd("cosmon:nucleate");

    let formulas_dir = cosmon_filestore::resolve_formulas_dir(args.formulas_dir.as_deref());
    let (store_dir, store_origin) =
        cosmon_filestore::resolve_state_dir_with_origin(args.store_dir.as_deref());
    guard_global_fallback(store_origin, &store_dir, args.require_galaxy)?;
    let fleet_id =
        FleetId::new(&args.fleet).map_err(|e| anyhow::anyhow!("invalid fleet id: {e}"))?;

    // Resolve project_id and energy defaults from config — graceful fallback
    // to defaults for legacy projects.
    let config_path = super::resolve_config_from_context(ctx);
    let loaded_config = cosmon_filestore::load_project_config(&config_path).ok();
    let project_id = loaded_config
        .as_ref()
        .and_then(|c| c.project.project_id.clone());
    let energy_default = loaded_config
        .as_ref()
        .map_or(100, |c| c.energy.default_step_budget);

    let role = args
        .role
        .as_deref()
        .map(str::parse::<AgentRole>)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid role: {e}"))?;

    // Resolve the per-molecule step budget: CLI override beats project
    // default. `0` (either source) disables the breaker for this molecule.
    let energy_budget_cap = args.energy_budget.unwrap_or(energy_default);

    if let Some(ref from_path) = args.from {
        run_from_declarations(
            ctx,
            from_path,
            &formulas_dir,
            &store_dir,
            &fleet_id,
            project_id.as_ref(),
            energy_default,
        )
    } else {
        let formula_name = args
            .formula
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("formula name is required (or use --from <PATH>)"))?;
        let (blocks, cross_blocks) = parse_link_refs(&args.blocks, "--blocks")?;
        let (blocked_by, cross_blocked_by) = parse_link_refs(&args.blocked_by, "--blocked-by")?;
        let vars_parsed = parse_vars(&args.vars)?;
        let class = args
            .class
            .as_deref()
            .map(str::parse::<cosmon_core::molecule_class::MoleculeClass>)
            .transpose()
            .map_err(|e| anyhow::anyhow!("invalid --class: {e}"))?
            .unwrap_or_default();
        // ADR-085 §M5 — delib-prep keyword lint. With M4 the parsed class
        // suppresses the warning when the operator has correctly opted in
        // via `--class stress-test`.
        if let Some(warning) = super::stress_test_lint::lint_deep_think(
            formula_name,
            vars_parsed.get("question").map(String::as_str),
            class,
        ) {
            eprintln!("{warning}");
        }
        let refines = resolve_refines(&args.refines, args.kind.as_deref(), &vars_parsed)?;
        let refutes = parse_molecule_ids(&args.refutes, "--refutes")?;
        let tags = parse_tags(&args.tags)?;
        let tags = merge_interaction_mode(tags, args.interaction_mode.as_deref())?;
        let tags = merge_operator_block(tags, args.may_block_on_operator.as_deref())?;
        let tags = merge_fleet_review(tags, &store_dir)?;
        let expires_at = resolve_expires_at(args.ttl.as_deref(), args.expires_at.as_deref())?;
        let expiry_policy = args
            .expiry_policy
            .as_deref()
            .map(parse_expiry_policy)
            .transpose()?;
        // The auto-parent contract steps aside whenever the operator
        // already declared an explicit edge — local OR cross-galaxy.
        // We carry the cross-galaxy counts in by treating any explicit
        // edge as "operator already wired the lineage".
        let any_explicit_blocks = !blocks.is_empty() || !cross_blocks.is_empty();
        let any_explicit_blocked_by = !blocked_by.is_empty() || !cross_blocked_by.is_empty();
        let decayed_from = resolve_decayed_from_explicit(
            args,
            any_explicit_blocks,
            any_explicit_blocked_by,
            &read_parent_env,
        )?;
        // b22c guard: when a worker nucleates from inside a formula step
        // that declares `requires_parent_link = true`, require that the
        // child carry an explicit `--blocks` or `--blocked-by` edge. The
        // parent handle comes from the worker's `COSMON_PARENT_MOL_ID`
        // env var (the same channel used by the auto-parent contract).
        if !args.no_parent {
            if let Some(parent_raw) = read_parent_env() {
                if let Ok(parent_id) = MoleculeId::new(&parent_raw) {
                    // Cross-galaxy edges count for the parent-link guard
                    // — they are still typed edges declared by the
                    // operator, just pointing into another galaxy.
                    enforce_parent_link_guard(
                        &store_dir,
                        &formulas_dir,
                        &parent_id,
                        blocks.len() + cross_blocks.len(),
                        blocked_by.len() + cross_blocked_by.len(),
                    )?;
                    // godel ordinal stratification (smithy ADR-0021,
                    // delib-20260523-a682; cosmon ADR-110 §I4): a worker may
                    // nucleate a decomposer (tier ≥ 1) only at a strictly
                    // lower tier than its own. DecayProduct continuations are
                    // peer-level by construction — exempt — so the guard is
                    // skipped when this nucleation is a decay product.
                    if decayed_from.is_none() {
                        enforce_tier_descent_guard(
                            &store_dir,
                            &formulas_dir,
                            &parent_id,
                            formula_name,
                        )?;
                    }
                }
            }
        }
        run_single(
            ctx,
            formula_name,
            &formulas_dir,
            &store_dir,
            &fleet_id,
            vars_parsed,
            args.assign.as_deref(),
            args.kind.as_deref(),
            class,
            role,
            &[],
            &blocks,
            &blocked_by,
            &cross_blocks,
            &cross_blocked_by,
            &refines,
            &refutes,
            decayed_from.as_ref(),
            project_id.as_ref(),
            &tags,
            expires_at,
            expiry_policy,
            energy_budget_cap,
        )
    }
}

/// Resolve the effective `--refines` target list.
///
/// Starts from explicit `--refines` CLI flags. If `--kind constellation` is
/// set, additionally parses a comma-separated `citations` variable (trimmed)
/// and appends any targets not already covered by the explicit flags. This
/// is the mechanism that makes `cs nucleate constellation --var
/// citations="a,b,c"` emit three `Refines` edges without requiring the
/// operator to also pass `--refines a --refines b --refines c`.
fn resolve_refines(
    explicit: &[String],
    kind: Option<&str>,
    variables: &HashMap<String, String>,
) -> anyhow::Result<Vec<MoleculeId>> {
    let mut out: Vec<MoleculeId> = parse_molecule_ids(explicit, "--refines")?;
    let is_constellation = kind.and_then(|k| k.parse::<cosmon_core::kind::MoleculeKind>().ok())
        == Some(cosmon_core::kind::MoleculeKind::Constellation);
    if !is_constellation {
        return Ok(out);
    }
    let Some(raw) = variables.get("citations") else {
        return Ok(out);
    };
    for chunk in raw.split(',') {
        let trimmed = chunk.trim();
        if trimmed.is_empty() {
            continue;
        }
        let id = MoleculeId::new(trimmed)
            .map_err(|e| anyhow::anyhow!("invalid --var citations entry `{trimmed}`: {e}"))?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Environment lookup for `COSMON_PARENT_MOL_ID`. Factored behind a
/// closure so tests can substitute a deterministic reader without
/// poisoning the process env (which the test harness runs in parallel).
fn read_parent_env() -> Option<String> {
    std::env::var("COSMON_PARENT_MOL_ID").ok()
}

/// Resolve the effective `--decayed-from` target for a single-formula
/// nucleation, applying the auto-parent contract (ADR-037 lineage
/// conservation). Precedence:
///
/// 1. `--no-parent` → always `None`.
/// 2. Explicit `--decayed-from` → use it verbatim.
/// 3. Any explicit `--blocks` / `--blocked-by` → `None` (the operator
///    already declared an edge; the env layer stays silent so we do
///    not silently add a second edge on top of an explicit contract).
/// 4. `COSMON_PARENT_MOL_ID` env var set → parse and return it, and
///    emit a stderr hint so the operator can see the implicit edge.
/// 5. Otherwise → `None`.
///
/// The parser is passed in so unit tests can stub it; the production
/// caller uses [`read_parent_env`].
#[cfg(test)]
fn resolve_decayed_from(
    args: &Args,
    blocks: &[MoleculeId],
    blocked_by: &[MoleculeId],
    env_reader: &dyn Fn() -> Option<String>,
) -> anyhow::Result<Option<MoleculeId>> {
    resolve_decayed_from_explicit(args, !blocks.is_empty(), !blocked_by.is_empty(), env_reader)
}

/// Same logic as `resolve_decayed_from` but parameterised on the
/// boolean "any explicit edge?" so cross-galaxy edges (which do not
/// fit the `Vec<MoleculeId>` shape) can also silence the env layer.
fn resolve_decayed_from_explicit(
    args: &Args,
    any_explicit_blocks: bool,
    any_explicit_blocked_by: bool,
    env_reader: &dyn Fn() -> Option<String>,
) -> anyhow::Result<Option<MoleculeId>> {
    if args.no_parent {
        return Ok(None);
    }
    if let Some(ref raw) = args.decayed_from {
        let id = MoleculeId::new(raw)
            .map_err(|e| anyhow::anyhow!("invalid --decayed-from molecule id `{raw}`: {e}"))?;
        return Ok(Some(id));
    }
    if any_explicit_blocks || any_explicit_blocked_by {
        return Ok(None);
    }
    let Some(raw) = env_reader() else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let id = MoleculeId::new(&raw).map_err(|e| {
        anyhow::anyhow!(
            "COSMON_PARENT_MOL_ID=`{raw}` is not a valid molecule id: {e} \
             (pass --no-parent to disable the auto-parent contract)"
        )
    })?;
    eprintln!(
        "auto-linked to parent {id} via DecayProduct \
         (pass --no-parent to disable)"
    );
    Ok(Some(id))
}

/// Enforce the b22c parent-link contract (see
/// [`super::guard::ensure_parent_link_when_required`]). Loads the parent
/// molecule's state and formula from disk on every invocation — the
/// formula lookup is intentionally fresh so operators can hot-patch a
/// `requires_parent_link` flag without restarting the worker.
fn enforce_parent_link_guard(
    store_dir: &Path,
    formulas_dir: &Path,
    parent_id: &MoleculeId,
    blocks_len: usize,
    blocked_by_len: usize,
) -> anyhow::Result<()> {
    let store = FileStore::new(store_dir);
    // A missing parent or unreadable state is not fatal — the existing
    // nucleate path still has its own validation. We only enforce the
    // guard when we can see enough context to prove it should fire.
    let Ok(parent_mol) = store.load_molecule(parent_id) else {
        return Ok(());
    };
    let formula_name = parent_mol.formula_id.as_str();
    let parent_formula = load_formula(formulas_dir, formula_name).ok();
    super::guard::ensure_parent_link_when_required(
        parent_id,
        &parent_mol,
        parent_formula.as_ref(),
        blocks_len,
        blocked_by_len,
    )
    .map_err(anyhow::Error::from)
}

/// Enforce the godel ordinal stratification rule (see
/// [`super::guard::ensure_tier_descends`]). Loads both the parent
/// molecule's formula and the child formula being nucleated, then checks
/// that creating a decomposer (tier ≥ 1) strictly descends the tier.
///
/// Leniency matches [`enforce_parent_link_guard`]: a missing parent, an
/// unreadable parent state, or an unloadable formula on either side is
/// non-fatal (`Ok`). We only refuse when we can see enough context to
/// prove the descent rule is broken.
fn enforce_tier_descent_guard(
    store_dir: &Path,
    formulas_dir: &Path,
    parent_id: &MoleculeId,
    child_formula_name: &str,
) -> anyhow::Result<()> {
    let store = FileStore::new(store_dir);
    let Ok(parent_mol) = store.load_molecule(parent_id) else {
        return Ok(());
    };
    let Ok(parent_formula) = load_formula(formulas_dir, parent_mol.formula_id.as_str()) else {
        return Ok(());
    };
    let Ok(child_formula) = load_formula(formulas_dir, child_formula_name) else {
        return Ok(());
    };
    super::guard::ensure_tier_descends(parent_id, &parent_formula.tier, &child_formula.tier)
        .map_err(anyhow::Error::from)
}

/// Resolve the effective `expires_at` from mutually-exclusive `--ttl` and
/// `--expires-at` flags. Relative TTLs anchor on the current wall clock.
fn resolve_expires_at(
    ttl: Option<&str>,
    expires_at: Option<&str>,
) -> anyhow::Result<Option<DateTime<chrono::Utc>>> {
    match (ttl, expires_at) {
        (Some(_), Some(_)) => Err(anyhow::anyhow!(
            "--ttl and --expires-at are mutually exclusive"
        )),
        (Some(d), None) => {
            let dur = parse_ttl(d).map_err(|e| anyhow::anyhow!("invalid --ttl: {e}"))?;
            Ok(Some(chrono::Utc::now() + dur))
        }
        (None, Some(s)) => parse_expires_at(s)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("{e}")),
        (None, None) => Ok(None),
    }
}

/// Parse `--expiry-policy` into [`ExpiryPolicy`].
fn parse_expiry_policy(s: &str) -> anyhow::Result<ExpiryPolicy> {
    match s.trim().to_ascii_lowercase().as_str() {
        "warn" => Ok(ExpiryPolicy::Warn),
        "collapse" => Ok(ExpiryPolicy::Collapse),
        "escalate" => Ok(ExpiryPolicy::Escalate),
        other => Err(anyhow::anyhow!(
            "invalid --expiry-policy `{other}` (expected warn|collapse|escalate)"
        )),
    }
}

/// Parse a list of raw strings into validated [`Tag`]s.
fn parse_tags(raw: &[String]) -> anyhow::Result<Vec<Tag>> {
    raw.iter()
        .map(|s| Tag::new(s.clone()).map_err(|e| anyhow::anyhow!("invalid --tag `{s}`: {e}")))
        .collect()
}

/// Fold `--interaction-mode <MODE>` into a tag list.
///
/// The flag is the typed front-door for the `interaction-mode:<value>` tag.
/// We refuse to silently coexist with a hand-written `--tag
/// interaction-mode:*` of the same key — *Default explicit, not implicit*
/// means the discriminant is single-sourced. The flag spelling wins by
/// convention, but the conflict is surfaced as an error rather than a
/// silent override.
fn merge_interaction_mode(
    mut tags: Vec<Tag>,
    interaction_mode: Option<&str>,
) -> anyhow::Result<Vec<Tag>> {
    let preexisting = tags.iter().any(|t| t.key() == INTERACTION_MODE_TAG_KEY);
    let Some(raw) = interaction_mode else {
        return Ok(tags);
    };
    if preexisting {
        return Err(anyhow::anyhow!(
            "--interaction-mode conflicts with --tag {INTERACTION_MODE_TAG_KEY}:* — \
             pass exactly one of the two to keep the discriminant single-sourced"
        ));
    }
    let mode: InteractionMode = raw
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --interaction-mode `{raw}`: {e}"))?;
    tags.push(mode.to_tag());
    Ok(tags)
}

/// Fold `--may-block-on-operator <BOUNDARY>` into a tag list (ADR-123 Q5).
///
/// The flag is the typed front-door for the `op-block:<boundary>` tag —
/// the operator-block capability, granted once at nucleation and read by
/// a worker to decide whether it MAY pause for an operator. Same
/// single-sourced discipline as `--interaction-mode`: a hand-written
/// `--tag op-block:*` of the same key conflicts, surfaced as an error
/// rather than a silent override.
fn merge_operator_block(
    mut tags: Vec<Tag>,
    may_block_on_operator: Option<&str>,
) -> anyhow::Result<Vec<Tag>> {
    let preexisting = tags
        .iter()
        .any(|t| t.key() == cosmon_core::operator_block::CAPABILITY_TAG_KEY);
    let Some(raw) = may_block_on_operator else {
        return Ok(tags);
    };
    if preexisting {
        return Err(anyhow::anyhow!(
            "--may-block-on-operator conflicts with --tag {}:* — pass exactly one \
             to keep the capability single-sourced",
            cosmon_core::operator_block::CAPABILITY_TAG_KEY
        ));
    }
    let boundary: cosmon_core::operator_block::IrreversibleBoundary = raw
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --may-block-on-operator `{raw}`: {e}"))?;
    tags.push(cosmon_core::operator_block::OperatorBlockCapability::new(boundary).to_tag());
    Ok(tags)
}

/// Project the fleet's explicit review opt-in onto newly nucleated work.
///
/// The absence of `[review] cross_provider = true` is intentionally a no-op:
/// the policy is opt-in, not inferred from a task's criticality.  The generic
/// tag keeps RR-SAFE-2's existing authority reservation, while the specific
/// tag tells the pilot which independent-review path must clear it.
fn merge_fleet_review(mut tags: Vec<Tag>, store_dir: &Path) -> anyhow::Result<Vec<Tag>> {
    let path = store_dir.parent().unwrap_or(store_dir).join("fleet.toml");
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(tags);
    };
    let spec =
        FleetSpec::parse(&text).map_err(|e| anyhow::anyhow!("invalid fleet review policy: {e}"))?;
    if !spec.review.cross_provider {
        return Ok(tags);
    }
    for raw in ["needs-review", "needs-review-cross-provider"] {
        let tag = Tag::new(raw).expect("static review tag is valid");
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    if let Some(adapter) = spec.review.reviewer_adapter {
        let tag = Tag::new(format!("reviewer-adapter:{adapter}"))
            .map_err(|e| anyhow::anyhow!("invalid fleet reviewer_adapter `{adapter}`: {e}"))?;
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    Ok(tags)
}

/// Parse a list of raw strings into validated `MoleculeId`s, reporting
/// which flag produced an invalid value.
fn parse_molecule_ids(raw: &[String], flag: &str) -> anyhow::Result<Vec<MoleculeId>> {
    raw.iter()
        .map(|s| {
            MoleculeId::new(s).map_err(|e| anyhow::anyhow!("invalid {flag} molecule id `{s}`: {e}"))
        })
        .collect()
}

/// Split a raw `--blocks` or `--blocked-by` argument list into local
/// molecule IDs and cross-galaxy references (Phase 1 of ADR-035).
///
/// A token containing `:` or `@` is interpreted as `<alias>:<mol_id>`
/// and parsed via `CrossGalaxyRef::from_str`. A token with neither
/// separator is treated as a local [`MoleculeId`]. This mirrors the
/// way `git remote` accepts both `origin` and `git@host:repo` in the
/// same flag — the operator does not have to flip a CLI mode to point
/// at another galaxy.
fn parse_link_refs(
    raw: &[String],
    flag: &str,
) -> anyhow::Result<(Vec<MoleculeId>, Vec<CrossGalaxyRef>)> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for token in raw {
        if CrossGalaxyRef::looks_like_cross_galaxy(token) {
            let cgr: CrossGalaxyRef = token.parse().map_err(|e| {
                anyhow::anyhow!("invalid {flag} cross-galaxy reference `{token}`: {e}")
            })?;
            remote.push(cgr);
        } else {
            let mid = MoleculeId::new(token)
                .map_err(|e| anyhow::anyhow!("invalid {flag} molecule id `{token}`: {e}"))?;
            local.push(mid);
        }
    }
    Ok((local, remote))
}

/// Nucleate a single molecule from a formula name (the classic CLI path).
#[allow(clippy::too_many_arguments)]
fn run_single(
    ctx: &Context,
    formula_name: &str,
    formulas_dir: &Path,
    store_dir: &Path,
    fleet_id: &FleetId,
    variables: HashMap<String, String>,
    assign: Option<&str>,
    kind: Option<&str>,
    class: cosmon_core::molecule_class::MoleculeClass,
    role: Option<AgentRole>,
    links: &[String],
    blocks: &[MoleculeId],
    blocked_by: &[MoleculeId],
    cross_blocks: &[CrossGalaxyRef],
    cross_blocked_by: &[CrossGalaxyRef],
    refines: &[MoleculeId],
    refutes: &[MoleculeId],
    decayed_from: Option<&MoleculeId>,
    project_id: Option<&ProjectId>,
    tags: &[Tag],
    expires_at: Option<DateTime<chrono::Utc>>,
    expiry_policy: Option<ExpiryPolicy>,
    energy_budget_cap: u32,
) -> anyhow::Result<()> {
    let formula = load_formula(formulas_dir, formula_name)?;
    let (result, _path) = nucleate_and_persist(
        &formula,
        variables,
        assign,
        kind,
        class,
        role,
        fleet_id,
        store_dir,
        links,
        blocks,
        blocked_by,
        cross_blocks,
        cross_blocked_by,
        refines,
        refutes,
        decayed_from,
        project_id,
        tags,
        expires_at,
        expiry_policy,
        energy_budget_cap,
    )?;
    emit_output(ctx, std::slice::from_ref(&result));
    Ok(())
}

/// Hydrate one or more molecules from `MoleculeDeclaration` TOML files.
///
/// `from_path` may be a single file or a directory. When it's a directory,
/// all `*.toml` files inside (non-recursive, sorted by file name) are loaded.
/// A partial failure short-circuits: the first error aborts processing so
/// the operator can fix the offending declaration before retrying.
fn run_from_declarations(
    ctx: &Context,
    from_path: &Path,
    formulas_dir: &Path,
    store_dir: &Path,
    fleet_id: &FleetId,
    project_id: Option<&ProjectId>,
    energy_budget_cap: u32,
) -> anyhow::Result<()> {
    let decl_paths = collect_declaration_paths(from_path)?;
    if decl_paths.is_empty() {
        return Err(anyhow::anyhow!(
            "no declarations found at {}",
            from_path.display()
        ));
    }

    let mut results = Vec::with_capacity(decl_paths.len());
    for decl_path in &decl_paths {
        let content = std::fs::read_to_string(decl_path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", decl_path.display()))?;
        let declaration = MoleculeDeclaration::parse(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", decl_path.display()))?;

        let mut formula = load_formula(formulas_dir, &declaration.formula)
            .map_err(|e| anyhow::anyhow!("{}: {e}", decl_path.display()))?;

        // Declaration's id_prefix (when set) overrides the formula's.
        if !declaration.id_prefix.is_empty() {
            formula.id_prefix.clone_from(&declaration.id_prefix);
        }

        let (result, _mol_path) = nucleate_and_persist(
            &formula,
            declaration.variables.clone(),
            declaration.assign.as_deref(),
            declaration.kind.as_deref(),
            // Declarations don't carry --class today; default to Standard.
            // Stress-test molecules must be declared at the CLI to ratify
            // the operator-intent invariant in ADR-085 §1.
            cosmon_core::molecule_class::MoleculeClass::default(),
            None, // declarations don't carry role yet
            fleet_id,
            store_dir,
            &declaration.links,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            // Declarations are git-tracked contracts — the env-driven
            // auto-parent contract is deliberately not applied here.
            None,
            project_id,
            &[],
            None,
            None,
            energy_budget_cap,
        )
        .map_err(|e| anyhow::anyhow!("{}: {e}", decl_path.display()))?;

        results.push(result);
    }

    emit_output(ctx, &results);
    Ok(())
}

/// Enumerate declaration files from a path (file or directory).
fn collect_declaration_paths(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !path.exists() {
        return Err(anyhow::anyhow!("path does not exist: {}", path.display()));
    }

    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }

    let mut out = Vec::new();
    let entries = fs::read_dir(path)
        .map_err(|e| anyhow::anyhow!("failed to read directory {}: {e}", path.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|e| anyhow::anyhow!("failed to read entry in {}: {e}", path.display()))?;
        let entry_path = entry.path();
        if entry_path.is_file()
            && entry_path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            out.push(entry_path);
        }
    }
    // Deterministic order for reproducible batches.
    out.sort();
    Ok(out)
}

/// Load and parse a formula by name from `formulas_dir`.
fn load_formula(formulas_dir: &Path, name: &str) -> anyhow::Result<Formula> {
    let formula_path = formulas_dir.join(format!("{name}.formula.toml"));
    if !formula_path.exists() {
        return Err(anyhow::anyhow!(
            "formula not found: {} (looked in {})",
            name,
            formula_path.display()
        ));
    }

    let toml_text = fs::read_to_string(&formula_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", formula_path.display()))?;

    Formula::parse(&toml_text).map_err(|e| anyhow::anyhow!("failed to parse formula: {e}"))
}

/// Load and parse a formula from an explicit file path.
///
/// `cs nucleate` resolves recipes by *name* inside a formulas directory;
/// `cs spore run` instead names each node's recipe by a path relative to
/// the spore manifest. This loader serves that second caller without
/// re-deriving a name, so a spore can reference `formulas/foo.formula.toml`
/// verbatim.
pub(crate) fn load_formula_at_path(path: &Path) -> anyhow::Result<Formula> {
    let toml_text = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read formula {}: {e}", path.display()))?;
    Formula::parse(&toml_text)
        .map_err(|e| anyhow::anyhow!("failed to parse formula {}: {e}", path.display()))
}

/// One spore-driven nucleation (`cs spore run`).
///
/// `cs spore run` replays an [`expand`](fn@cosmon_core::spore::expand)ed call
/// list against the live state store. Each
/// [`NucleateCall`](cosmon_core::spore::NucleateCall) becomes one molecule
/// via [`nucleate_for_spore`], which reuses the exact persistence path the
/// interactive `cs nucleate` verb uses — symmetric `BlockedBy` links,
/// briefing/prompt/log artifacts, the nucleation event, and the prompt
/// seal. The spore shell owns the alias→[`MoleculeId`] mapping and passes
/// already-resolved `blocked_by` IDs in here, so the executor stays a thin
/// adapter over the canonical nucleation core (no second write path).
pub(crate) struct SporeNucleation<'a> {
    /// The node's resolved recipe (already loaded from its path).
    pub formula: &'a Formula,
    /// The substituted variable bindings for this node.
    pub variables: HashMap<String, String>,
    /// The molecule kind, if the spore node declares one.
    pub kind: Option<&'a str>,
    /// Predecessor molecule IDs, resolved from the call's `blocked_by`
    /// aliases by the spore shell.
    pub blocked_by: &'a [MoleculeId],
    /// The fleet to germinate into.
    pub fleet_id: &'a FleetId,
    /// The state store root.
    pub store_dir: &'a Path,
    /// The project id resolved from `.cosmon/config.toml`, if any.
    pub project_id: Option<&'a ProjectId>,
    /// Tags to attach to every germinated molecule.
    pub tags: &'a [Tag],
    /// Per-molecule step-budget circuit breaker cap.
    pub energy_budget_cap: u32,
}

/// Germinate a single spore node, returning its [`NucleateResult`] so the
/// shell can map the node alias to the freshly-minted [`MoleculeId`].
pub(crate) fn nucleate_for_spore(req: SporeNucleation<'_>) -> anyhow::Result<NucleateResult> {
    let (result, _dir) = nucleate_and_persist(
        req.formula,
        req.variables,
        None,
        req.kind,
        cosmon_core::molecule_class::MoleculeClass::default(),
        None,
        req.fleet_id,
        req.store_dir,
        &[],
        &[],
        req.blocked_by,
        &[],
        &[],
        &[],
        &[],
        None,
        req.project_id,
        req.tags,
        None,
        None,
        req.energy_budget_cap,
    )?;
    Ok(result)
}

/// Core nucleation + persistence path shared by both entrypoints.
///
/// Performs domain nucleation, saves `state.json`, writes `briefing.md`
/// and `log.md`, emits the `molecule_nucleated` event, maintains symmetric
/// blocking links on referenced targets, and returns the [`NucleateResult`]
/// together with the molecule directory path.
///
/// `blocks` / `blocked_by` parameters accept molecule IDs whose existence
/// is validated before persistence — a dangling reference aborts nucleation
/// with a descriptive error.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn nucleate_and_persist(
    formula: &Formula,
    variables: HashMap<String, String>,
    assign: Option<&str>,
    kind: Option<&str>,
    class: cosmon_core::molecule_class::MoleculeClass,
    role: Option<AgentRole>,
    fleet_id: &FleetId,
    store_dir: &Path,
    links: &[String],
    blocks: &[MoleculeId],
    blocked_by: &[MoleculeId],
    cross_blocks: &[CrossGalaxyRef],
    cross_blocked_by: &[CrossGalaxyRef],
    refines: &[MoleculeId],
    refutes: &[MoleculeId],
    decayed_from: Option<&MoleculeId>,
    project_id: Option<&ProjectId>,
    tags: &[Tag],
    expires_at: Option<DateTime<chrono::Utc>>,
    expiry_policy: Option<ExpiryPolicy>,
    energy_budget_cap: u32,
) -> anyhow::Result<(NucleateResult, PathBuf)> {
    let assign_id = assign
        .map(WorkerId::new)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid worker id: {e}"))?;

    let result = nucleate::nucleate(
        NucleateRequest {
            formula,
            variables,
            assign: assign_id,
        },
        &mut rand::thread_rng(),
    )
    .map_err(|e| anyhow::anyhow!("nucleation failed: {e}"))?;

    let mol_kind = kind
        .map(str::parse::<cosmon_core::kind::MoleculeKind>)
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid kind: {e}"))?;

    let store = FileStore::new(store_dir);

    // Build the typed_links for this molecule: blocks go one direction,
    // blocked_by go the other, plus the optional DecayedFrom edge and any
    // `Refines` citation edges. Cross-galaxy edges are recorded only on
    // the source side (one-writer-per-galaxy, ADR-052) — we cannot
    // mutate the target galaxy's state from here. The symmetric
    // counterpart on each *local* target is added after persistence
    // below.
    let mut typed_links: Vec<MoleculeLink> = Vec::with_capacity(
        blocks.len()
            + blocked_by.len()
            + cross_blocks.len()
            + cross_blocked_by.len()
            + refines.len()
            + refutes.len()
            + usize::from(decayed_from.is_some()),
    );
    for target in blocks {
        typed_links.push(MoleculeLink::Blocks {
            target: target.clone(),
        });
    }
    for source in blocked_by {
        typed_links.push(MoleculeLink::BlockedBy {
            source: source.clone(),
        });
    }
    for target in cross_blocks {
        typed_links.push(MoleculeLink::CrossGalaxyBlocks {
            target: target.clone(),
        });
    }
    for source in cross_blocked_by {
        typed_links.push(MoleculeLink::CrossGalaxyBlockedBy {
            source: source.clone(),
        });
    }
    for target in refines {
        typed_links.push(MoleculeLink::Refines {
            target: target.clone(),
        });
    }
    for target in refutes {
        typed_links.push(MoleculeLink::Refutes {
            target: target.clone(),
        });
    }
    if let Some(parent) = decayed_from {
        typed_links.push(MoleculeLink::DecayedFrom { id: parent.clone() });
    }

    let mol_data = MoleculeData {
        id: result.id.clone(),
        fleet_id: fleet_id.clone(),
        formula_id: result.formula_id.clone(),
        status: result.status,
        variables: result.variables.clone(),
        assigned_worker: result.assigned_worker.clone(),
        created_at: result.created_at,
        updated_at: result.created_at,
        total_steps: result.total_steps,
        current_step: 0,
        completed_steps: Vec::new(),
        collapse_reason: None,
        collapse_cause: None,
        collapse_reason_kind: None,
        collapsed_step: None,
        links: links.to_vec(),
        kind: mol_kind,
        class,
        typed_links,
        project_id: project_id.cloned(),
        assigned_role: role,
        session_name: None,
        tags: tags.iter().cloned().collect(),
        escalations: Vec::new(),
        freeze_on_last_step: formula.freeze_on_last_step,
        expires_at,
        expiry_policy,
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
        energy_budget: if energy_budget_cap == 0 {
            None
        } else {
            Some(cosmon_core::energy::StepBudget::new(energy_budget_cap))
        },
        stuck_at: None,
        tackled_by: None,
        tackled_at: None,
    };

    // Soft attention-budget warning (non-fatal).
    let fleet = store.load_fleet().unwrap_or_default();
    let molecules = store
        .list_molecules(&cosmon_state::MoleculeFilter::default())
        .unwrap_or_default();
    let statuses: Vec<_> = molecules.iter().map(|m| m.status).collect();
    let attention =
        cosmon_core::attention::check_attention_budget(fleet.attention_budget, &statuses);
    if let Some(warning) = attention.warning() {
        eprintln!("⚠️  {warning}");
    }

    // Hold the fleet lock for the entire validate → save → symmetric-link
    // cycle. Without this, two concurrent `--blocks T` nucleations race on
    // T's typed_links and last-writer-wins (knuth's Race B).
    let mol_dir = {
        // ADR-131 Decision 2: RAII guard replaces the lock-bounding closure.
        // `_g` releases the flock at end of block; `s` keeps the body's
        // port-only helper calls byte-identical.
        let _g = store.lock_fleet()?;
        let s = &store;
        // Validate referenced targets exist BEFORE we persist the new molecule.
        // Dangling edges would leave a half-formed DAG that no policy can schedule.
        validate_targets_exist(s, blocks, "--blocks")?;
        validate_targets_exist(s, blocked_by, "--blocked-by")?;
        validate_targets_exist(s, refines, "--refines")?;
        validate_targets_exist(s, refutes, "--refutes")?;
        if let Some(parent) = decayed_from {
            validate_targets_exist(s, std::slice::from_ref(parent), "--decayed-from")?;
        }
        // Cross-galaxy references are checked best-effort: we emit a
        // warning if the target galaxy/molecule is unreachable, but
        // we do *not* abort. A galaxy may legitimately be offline
        // (rclone not synced, worktree pruned, …) and the operator
        // wants the edge recorded so the runtime can resolve it
        // later.
        warn_cross_galaxy_reachability(cross_blocks, "--blocks");
        warn_cross_galaxy_reachability(cross_blocked_by, "--blocked-by");

        s.save_molecule(&result.id, &mol_data)
            .map_err(|e| anyhow::anyhow!("failed to save molecule: {e}"))?;

        // Symmetry maintenance: add the reverse link to every referenced target.
        // If `new` has Blocks{target=T}, then T gets BlockedBy{source=new.id}.
        // Each add is idempotent (skip if already present), so re-running the
        // same nucleate (or manual retries) does not duplicate edges.
        for target in blocks {
            add_symmetric_link(
                s,
                target,
                MoleculeLink::BlockedBy {
                    source: result.id.clone(),
                },
            )?;
        }
        for source in blocked_by {
            add_symmetric_link(
                s,
                source,
                MoleculeLink::Blocks {
                    target: result.id.clone(),
                },
            )?;
        }
        // Symmetric RefinedBy on each cited target — lets `cs deps` on a
        // cited molecule surface the constellation(s) that reference it.
        for target in refines {
            add_symmetric_link(
                s,
                target,
                MoleculeLink::RefinedBy {
                    source: result.id.clone(),
                },
            )?;
        }
        // Symmetric RefutedBy on each refuted diagnosis — lets `cs deps`
        // on the diagnosis molecule surface the verify molecule(s) that
        // contradicted it (ADR-143 calibration loop).
        for target in refutes {
            add_symmetric_link(
                s,
                target,
                MoleculeLink::RefutedBy {
                    source: result.id.clone(),
                },
            )?;
        }
        // DecayedFrom is an information edge, not a progression edge —
        // the parent keeps running, and the runtime's lateral-drain pass
        // (see fix dc66e2f) picks up the `DecayProduct` counterpart so
        // orphan children are rescued even when the parent is busy.
        if let Some(parent) = decayed_from {
            add_symmetric_link(
                s,
                parent,
                MoleculeLink::DecayProduct {
                    id: result.id.clone(),
                },
            )?;
        }

        let dir = store_dir
            .join("fleets")
            .join(fleet_id.as_str())
            .join("molecules")
            .join(result.id.as_str());

        write_briefing(&dir, formula, &result)?;
        write_prompt(&dir, formula, &result)?;
        write_log(&dir, &result)?;
        emit_event(store_dir, &result, blocks)?;

        // Soft-contract seal: hash prompt.md and stamp the molecule's
        // `prompt_seal`. Defensive — any failure (I/O, hash, state
        // reload) is logged and swallowed; seal emission must never
        // block nucleation. Mirrors `try_seal_briefing` in `cs evolve`.
        if let Some(seal) = try_seal_prompt(&dir) {
            let _ = stamp_prompt_seal(s, &result.id, &seal);
            let _ = cosmon_state::event_log::emit_one(
                store_dir.join("events.jsonl"),
                cosmon_core::event_v2::EventV2::PromptSealed {
                    molecule_id: result.id.clone(),
                    hash: seal.hash.clone(),
                    sealed_at: seal.sealed_at,
                    bytes: seal.briefing_bytes,
                    canonical_version: seal.canonical_version,
                },
                None,
            );
        }

        dir
    };

    Ok((result, mol_dir))
}

/// Compute a [`BriefingSeal`] over `prompt.md` in `mol_dir`. Returns
/// `None` if the file cannot be read — the caller treats this as "no
/// seal", never as an error. Seal emission is a probe, not a lock.
fn try_seal_prompt(mol_dir: &Path) -> Option<BriefingSeal> {
    match fs::read(mol_dir.join("prompt.md")) {
        Ok(bytes) => Some(BriefingSeal::of_text_or_bytes(0, &bytes)),
        Err(e) => {
            eprintln!("warning: could not seal prompt.md: {e}");
            None
        }
    }
}

/// Persist `prompt_seal` on the newly nucleated molecule. Idempotent —
/// re-running with the same seal is a no-op.
fn stamp_prompt_seal(
    store: &FileStore,
    mol_id: &MoleculeId,
    seal: &BriefingSeal,
) -> anyhow::Result<()> {
    let mut mol = store
        .load_molecule(mol_id)
        .map_err(|e| anyhow::anyhow!("failed to reload for seal stamp: {e}"))?;
    if mol.prompt_seal.as_ref() == Some(seal) {
        return Ok(());
    }
    mol.prompt_seal = Some(seal.clone());
    mol.updated_at = Utc::now();
    store
        .save_molecule(mol_id, &mol)
        .map_err(|e| anyhow::anyhow!("failed to persist prompt seal: {e}"))?;
    Ok(())
}

/// Best-effort reachability probe for cross-galaxy references. Prints
/// a `warning:` line to stderr for each ref whose galaxy or target
/// molecule cannot be located, then proceeds — the edge is still
/// recorded locally because the remote galaxy may come online later
/// (offline / unsynced / archived). This mirrors ADR-035 §6, where a
/// network partition surfaces as a `StaleEdge` rather than aborting
/// the local DAG.
fn warn_cross_galaxy_reachability(refs: &[CrossGalaxyRef], flag: &str) {
    use super::cross_galaxy::{resolve_cross_galaxy_ref, CrossGalaxyResolution};
    for cgr in refs {
        match resolve_cross_galaxy_ref(cgr) {
            CrossGalaxyResolution::Resolved { .. } => {}
            CrossGalaxyResolution::MoleculeMissing { galaxy_path } => {
                eprintln!(
                    "warning: {flag} `{cgr}` — galaxy `{}` is reachable at {} but the molecule \
                     was not found (recorded anyway; resolve later if it lands)",
                    cgr.galaxy,
                    galaxy_path.display()
                );
            }
            CrossGalaxyResolution::GalaxyUnknown => {
                eprintln!(
                    "warning: {flag} `{cgr}` — galaxy `{}` not in registry, override file, \
                     or `~/galaxies/<alias>/.cosmon/` (recorded anyway)",
                    cgr.galaxy
                );
            }
        }
    }
}

/// Verify every referenced molecule exists before nucleation commits.
///
/// Without this check, `--blocks foo --blocks bar` where `bar` doesn't exist
/// would leave a half-formed DAG: the new molecule would point to bar via
/// `Blocks`, but bar would never gain the symmetric `BlockedBy` (it can't,
/// it doesn't exist). Dangling references break the runtime's scheduler.
fn validate_targets_exist(store: &FileStore, ids: &[MoleculeId], flag: &str) -> anyhow::Result<()> {
    for id in ids {
        store.load_molecule(id).map_err(|_| {
            anyhow::anyhow!("{flag} references unknown molecule `{id}` — create it first")
        })?;
    }
    Ok(())
}

/// Add a typed link to `target`'s `typed_links`, skipping the insert if an
/// equivalent link already exists. Equivalence is by variant + key field
/// (target/source molecule ID). Preserves idempotency so re-running a
/// failed nucleate (or concurrent creators) does not duplicate edges.
fn add_symmetric_link(
    store: &FileStore,
    target_id: &MoleculeId,
    new_link: MoleculeLink,
) -> anyhow::Result<()> {
    let mut target = store
        .load_molecule(target_id)
        .map_err(|e| anyhow::anyhow!("failed to load {target_id} for symmetry update: {e}"))?;

    if link_already_present(&target.typed_links, &new_link) {
        return Ok(());
    }

    target.typed_links.push(new_link);
    target.updated_at = Utc::now();
    store
        .save_molecule(target_id, &target)
        .map_err(|e| anyhow::anyhow!("failed to persist symmetry update on {target_id}: {e}"))?;
    Ok(())
}

/// Check whether a typed link with matching variant + key is already in
/// the list. Used for idempotent symmetry maintenance. We match on the
/// shape of each variant pair and reduce to the molecule-id key each
/// one carries, because every link variant is keyed by a single
/// molecule-id field regardless of whether it's called `target`,
/// `source`, or `id`.
fn link_already_present(existing: &[MoleculeLink], candidate: &MoleculeLink) -> bool {
    existing.iter().any(|l| match (l, candidate) {
        (MoleculeLink::Blocks { target: a }, MoleculeLink::Blocks { target: b })
        | (MoleculeLink::BlockedBy { source: a }, MoleculeLink::BlockedBy { source: b })
        | (MoleculeLink::DecayProduct { id: a }, MoleculeLink::DecayProduct { id: b })
        | (MoleculeLink::DecayedFrom { id: a }, MoleculeLink::DecayedFrom { id: b })
        | (MoleculeLink::Refines { target: a }, MoleculeLink::Refines { target: b })
        | (MoleculeLink::RefinedBy { source: a }, MoleculeLink::RefinedBy { source: b }) => a == b,
        _ => false,
    })
}

/// Emit final command output for one or more nucleated molecules.
fn emit_output(ctx: &Context, results: &[NucleateResult]) {
    if ctx.json {
        if results.len() == 1 {
            println!("{}", serialize_one(&results[0]));
        } else {
            let arr: Vec<_> = results.iter().map(serialize_one).collect();
            let value = serde_json::Value::Array(arr);
            println!("{value}");
        }
    } else {
        for result in results {
            println!(
                "Nucleated molecule {} from formula {}",
                result.id, result.formula_id
            );
            if let Some(ref worker) = result.assigned_worker {
                println!("  Assigned to: {worker}");
            }
            println!("  Steps: {}", result.total_steps);
            if ctx.verbose {
                for (k, v) in &result.variables {
                    println!("  {k} = {v}");
                }
            }
        }
        if results.len() > 1 {
            println!("Nucleated {} molecules.", results.len());
        }
    }
}

/// Serialize a single `NucleateResult` to the JSON shape used by `--json`.
fn serialize_one(result: &NucleateResult) -> serde_json::Value {
    serde_json::json!({
        "id": result.id.as_str(),
        "formula": result.formula_id.as_str(),
        "status": "active",
        "total_steps": result.total_steps,
        "assigned_worker": result.assigned_worker.as_ref().map(WorkerId::as_str),
        "variables": result.variables,
        "created_at": result.created_at.to_rfc3339(),
    })
}

/// Parse `--var key=value` flags into a `HashMap`.
fn parse_vars(vars: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for kv in vars {
        let (key, value) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid variable format (expected key=value): {kv}"))?;
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

/// Write `briefing.md` into the molecule directory with step descriptions.
fn write_briefing(
    mol_dir: &std::path::Path,
    formula: &Formula,
    result: &nucleate::NucleateResult,
) -> anyhow::Result<()> {
    let mut md = String::new();
    let _ = write!(md, "# Molecule: {}\n\n", result.id);
    let _ = write!(md, "**Formula:** {}\n\n", result.formula_id);
    if let Some(ref worker) = result.assigned_worker {
        let _ = write!(md, "**Assigned to:** {worker}\n\n");
    }
    md.push_str("## Steps\n\n");
    for (i, step) in formula.steps.iter().enumerate() {
        let _ = write!(md, "### Step {} — {}\n\n", i + 1, step.title);
        if !step.description.is_empty() {
            md.push_str(&step.description);
            md.push_str("\n\n");
        }
        if let Some(ref criteria) = step.exit_criteria {
            let _ = write!(md, "**Exit criteria:** {criteria}\n\n");
        }
    }

    fs::write(mol_dir.join("briefing.md"), md.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to write briefing.md: {e}"))
}

/// Write `prompt.md` — the operator-intent artifact (proof-of-work for the
/// nucleation step). Renders the raw variables as human-readable markdown
/// alongside a metadata frontmatter block, so a verifier can audit the
/// transformation chain `prompt → briefing → synthesis` without parsing
/// JSON-escaped strings from `state.json`.
fn write_prompt(
    mol_dir: &std::path::Path,
    formula: &Formula,
    result: &nucleate::NucleateResult,
) -> anyhow::Result<()> {
    let mut md = String::new();
    md.push_str("---\n");
    let _ = writeln!(
        md,
        "nucleated_at: {}",
        result.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    let _ = writeln!(md, "molecule_id: {}", result.id);
    let _ = writeln!(md, "formula: {}", formula.name);
    let _ = writeln!(md, "formula_id: {}", result.formula_id);
    if !result.variables.is_empty() {
        md.push_str("variables:\n");
        let mut keys: Vec<&String> = result.variables.keys().collect();
        keys.sort();
        for k in keys {
            let v = &result.variables[k];
            if v.contains('\n') {
                let _ = writeln!(md, "  {k}: |");
                for line in v.lines() {
                    let _ = writeln!(md, "    {line}");
                }
            } else {
                let escaped = v.replace('\\', "\\\\").replace('"', "\\\"");
                let _ = writeln!(md, "  {k}: \"{escaped}\"");
            }
        }
    }
    md.push_str("---\n\n# Operator prompt\n\n");
    if result.variables.is_empty() {
        md.push_str("_(no variables bound at nucleation)_\n");
    } else {
        let mut keys: Vec<&String> = result.variables.keys().collect();
        keys.sort();
        for k in keys {
            let _ = writeln!(md, "## {k}\n\n{}\n", result.variables[k]);
        }
    }

    fs::write(mol_dir.join("prompt.md"), md.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to write prompt.md: {e}"))
}

/// Write `log.md` with the creation entry.
fn write_log(mol_dir: &std::path::Path, result: &nucleate::NucleateResult) -> anyhow::Result<()> {
    let entry = format!(
        "# Log: {}\n\n- **{}** — Molecule nucleated from formula `{}`\n",
        result.id,
        result.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
        result.formula_id,
    );
    fs::write(mol_dir.join("log.md"), entry.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to write log.md: {e}"))
}

/// Append a creation event to `events.jsonl`.
fn emit_event(
    store_dir: &std::path::Path,
    result: &nucleate::NucleateResult,
    blocks: &[MoleculeId],
) -> anyhow::Result<()> {
    use cosmon_core::event_v2::EventV2;
    use cosmon_state::event_log;

    let events_path = store_dir.join("events.jsonl");

    // Emit canonical EventV2 record. `parent_id` is None at the CLI layer
    // (it gets populated by higher-level flows like decay splicing that
    // emit their own events); `blocks` encodes the outgoing DAG edges
    // captured at nucleation time so replay has a sufficient statistic.
    event_log::emit_one(
        &events_path,
        EventV2::MoleculeNucleated {
            molecule_id: result.id.clone(),
            formula_id: result.formula_id.as_str().to_owned(),
            parent_id: None,
            blocks: blocks.to_vec(),
        },
        None,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_filestore::StateDirOrigin;

    /// A deliberate destination (a galaxy found via walk-up, an explicit
    /// `--store-dir`, or `COSMON_STATE_DIR`) is never flagged — the guard
    /// stays silent and returns `Ok`.
    #[test]
    fn guard_silent_for_non_global_origins() {
        let dir = Path::new("/tmp/whatever/.cosmon/state");
        for origin in [
            StateDirOrigin::Project,
            StateDirOrigin::Explicit,
            StateDirOrigin::Env,
        ] {
            assert!(guard_global_fallback(origin, dir, false).is_ok());
            // Even with --require-galaxy, a scoped destination passes.
            assert!(guard_global_fallback(origin, dir, true).is_ok());
        }
    }

    /// The home-global fallback warns but does not block by default —
    /// behaviour-preserving (the molecule is still created).
    #[test]
    fn guard_warns_but_allows_global_fallback_by_default() {
        let dir = Path::new("/home/u/.cosmon/state");
        assert!(guard_global_fallback(StateDirOrigin::GlobalFallback, dir, false).is_ok());
    }

    /// `--require-galaxy` turns the silent fallback into a hard error so
    /// scripts can fail fast instead of leaking orphans.
    #[test]
    fn guard_blocks_global_fallback_when_require_galaxy() {
        let dir = Path::new("/home/u/.cosmon/state");
        let err = guard_global_fallback(StateDirOrigin::GlobalFallback, dir, true)
            .expect_err("--require-galaxy must reject the host-global fallback");
        let msg = err.to_string();
        assert!(
            msg.contains("no galaxy"),
            "message should explain why: {msg}"
        );
        assert!(
            msg.contains("cs init"),
            "message should suggest a fix: {msg}"
        );
    }

    /// Minimal `Args` builder for the resolver tests — every field is a
    /// CLI flag, but the resolver only inspects a small subset.
    fn empty_args() -> Args {
        Args {
            formula: Some("task-work".to_owned()),
            from: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            decayed_from: None,
            no_parent: false,
            refines: Vec::new(),
            refutes: Vec::new(),
            fleet: "default".to_owned(),
            kind: None,
            class: None,
            assign: None,
            vars: Vec::new(),
            formulas_dir: None,
            role: None,
            store_dir: None,
            tags: Vec::new(),
            interaction_mode: None,
            may_block_on_operator: None,
            ttl: None,
            expires_at: None,
            expiry_policy: None,
            energy_budget: None,
            require_galaxy: false,
        }
    }

    fn mol_id(s: &str) -> MoleculeId {
        MoleculeId::new(s).unwrap()
    }

    #[test]
    fn resolve_decayed_from_no_env_returns_none() {
        let args = empty_args();
        let no_env = || None;
        let got = resolve_decayed_from(&args, &[], &[], &no_env).unwrap();
        assert!(got.is_none(), "absent env var must yield None");
    }

    #[test]
    fn resolve_decayed_from_env_var_is_picked_up() {
        let args = empty_args();
        let env = || Some("task-20260414-abcd".to_owned());
        let got = resolve_decayed_from(&args, &[], &[], &env).unwrap();
        assert_eq!(
            got.as_ref().map(MoleculeId::as_str),
            Some("task-20260414-abcd")
        );
    }

    #[test]
    fn resolve_decayed_from_no_parent_flag_suppresses_env() {
        let mut args = empty_args();
        args.no_parent = true;
        let env = || Some("task-20260414-abcd".to_owned());
        let got = resolve_decayed_from(&args, &[], &[], &env).unwrap();
        assert!(
            got.is_none(),
            "--no-parent must hard-disable the auto-parent contract"
        );
    }

    #[test]
    fn resolve_decayed_from_explicit_flag_wins_over_env() {
        let mut args = empty_args();
        args.decayed_from = Some("task-20260414-f00d".to_owned());
        let env = || Some("task-20260414-abcd".to_owned());
        let got = resolve_decayed_from(&args, &[], &[], &env).unwrap();
        assert_eq!(
            got.as_ref().map(MoleculeId::as_str),
            Some("task-20260414-f00d"),
            "explicit --decayed-from must override env var"
        );
    }

    #[test]
    fn resolve_decayed_from_explicit_blocks_silences_env() {
        let args = empty_args();
        let blocks = vec![mol_id("task-20260414-aaaa")];
        let env = || Some("task-20260414-abcd".to_owned());
        let got = resolve_decayed_from(&args, &blocks, &[], &env).unwrap();
        assert!(
            got.is_none(),
            "explicit --blocks must silence the env-driven auto-parent"
        );
    }

    #[test]
    fn resolve_decayed_from_explicit_blocked_by_silences_env() {
        let args = empty_args();
        let blocked_by = vec![mol_id("task-20260414-bbbb")];
        let env = || Some("task-20260414-abcd".to_owned());
        let got = resolve_decayed_from(&args, &[], &blocked_by, &env).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn resolve_decayed_from_rejects_malformed_env_value() {
        let args = empty_args();
        let env = || Some("NOT A MOLECULE ID".to_owned());
        let err = resolve_decayed_from(&args, &[], &[], &env).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("COSMON_PARENT_MOL_ID"),
            "error should name the env var, got: {msg}"
        );
    }

    #[test]
    fn resolve_decayed_from_empty_env_value_treated_as_unset() {
        let args = empty_args();
        let env = || Some(String::new());
        let got = resolve_decayed_from(&args, &[], &[], &env).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn link_already_present_detects_duplicate_decay_product() {
        let existing = vec![MoleculeLink::DecayProduct {
            id: mol_id("task-20260414-abcd"),
        }];
        let candidate = MoleculeLink::DecayProduct {
            id: mol_id("task-20260414-abcd"),
        };
        assert!(link_already_present(&existing, &candidate));

        let different = MoleculeLink::DecayProduct {
            id: mol_id("task-20260414-f00d"),
        };
        assert!(!link_already_present(&existing, &different));
    }

    #[test]
    fn link_already_present_detects_duplicate_refines() {
        let existing = vec![MoleculeLink::Refines {
            target: mol_id("task-20260422-aaaa"),
        }];
        let candidate = MoleculeLink::Refines {
            target: mol_id("task-20260422-aaaa"),
        };
        assert!(link_already_present(&existing, &candidate));

        let refined_by_is_different_variant = MoleculeLink::RefinedBy {
            source: mol_id("task-20260422-aaaa"),
        };
        assert!(!link_already_present(
            &existing,
            &refined_by_is_different_variant
        ));
    }

    #[test]
    fn resolve_refines_explicit_flags_only() {
        let got = resolve_refines(
            &[
                "task-20260422-0001".to_owned(),
                "task-20260422-0002".to_owned(),
            ],
            None,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            got.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
            vec!["task-20260422-0001", "task-20260422-0002"]
        );
    }

    #[test]
    fn resolve_refines_ignores_citations_without_constellation_kind() {
        let mut vars = HashMap::new();
        vars.insert(
            "citations".to_owned(),
            "task-20260422-0001,task-20260422-0002".to_owned(),
        );
        let got = resolve_refines(&[], Some("task"), &vars).unwrap();
        assert!(
            got.is_empty(),
            "only --kind constellation should expand citations into refines"
        );
    }

    #[test]
    fn resolve_refines_expands_citations_for_constellation() {
        let mut vars = HashMap::new();
        vars.insert(
            "citations".to_owned(),
            " task-20260422-0001, task-20260422-0002 ,task-20260422-0003 ".to_owned(),
        );
        let got = resolve_refines(&[], Some("constellation"), &vars).unwrap();
        assert_eq!(
            got.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
            vec![
                "task-20260422-0001",
                "task-20260422-0002",
                "task-20260422-0003"
            ]
        );
    }

    #[test]
    fn resolve_refines_deduplicates_explicit_and_citations() {
        let mut vars = HashMap::new();
        vars.insert(
            "citations".to_owned(),
            "task-20260422-0001,task-20260422-0002".to_owned(),
        );
        let got = resolve_refines(
            &["task-20260422-0001".to_owned()],
            Some("constellation"),
            &vars,
        )
        .unwrap();
        assert_eq!(
            got.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
            vec!["task-20260422-0001", "task-20260422-0002"]
        );
    }

    #[test]
    fn resolve_refines_rejects_malformed_citation() {
        let mut vars = HashMap::new();
        vars.insert(
            "citations".to_owned(),
            "task-20260422-0001,NOT A MOLECULE,task-20260422-0003".to_owned(),
        );
        let err = resolve_refines(&[], Some("constellation"), &vars).unwrap_err();
        assert!(
            format!("{err}").contains("--var citations"),
            "error should name the bad entry, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Cross-galaxy parser — Phase 1 of ADR-035
    // -----------------------------------------------------------------------

    #[test]
    fn parse_link_refs_handles_pure_local_list() {
        let raw = vec![
            "task-20260425-aaaa".to_owned(),
            "task-20260425-bbbb".to_owned(),
        ];
        let (local, remote) = parse_link_refs(&raw, "--blocked-by").unwrap();
        assert_eq!(local.len(), 2);
        assert!(remote.is_empty());
        assert_eq!(local[0].as_str(), "task-20260425-aaaa");
    }

    #[test]
    fn parse_link_refs_handles_pure_cross_galaxy_list() {
        let raw = vec![
            "mailroom:delib-20260425-39c1".to_owned(),
            "tenant-demo@delib-20260425-54aa".to_owned(),
        ];
        let (local, remote) = parse_link_refs(&raw, "--blocked-by").unwrap();
        assert!(local.is_empty());
        assert_eq!(remote.len(), 2);
        assert_eq!(remote[0].galaxy, "mailroom");
        assert_eq!(remote[0].mol_id.as_str(), "delib-20260425-39c1");
        assert_eq!(remote[1].galaxy, "tenant-demo");
        assert_eq!(remote[1].mol_id.as_str(), "delib-20260425-54aa");
    }

    #[test]
    fn parse_link_refs_mixes_local_and_cross_galaxy() {
        let raw = vec![
            "task-20260425-aaaa".to_owned(),
            "mailroom:delib-20260425-39c1".to_owned(),
        ];
        let (local, remote) = parse_link_refs(&raw, "--blocks").unwrap();
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].as_str(), "task-20260425-aaaa");
        assert_eq!(remote.len(), 1);
        assert_eq!(
            remote[0].to_canonical_string(),
            "mailroom:delib-20260425-39c1"
        );
    }

    #[test]
    fn parse_link_refs_rejects_invalid_local() {
        let raw = vec!["not-a-real-id".to_owned()];
        let err = parse_link_refs(&raw, "--blocked-by").unwrap_err();
        assert!(
            format!("{err}").contains("--blocked-by"),
            "error should name the flag, got: {err}"
        );
    }

    #[test]
    fn parse_link_refs_rejects_cross_galaxy_with_bad_mol_id() {
        let raw = vec!["mailroom:not-a-real-id".to_owned()];
        let err = parse_link_refs(&raw, "--blocked-by").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("cross-galaxy") && msg.contains("--blocked-by"),
            "error should label cross-galaxy + flag, got: {msg}"
        );
    }

    // ---- merge_interaction_mode (delib-20260503-9aab P4) -----------------

    #[test]
    fn merge_interaction_mode_absent_flag_is_noop() {
        let tags = vec![Tag::new("priority:high").unwrap()];
        let got = merge_interaction_mode(tags.clone(), None).unwrap();
        assert_eq!(got, tags);
    }

    #[test]
    fn merge_interaction_mode_appends_canonical_tag() {
        let got = merge_interaction_mode(Vec::new(), Some("operator-required")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_str(), "interaction-mode:operator-required");
    }

    #[test]
    fn merge_interaction_mode_accepts_background() {
        let got = merge_interaction_mode(Vec::new(), Some("background")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_str(), "interaction-mode:background");
    }

    #[test]
    fn merge_interaction_mode_rejects_unknown_value() {
        let err = merge_interaction_mode(Vec::new(), Some("operator-maybe")).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--interaction-mode"),
            "error should name the flag, got: {msg}"
        );
    }

    #[test]
    fn merge_interaction_mode_refuses_double_source() {
        let tags = vec![Tag::new("interaction-mode:background").unwrap()];
        let err = merge_interaction_mode(tags, Some("operator-required")).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("single-sourced"),
            "error should explain the single-source rule, got: {msg}"
        );
    }

    #[test]
    fn fleet_review_opt_in_projects_protected_cross_provider_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let cosmon = tmp.path().join(".cosmon");
        let state = cosmon.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            cosmon.join("fleet.toml"),
            r#"
fleet = "reviewed"

[review]
cross_provider = true
reviewer_adapter = "openai"

[[agents]]
name = "worker"
role = "implementation"
clearance = "write"
"#,
        )
        .unwrap();

        let tags = merge_fleet_review(Vec::new(), &state).unwrap();
        let names: Vec<_> = tags.iter().map(|tag| tag.as_str()).collect();
        assert_eq!(
            names,
            [
                "needs-review",
                "needs-review-cross-provider",
                "reviewer-adapter:openai"
            ]
        );
    }
}
