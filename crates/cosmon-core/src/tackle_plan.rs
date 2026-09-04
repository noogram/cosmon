// SPDX-License-Identifier: AGPL-3.0-only

//! The **pure decision half of `cs tackle`** — a [`TacklePlan`] value.
//!
//! Issue #54 / U4. `cs tackle`'s dispatch used to be one 1700-line effectful
//! function in `cosmon-cli`, which made "tackle a molecule" reachable only by
//! shelling the `cs` binary (the ADR-080 §3.5 clause (e) subprocess envelope).
//! This module extracts the *decision* half — everything phases 1–6 of
//! `cmd::tackle::run` compute before the first side effect — into pure
//! functions over typed, in-memory inputs, so a library consumer (the
//! rpp-adapter after U5/U6, a test, a future scheduler) can compute the same
//! plan the CLI computes without a filesystem, a process, or an environment.
//!
//! # What is IN the plan (pure, decided here)
//!
//! - the **adapter selection** — the six-level Q5a chain (flag → formula-step
//!   pin → `$COSMON_DEFAULT_ADAPTER` → per-galaxy config → global config →
//!   built-in floor), validated against the dispatch registry
//!   ([`resolve_selection`]);
//! - the **model selection** — the sibling six-level chain scoped to the
//!   resolved adapter (delib-20260704-b476 C1);
//! - the **loop-ownership axis** (ADR-103), with any config-shape warning
//!   carried as *data* rather than printed;
//! - the **worker prompt** — the full bootstrap brief ([`build_prompt`]),
//!   including the `--dry-run` rendering (the dry-run output IS the prompt);
//! - the **predicted worktree root** ([`predicted_sandbox_root`]) and the
//!   **branch name** (`feat/<mol-id>`) the effect half will create.
//!
//! # What is deliberately NOT here (the effect half, with reasons)
//!
//! - **Base-branch validation and the reviewed-tree pin** shell `git`
//!   (`refs/heads` existence, `rev-parse`); the caller resolves them first and
//!   passes the results in (`base_branch`, `reviewed_start_point`).
//! - **The strong-model budget ceiling** (delib-20260704-b476 C4) folds the
//!   fleet `events.jsonl` — an effect-half input. [`TacklePlan::resolve`]
//!   applies *no* ceiling; a caller with a budget history must run the gate
//!   from [`crate::model_budget`] between [`resolve_selection`] and
//!   [`TacklePlan::from_parts`], exactly as `cmd::tackle::run` does.
//! - **Event emission** (`AdapterSelected`, `ModelSelected`), the adapter
//!   preflight probes (network), briefing injection (filesystem), and the
//!   worktree/tmux spawn itself. The plan carries the selection records those
//!   events are minted from; emitting them is the caller's job.
//! - **The worker command argv + env allow-list** — the effect half of the
//!   spawn (U5); they will join the plan when the executor seam lands.
//!
//! # Purity contract
//!
//! Nothing in this module reads the filesystem, the environment, the clock,
//! or a process. Every input is a value; the falsifier is the unit test
//! `tackle_plan_resolves_purely_from_in_memory_state`, which builds a full
//! [`TacklePlan`] from literals.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{
    AdapterEntry, AdaptersConfig, GatesConfig, OnComplete, ProjectConfig, BUILTIN_FLOOR_ADAPTER,
};
use crate::event_v2::{AdapterSelectionSource, ModelSelectionSource};
use crate::formula::Formula;
use crate::id::{FormulaId, MoleculeId};
use crate::kind::MoleculeKind;
use crate::spawn_seam::{
    built_in_adapter_names, validate_adapter_name, LoopOwnership, SupervisionMode, UnknownAdapter,
    ValidatedAdapterName,
};

/// Example relative artifact path shown to a local worker, so the brief
/// demonstrates the shape it must use instead of an absolute path.
const DEFAULT_LOCAL_ARTIFACT_EXAMPLE: &str = "result.md";

/// The molecule fields the tackle decision actually reads — a borrowed
/// projection of the state store's molecule record.
///
/// Exists because `cosmon-core` sits *below* the state crate in the
/// dependency graph: the plan cannot take `cosmon_state::MoleculeData`, and
/// taking the whole record would also overstate what the decision depends
/// on. The caller (CLI today, adapter after U6) reads the molecule through
/// its state port and projects these six fields.
#[derive(Debug, Clone, Copy)]
pub struct MoleculeBrief<'a> {
    /// The molecule being tackled.
    pub id: &'a MoleculeId,
    /// Cognitive kind (idea / task / …); `None` for legacy molecules.
    pub kind: Option<MoleculeKind>,
    /// The formula the molecule executes, by id.
    pub formula_id: &'a FormulaId,
    /// Zero-based index of the currently executing step.
    pub current_step: usize,
    /// Total number of steps the formula declares.
    pub total_steps: usize,
    /// The molecule's variables (topic, mission text, …).
    pub variables: &'a HashMap<String, String>,
}

/// Inputs to [`resolve_selection`] — everything the adapter + model chains
/// read, already loaded by the caller.
///
/// Environment values (`$COSMON_DEFAULT_ADAPTER`, the model env tier) and
/// config files are *read by the caller* and passed as values, which is what
/// keeps the resolution itself pure and unit-testable without `std::env`.
#[derive(Debug, Clone, Copy)]
pub struct SelectionRequest<'a> {
    /// `--adapter <name>` if the operator passed it.
    pub adapter_flag: Option<&'a str>,
    /// `--model <id>` if the operator passed it.
    pub model_flag: Option<&'a str>,
    /// The resolved formula, when its id resolved.
    pub formula: Option<&'a Formula>,
    /// Zero-based index of the currently executing step (the step whose
    /// `adapter = "…"` / `model = "…"` pins are consulted).
    pub current_step: usize,
    /// `$COSMON_DEFAULT_ADAPTER`, caller-read; empty string = unset.
    pub env_default_adapter: Option<&'a str>,
    /// `(value, var_name)` of the first set-and-non-empty model env var
    /// (`$COSMON_DEFAULT_MODEL`, else the legacy `$ANTHROPIC_MODEL`).
    pub env_default_model: Option<(&'a str, &'a str)>,
    /// The per-galaxy `[adapters]` table, if configured.
    pub project_adapters: Option<&'a AdaptersConfig>,
    /// Path the per-galaxy config was read from — recorded verbatim on the
    /// `Config` selection-source variants for the audit trail.
    pub config_path: &'a Path,
    /// The global `[adapters]` table, if present and well-formed.
    pub global_adapters: Option<&'a AdaptersConfig>,
    /// Path the global config was read from — recorded on `GlobalConfig`
    /// selection-source variants.
    pub global_config_path: &'a Path,
    /// The formula-absence reason (id did not resolve), when there is one —
    /// folded into `Default` fallback reasons so they name the real cause
    /// (task-20260725-eb3b).
    pub formula_absence: Option<&'a str>,
}

/// The resolved **who-runs-this** half of a tackle decision: adapter, model,
/// and the axes the spawn seam derives from them.
///
/// Produced by [`resolve_selection`]; consumed by the caller's gates and
/// events, then folded into a [`TacklePlan`] via [`TacklePlan::from_parts`].
#[derive(Debug, Clone)]
pub struct TackleSelection {
    /// The validated adapter name — the only type the spawn seam accepts.
    pub adapter: ValidatedAdapterName,
    /// Where the adapter name came from (flag / pin / env / config / floor).
    pub adapter_source: AdapterSelectionSource,
    /// The supervision axis the validator derived for this adapter.
    pub supervision: SupervisionMode,
    /// The loop-ownership axis (ADR-103), after any TOML override.
    pub loop_ownership: LoopOwnership,
    /// Warning text for an unrecognised `[adapters.<name>].ownership` value,
    /// carried as data so this module stays print-free; the CLI surfaces it
    /// on stderr at the same point the inline code used to.
    pub ownership_warning: Option<String>,
    /// The per-molecule model pin; `None` = the adapter's own default.
    pub preferred_model: Option<String>,
    /// Where the model pin came from.
    pub model_source: ModelSelectionSource,
}

/// Resolve the adapter, model, and spawn axes for a tackle — the pure
/// composition of the two six-level Q5a chains plus registry validation.
///
/// Order of decisions mirrors the historical inline sequence in
/// `cmd::tackle::run` phase 3a exactly: adapter chain → fallback sharpening →
/// registry validation → loop ownership → model chain → sharpening. The
/// capability gate and every event emission stay with the caller, between
/// this call and [`TacklePlan::from_parts`].
///
/// # Errors
///
/// Returns [`UnknownAdapter`] when the resolved name is in neither the
/// built-in registry nor the caller's `[adapters]` table — the same typed
/// refusal the CLI has always printed, before any side effect lands.
pub fn resolve_selection(req: &SelectionRequest<'_>) -> Result<TackleSelection, UnknownAdapter> {
    // Per-step pins: `(value, formula_name, step_id)` for the currently
    // executing step, or `None`. Read from the same step for both axes.
    let formula_step_adapter: Option<(&str, &str, &str)> = req.formula.and_then(|f| {
        f.steps.get(req.current_step).and_then(|step| {
            step.adapter
                .as_deref()
                .map(|name| (name, f.name.as_str(), step.id.as_str()))
        })
    });
    let formula_step_model: Option<(&str, &str, &str)> = req.formula.and_then(|f| {
        f.steps.get(req.current_step).and_then(|step| {
            step.model
                .as_deref()
                .map(|id| (id, f.name.as_str(), step.id.as_str()))
        })
    });

    let (adapter_name, adapter_source) = resolve_adapter_selection(
        req.adapter_flag,
        formula_step_adapter,
        req.env_default_adapter,
        req.project_adapters,
        req.config_path,
        req.global_adapters,
        req.global_config_path,
    );
    let adapter_source = sharpen_adapter_fallback(adapter_source, req.formula_absence);

    // Compose the full dispatch registry: built-in Adapter names ∪ TOML
    // `[adapters]` extras. The built-in half is read from
    // `spawn_seam::built_in_adapter_names`, never re-typed here — a second
    // inventory is a first inventory that will one day disagree.
    let mut declared_names: Vec<String> = built_in_adapter_names()
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    if let Some(adapters) = req.project_adapters {
        declared_names.extend(AdaptersConfig::available_names(adapters));
    }
    let (adapter, supervision, loop_ownership_from_validator) =
        validate_adapter_name(&adapter_name, &declared_names)?;

    let (loop_ownership, ownership_warning) = resolve_loop_ownership(
        adapter.as_str(),
        loop_ownership_from_validator,
        req.project_adapters.and_then(|cfg| cfg.entry(adapter.as_str())),
    );

    let (preferred_model, model_source) = resolve_model_selection(
        req.model_flag,
        formula_step_model,
        req.env_default_model,
        adapter.as_str(),
        req.project_adapters,
        req.config_path,
        req.global_adapters,
        req.global_config_path,
    );
    let model_source = sharpen_model_fallback(model_source, req.formula_absence);

    Ok(TackleSelection {
        adapter,
        adapter_source,
        supervision,
        loop_ownership,
        ownership_warning,
        preferred_model,
        model_source,
    })
}

/// Everything [`TacklePlan::from_parts`] needs beyond a [`TackleSelection`]:
/// the molecule projection, the already-resolved effect-half inputs, and the
/// prompt's raw material.
#[derive(Debug, Clone, Copy)]
pub struct PromptRequest<'a> {
    /// The molecule being tackled.
    pub molecule: MoleculeBrief<'a>,
    /// The resolved formula, when its id resolved.
    pub formula: Option<&'a Formula>,
    /// The briefing text the worker receives, already resolved by the caller
    /// (file, fleet template injection, committee posture delivery — all
    /// effect-half work).
    pub briefing: Option<&'a str>,
    /// The per-galaxy project config (gates, worker policy, attribution).
    pub config: &'a ProjectConfig,
    /// The canonical molecule state directory, named in the brief as the
    /// durable artifact destination for coding-agent workers.
    pub molecule_dir: &'a Path,
    /// `--workdir` override, if the operator passed one.
    pub workdir: Option<&'a str>,
    /// `--no-worktree`: park the worker on the main checkout.
    pub no_worktree: bool,
    /// The repository root, when the dispatch runs inside a git repository —
    /// resolved by the caller (a `git` probe is effect-half work). `None`
    /// degrades the sandbox-root prediction to relative-paths wording.
    pub repo_root: Option<&'a Path>,
}

/// The full pure decision record for one tackle dispatch.
///
/// Everything the effect half consumes that phases 1–6 of the historical
/// `cmd::tackle::run` decided, as one typed value. The fields that remain
/// with the effect half (worker argv + env allow-list, ledger record) join
/// in U5 when the executor seam lands.
#[derive(Debug, Clone)]
pub struct TacklePlan {
    /// The molecule this plan dispatches.
    pub molecule_id: MoleculeId,
    /// The branch the effect half creates for the worker's worktree —
    /// `feat/<mol-id>`, the shape `cs done` merges back.
    pub branch_name: String,
    /// The directory the worker's tools will write into (worktree, or the
    /// `--workdir` override, or the repo root under `--no-worktree`).
    /// `None` when no git repository was found.
    pub worktree_path: Option<PathBuf>,
    /// The integration base branch persisted on the molecule, when one was
    /// named — validated by the caller against `git` before planning.
    pub base_branch: Option<String>,
    /// The reviewed-tree pin start point (committee seats), when the
    /// molecule carries one — resolved by the caller against `git`.
    pub reviewed_start_point: Option<String>,
    /// The validated adapter to dispatch.
    pub adapter: ValidatedAdapterName,
    /// Where the adapter came from — the `AdapterSelected` event payload.
    pub adapter_source: AdapterSelectionSource,
    /// The supervision axis for the spawn (tmux pane vs in-process).
    pub supervision: SupervisionMode,
    /// The loop-ownership axis (ADR-103).
    pub loop_ownership: LoopOwnership,
    /// The model pin (post any caller-applied budget gate); `None` = the
    /// adapter's own default.
    pub preferred_model: Option<String>,
    /// Where the model pin came from — the `ModelSelected` event payload.
    pub model_source: ModelSelectionSource,
    /// The full bootstrap prompt handed to the worker. This is also the
    /// exact `--dry-run` rendering.
    pub prompt: String,
}

impl TacklePlan {
    /// Assemble the plan from an already-gated [`TackleSelection`] plus the
    /// prompt inputs — the second pure stage, run after the caller's
    /// interleaved gates (capability, model budget) and effect-half
    /// resolutions (base branch, reviewed pin, briefing).
    #[must_use]
    pub fn from_parts(
        selection: TackleSelection,
        req: &PromptRequest<'_>,
        base_branch: Option<String>,
        reviewed_start_point: Option<String>,
    ) -> Self {
        let sandbox_root = predicted_sandbox_root(
            req.workdir,
            req.no_worktree,
            req.molecule.id.as_str(),
            req.repo_root,
        );
        let prompt = build_prompt(
            &req.molecule,
            req.formula,
            req.briefing,
            req.config,
            req.molecule_dir,
            selection.adapter.as_str(),
            sandbox_root.as_deref(),
        );
        TacklePlan {
            molecule_id: req.molecule.id.clone(),
            branch_name: format!("feat/{}", req.molecule.id),
            worktree_path: sandbox_root,
            base_branch,
            reviewed_start_point,
            adapter: selection.adapter,
            adapter_source: selection.adapter_source,
            supervision: selection.supervision,
            loop_ownership: selection.loop_ownership,
            preferred_model: selection.preferred_model,
            model_source: selection.model_source,
            prompt,
        }
    }

    /// One-shot pure resolution: [`resolve_selection`] then
    /// [`TacklePlan::from_parts`], for callers with no interleaved gates.
    ///
    /// Applies **no strong-model budget ceiling** — that gate folds the
    /// event log, an effect-half input. A caller with budget history runs
    /// the two stages separately with the gate between, as the CLI does.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownAdapter`] when the resolved adapter name is not in
    /// the dispatch registry.
    pub fn resolve(
        selection_req: &SelectionRequest<'_>,
        prompt_req: &PromptRequest<'_>,
        base_branch: Option<String>,
        reviewed_start_point: Option<String>,
    ) -> Result<Self, UnknownAdapter> {
        let selection = resolve_selection(selection_req)?;
        Ok(Self::from_parts(
            selection,
            prompt_req,
            base_branch,
            reviewed_start_point,
        ))
    }
}

/// Resolve the Worker-Spawn Port Adapter name for a tackle (ADR-097 / C6;
/// ADR-108 Q5a chain).
///
/// Walks the six-level resolution chain, highest priority first:
///
/// 1. `--adapter <name>` (flag passed) → [`AdapterSelectionSource::Cli`].
/// 2. **formula step `adapter = "<name>"`** → [`AdapterSelectionSource::FormulaStep`].
/// 3. `$COSMON_DEFAULT_ADAPTER` (set non-empty) → [`AdapterSelectionSource::EnvVar`].
/// 4. per-galaxy `.cosmon/config.toml::[adapters.default]` → [`AdapterSelectionSource::Config`].
/// 5. global `~/.config/cosmon/config.toml::[adapters.default]` → [`AdapterSelectionSource::GlobalConfig`].
/// 6. Built-in floor [`BUILTIN_FLOOR_ADAPTER`] → [`AdapterSelectionSource::Default`].
///
/// **The loci and what each carries** (Q5a, plus the two
/// operator-preference tiers):
///
/// - **`--adapter` flag** — the operator's in-the-moment choice. Always wins.
/// - **formula step adapter** — the per-workflow *override*. A step may
///   legitimately pin `adapter = "claude"` (e.g. a `deep-think` panel needs
///   frontier reasoning) *regardless of any default*. Ranks above every
///   default, below the flag.
/// - **`$COSMON_DEFAULT_ADAPTER`** — the operator's *session hammer*: a
///   single `export` that flips the default everywhere, this shell, right
///   now, with no committed config. It outranks **both** config files (it
///   is the explicit live intent) but stays **below the formula-step pin**:
///   a step expressing a correctness need must not be silently overridden
///   by a blanket env preference. An empty string is treated as unset.
/// - **per-galaxy `[adapters.default]`** — the committed project *policy*.
/// - **global `[adapters.default]`** — the operator's *machine preference*,
///   consulted only when the per-galaxy config carries no default, so a
///   committed per-galaxy choice always wins over the uncommitted
///   machine-wide one.
/// - **floor constant [`BUILTIN_FLOOR_ADAPTER`]** — the invariant *floor*:
///   "no config = local autonomy".
///   **Config-undeletable *and* copy-undeletable by construction** —
///   deleting every config row, unsetting the env, falls through to this
///   one constant (spelled exactly once, in [`crate::config`]), never
///   to Claude.
///
/// The opt-in escape to Claude therefore exists at *every* level, which IS
/// the operator's decision (iii): "Claude becomes an opt-in adapter."
///
/// `formula_step_adapter` is `(adapter_name, formula_name, step_id)` for the
/// currently executing step, or `None` when there is no formula, the step
/// does not pin an adapter, or the dispatch is not formula-driven.
///
/// `env_default` is the value of `$COSMON_DEFAULT_ADAPTER` (caller-read);
/// an empty string is treated as unset and falls through.
///
/// `config_path` / `global_config_path` are the paths the resolver actually
/// read; each appears verbatim on its variant so a retrospective audit can
/// distinguish a per-galaxy override from a global one from a built-in
/// fallback.
#[must_use]
pub fn resolve_adapter_selection(
    flag: Option<&str>,
    formula_step_adapter: Option<(&str, &str, &str)>,
    env_default: Option<&str>,
    adapters_cfg: Option<&AdaptersConfig>,
    config_path: &Path,
    global_adapters_cfg: Option<&AdaptersConfig>,
    global_config_path: &Path,
) -> (String, AdapterSelectionSource) {
    if let Some(name) = flag {
        return (
            name.to_owned(),
            AdapterSelectionSource::Cli {
                flag: name.to_owned(),
            },
        );
    }
    if let Some((name, formula, step_id)) = formula_step_adapter {
        return (
            name.to_owned(),
            AdapterSelectionSource::FormulaStep {
                formula: formula.to_owned(),
                step_id: step_id.to_owned(),
            },
        );
    }
    // Q5a extension (C99E): the operator's session hammer. Empty string =
    // unset (falls through), so `COSMON_DEFAULT_ADAPTER= cs tackle` does
    // not pin a nonsensical empty adapter name.
    if let Some(name) = env_default.filter(|s| !s.is_empty()) {
        return (
            name.to_owned(),
            AdapterSelectionSource::EnvVar {
                var: "COSMON_DEFAULT_ADAPTER".to_owned(),
            },
        );
    }
    if let Some(cfg) = adapters_cfg {
        if let Some(name) = cfg.default_adapter() {
            return (
                name.to_owned(),
                AdapterSelectionSource::Config {
                    path: config_path.to_string_lossy().into_owned(),
                    key: "adapters.default".to_owned(),
                },
            );
        }
    }
    // Q5a extension (C99E): the operator's machine-wide preference,
    // consulted only when the per-galaxy config declared no default.
    if let Some(cfg) = global_adapters_cfg {
        if let Some(name) = cfg.default_adapter() {
            return (
                name.to_owned(),
                AdapterSelectionSource::GlobalConfig {
                    path: global_config_path.to_string_lossy().into_owned(),
                },
            );
        }
    }
    (
        BUILTIN_FLOOR_ADAPTER.to_owned(),
        AdapterSelectionSource::Default {
            fallback_reason: "no --adapter flag, no formula-step adapter pin, no \
                              $COSMON_DEFAULT_ADAPTER, and no [adapters.default] in \
                              either the per-galaxy or global config; using built-in \
                              'local' (Ollama-backed in-process loop, no Claude Code \
                              in the default path)"
                .to_owned(),
        },
    )
}

/// Resolve the per-molecule **model** pin (delib-20260704-b476 C1) — the
/// model sibling of [`resolve_adapter_selection`], a verbatim shape-clone of
/// its chain.
///
/// Walks the six-level resolution chain, highest priority first:
///
/// 1. `--model <id>` (flag passed) → [`ModelSelectionSource::Flag`].
/// 2. formula step `model = "<id>"` → [`ModelSelectionSource::FormulaPin`].
/// 3. a model env var (`$COSMON_DEFAULT_MODEL`, else the legacy
///    `$ANTHROPIC_MODEL`) → [`ModelSelectionSource::EnvVar`].
/// 4. per-galaxy `[adapters.<name>].default_model` →
///    [`ModelSelectionSource::Config`].
/// 5. global `[adapters.<name>].default_model` →
///    [`ModelSelectionSource::GlobalConfig`].
/// 6. **floor `None`** → [`ModelSelectionSource::Default`]: cosmon pins no
///    model and the adapter's own default applies.
///
/// **Two structural differences from the adapter chain**, both load-bearing:
///
/// - **The floor is `None`, not a named constant** (von-neumann's minimax).
///   A strong floor's worst case is a silent frontier dispatch with zero
///   operator intent; `None`'s worst case is "the adapter runs its own
///   default", strictly dominated and byte-identical to today's no-pin
///   path. So the return type is `Option<String>`, not `String`.
/// - **The config tiers are scoped to `adapter_name`**
///   (`[adapters.<name>].default_model`), because a model id only has
///   meaning inside its adapter — unlike `[adapters.default]`, which names
///   the adapter itself.
///
/// `formula_step_model` is `(model_id, formula_name, step_id)` for the
/// currently executing step, or `None`. `env_default` is
/// `(value, var_name)` — the caller resolves `$COSMON_DEFAULT_MODEL` then
/// the legacy `$ANTHROPIC_MODEL` and passes whichever fired, with its name,
/// so the recorded source names the exact origin. An empty string is
/// treated as unset (the caller already filters, kept here as defence).
///
/// **Safe-default note (C4).** This resolver builds the full chain but does
/// **not** enforce the "config/env may not resolve to a *strong* model"
/// guard — that is the model-budget gate's job, run by the caller with the
/// event-log history in hand. C1 must not itself wire a config path that
/// *silently defaults* to strong; here it does not — a config
/// `default_model` is only consulted when no positive per-molecule act
/// (flag / pin) fired. The [`ModelSelectionSource`] is carried out verbatim
/// so the `ModelSelected` event and the budget guards can read the origin.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn resolve_model_selection(
    flag: Option<&str>,
    formula_step_model: Option<(&str, &str, &str)>,
    env_default: Option<(&str, &str)>,
    adapter_name: &str,
    adapters_cfg: Option<&AdaptersConfig>,
    config_path: &Path,
    global_adapters_cfg: Option<&AdaptersConfig>,
    global_config_path: &Path,
) -> (Option<String>, ModelSelectionSource) {
    if let Some(id) = flag.filter(|s| !s.is_empty()) {
        return (
            Some(id.to_owned()),
            ModelSelectionSource::Flag {
                flag: id.to_owned(),
            },
        );
    }
    if let Some((id, formula, step_id)) = formula_step_model {
        return (
            Some(id.to_owned()),
            ModelSelectionSource::FormulaPin {
                formula: formula.to_owned(),
                step_id: step_id.to_owned(),
            },
        );
    }
    // The operator's session hammer. Empty string = unset (falls through).
    if let Some((value, var)) = env_default.filter(|(v, _)| !v.is_empty()) {
        return (
            Some(value.to_owned()),
            ModelSelectionSource::EnvVar {
                var: var.to_owned(),
            },
        );
    }
    // Config tiers are scoped to the resolved adapter — a model id only has
    // meaning inside its adapter.
    if let Some(id) = adapters_cfg
        .and_then(|cfg| cfg.entry(adapter_name))
        .and_then(|entry| entry.default_model.as_deref())
        .filter(|s| !s.is_empty())
    {
        return (
            Some(id.to_owned()),
            ModelSelectionSource::Config {
                path: config_path.to_string_lossy().into_owned(),
                key: format!("adapters.{adapter_name}.default_model"),
            },
        );
    }
    if let Some(id) = global_adapters_cfg
        .and_then(|cfg| cfg.entry(adapter_name))
        .and_then(|entry| entry.default_model.as_deref())
        .filter(|s| !s.is_empty())
    {
        return (
            Some(id.to_owned()),
            ModelSelectionSource::GlobalConfig {
                path: global_config_path.to_string_lossy().into_owned(),
            },
        );
    }
    (
        None,
        ModelSelectionSource::Default {
            fallback_reason: format!(
                "no --model flag, no formula-step model pin, no \
                 $COSMON_DEFAULT_MODEL / $ANTHROPIC_MODEL, and no \
                 [adapters.{adapter_name}].default_model in either the \
                 per-galaxy or global config; pinning no model (the adapter's \
                 own default applies — strong is never reachable from silence)"
            ),
        },
    )
}

/// Restate a `Default` adapter fallback in terms of the *real* cause when the
/// molecule's formula id did not resolve (task-20260725-eb3b).
///
/// Only the `Default` variant is touched, and only when the formula failed to
/// resolve: every other source came from a pin that actually fired, and a
/// formula that loaded and simply declares no `adapter` is a genuine absence
/// the existing wording already describes correctly.
#[must_use]
pub fn sharpen_adapter_fallback(
    source: AdapterSelectionSource,
    formula_absence: Option<&str>,
) -> AdapterSelectionSource {
    match (source, formula_absence) {
        (AdapterSelectionSource::Default { fallback_reason }, Some(absence)) => {
            AdapterSelectionSource::Default {
                fallback_reason: format!("{absence}; {fallback_reason}"),
            }
        }
        (other, _) => other,
    }
}

/// The model sibling of [`sharpen_adapter_fallback`] — same rule, same reason.
#[must_use]
pub fn sharpen_model_fallback(
    source: ModelSelectionSource,
    formula_absence: Option<&str>,
) -> ModelSelectionSource {
    match (source, formula_absence) {
        (ModelSelectionSource::Default { fallback_reason }, Some(absence)) => {
            ModelSelectionSource::Default {
                fallback_reason: format!("{absence}; {fallback_reason}"),
            }
        }
        (other, _) => other,
    }
}

/// Resolve the per-Adapter [`LoopOwnership`] axis (ADR-103).
///
/// Built-in names (`claude`, `aider`, `codex`, `openai`, `anthropic`)
/// take the validator's verdict verbatim — the
/// [`BUILT_IN_AXES`](crate::spawn_seam) table is the
/// authoritative source. TOML-only adapters (a `[adapters.<name>]`
/// row whose `<name>` is not built-in) may override the legacy
/// default by declaring `ownership = "cosmon"`; the absence-default
/// preserves the pre-ADR-103 `External` contract.
///
/// Unknown `ownership` strings fall back to the validator's verdict
/// with a **warning returned as data** rather than printed (this module is
/// print-free) and rather than aborting — `cs tackle` must remain
/// dispatch-tolerant of stale operator config. The caller surfaces the
/// warning on its own channel.
#[must_use]
pub fn resolve_loop_ownership(
    adapter_name: &str,
    from_validator: LoopOwnership,
    entry: Option<&AdapterEntry>,
) -> (LoopOwnership, Option<String>) {
    // Built-in adapters: the validator's axis table wins.
    if crate::spawn_seam::axes_for_built_in(adapter_name).is_some() {
        return (from_validator, None);
    }
    // TOML-only adapter: read the row, fall back to the validator's
    // verdict (which is `External` for any caller-supplied name).
    match entry.and_then(|e| e.ownership.as_deref()) {
        Some("cosmon") => (LoopOwnership::Cosmon, None),
        Some("external") | None => (from_validator, None),
        Some(other) => (
            from_validator,
            Some(format!(
                "cs tackle: warning — [adapters.{adapter_name}].ownership = {other:?} \
                 is not recognised ('external' or 'cosmon'); falling back to '{from_validator:?}'"
            )),
        ),
    }
}

/// The **strong cost-class** set for `adapter_name` (delib-20260704-b476 C4),
/// unioned across the per-galaxy and global `[adapters.<name>].strong` rows.
///
/// Union (not per-galaxy-wins) is the fail-open-*and*-conservative choice: a
/// larger strong set classifies *more* models as expensive, which only ever
/// tightens the ceiling — the direction that protects the operator's credits.
/// An id declared strong in either scope is treated as strong.
#[must_use]
pub fn adapter_strong_set(
    project_adapters: Option<&AdaptersConfig>,
    global_adapters: Option<&AdaptersConfig>,
    adapter_name: &str,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for cfg in [project_adapters, global_adapters].into_iter().flatten() {
        if let Some(entry) = cfg.entry(adapter_name) {
            for id in &entry.strong {
                let id = id.trim();
                if !id.is_empty() && !out.iter().any(|s| s == id) {
                    out.push(id.to_owned());
                }
            }
        }
    }
    out
}

/// Name where a model pin came from, in words an operator can act on.
///
/// The composition advisory is only useful if it says which knob to turn:
/// "the pin came from `$ANTHROPIC_MODEL`" points at the shell, "from
/// `--model`" points at the command line, and the two remedies are
/// different. [`ModelSelectionSource`] carries the origin for the audit
/// trail; this renders it for a human reading the advisory.
#[must_use]
pub fn describe_model_source(source: &ModelSelectionSource) -> String {
    match source {
        ModelSelectionSource::Flag { .. } => "the `--model` flag".to_owned(),
        ModelSelectionSource::FormulaPin { formula, step_id } => {
            format!("the formula-step pin `{formula}` / `{step_id}`")
        }
        ModelSelectionSource::EnvVar { var } => format!("the environment variable ${var}"),
        ModelSelectionSource::Config { path, key } => format!("`{key}` in {path}"),
        ModelSelectionSource::GlobalConfig { path } => format!("the global config {path}"),
        // `ModelSelectionSource` is `#[non_exhaustive]` for downstream
        // crates, but this match lives beside the enum: a new origin fails
        // to compile here, which forces its wording to be written with it.
        ModelSelectionSource::Default { .. } => "the no-pin floor".to_owned(),
    }
}

/// The directory this dispatch's worker will actually run in — computed
/// *before* it exists, so `--dry-run` prints the same brief the real
/// dispatch would.
///
/// Mirrors the `worktree_path` expression in the effect half exactly
/// (`--workdir` override → `<repo>/.worktrees/<id>` → repo root under
/// `--no-worktree`); the two must never drift, or a local worker is told a
/// root it does not write into (noogram/cosmon #24). Pure over its inputs so
/// the mapping is unit-testable without a git repository.
#[must_use]
pub fn predicted_sandbox_root(
    workdir: Option<&str>,
    no_worktree: bool,
    mol_id: &str,
    repo_root: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(dir) = workdir {
        return Some(PathBuf::from(dir));
    }
    let repo_root = repo_root?;
    if no_worktree {
        Some(repo_root.to_owned())
    } else {
        Some(repo_root.join(".worktrees").join(mol_id))
    }
}

/// Render the "run verification gates" step of the worker prompt.
///
/// If the project has configured any gate commands under `[gates]` in
/// `.cosmon/config.toml`, render them as an explicit numbered list so the
/// worker runs exactly what the project author specified. Otherwise fall
/// back to a neutral, language-agnostic instruction — cosmon does not
/// assume any particular toolchain.
#[must_use]
pub fn render_gates_instruction(gates: &GatesConfig) -> String {
    use std::fmt::Write;

    if gates.is_empty() {
        return "3. Run the project's verification gates \
                (see .cosmon/config.toml `[gates]` or the project's CLAUDE.md).\n"
            .to_owned();
    }

    let labeled: [(&str, &Option<String>); 7] = [
        ("setup", &gates.setup_command),
        ("build", &gates.build_command),
        ("typecheck", &gates.typecheck_command),
        ("test", &gates.test_command),
        ("lint", &gates.lint_command),
        ("format", &gates.format_command),
        ("doc", &gates.doc_command),
    ];

    let mut out = String::from(
        "3. Run the project's verification gates (from .cosmon/config.toml `[gates]`):\n",
    );
    for (label, cmd) in labeled {
        if let Some(cmd) = cmd {
            let _ = writeln!(out, "   - {label}: `{cmd}`");
        }
    }
    if let Some(test_cmd) = &gates.test_command {
        out.push_str(&render_test_stall_guidance(test_cmd));
    }
    out
}

/// Render the anti-stall guidance that travels with the test gate.
///
/// A workspace-wide test run (`cargo test --workspace`, `go test ./...`,
/// `pytest` over the whole tree) is a *trap* for an autonomous worker: one
/// slow, network-bound, or subprocess-spawning test in an *unrelated* crate
/// can block forever. The test process then sits near 0% CPU and never
/// returns, and a worker that polls it in an until-loop freezes — "active"
/// but making no progress. This is the doctrine of *a worker waiting for a
/// signal that never comes* (delib-20260614-98f2 C2; smithy task-e375).
///
/// The cure is not to weaken the merge contract — the configured gate stays
/// the Definition of Done — but to tell the worker *how* to run it without
/// hanging: scope to the crate it touched while iterating, always wrap the
/// run in a `timeout`, and treat a timeout firing as a finding (a stalled
/// test) rather than a flake to silently retry.
///
/// The note is emitted only when a test gate is configured, and the
/// cargo-specific `-p` / `--lib` hints are shown only when the command is a
/// `cargo` invocation — for every other toolchain the guidance stays
/// generic. An absent test gate leaves the prompt byte-identical.
fn render_test_stall_guidance(test_cmd: &str) -> String {
    use std::fmt::Write;

    let mut note = String::from(
        "   ⚠️ Test-gate anti-stall (doctrine: *a worker waiting for a signal \
         that never comes*). A whole-tree test run can hang forever on ONE \
         slow / network / subprocess-spawning test in an unrelated crate — \
         the process idles near 0% CPU and never returns, freezing this \
         worker. Stay live:\n",
    );
    if test_cmd.contains("cargo") {
        note.push_str(
            "      - Iterate on the crate you touched: `cargo test -p <crate>` \
             (or `--lib` for just the fast unit subset) — not the whole \
             workspace.\n",
        );
    } else {
        note.push_str(
            "      - Iterate on only the package / module you touched, not the \
             whole tree.\n",
        );
    }
    let _ = writeln!(
        note,
        "      - Always wrap the gate in a timeout, e.g. `timeout 600 {test_cmd}`. \
         A timeout firing is a FINDING (a stalled / hanging test), not a flake \
         to silently retry.",
    );
    note.push_str(
        "      - NEVER sit in an until-loop polling a test that shows no \
         progress. Kill it, scope down, and report the offending test.\n",
    );
    note.push_str(
        "      The configured gate stays the merge contract — run it last, \
         under the timeout, once the scoped tests pass.\n",
    );
    note
}

/// Build the bootstrap prompt that gives the agent full context.
///
/// `sandbox_root` is the directory the worker's tools actually write into —
/// the git worktree for a normal dispatch, the workdir for `--no-worktree`.
/// It is `None` only when the path cannot be resolved (no git repository).
/// A **local** worker is told this root and nothing else, because its
/// confined tool registry refuses every path outside it (noogram/cosmon #24).
#[allow(clippy::too_many_lines, clippy::comparison_chain)]
#[must_use]
pub fn build_prompt(
    mol: &MoleculeBrief<'_>,
    formula: Option<&Formula>,
    briefing: Option<&str>,
    config: &ProjectConfig,
    molecule_dir: &Path,
    adapter_name: &str,
    sandbox_root: Option<&Path>,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let kind_str = mol
        .kind
        .map_or_else(|| "molecule".to_owned(), |k| k.to_string());
    let kind_emoji = mol.kind.map_or("🔧", MoleculeKind::emoji);

    // ── AUTONOMOUS WORK MODE HEADER ─────────────────────────────
    // Register note (task-20260727-bbaf). The header and the closing
    // protocol used to be written in imperatives with the reason withheld
    // ("NON-NEGOTIABLE", "This is physics, not politeness", "There is NO
    // other valid way to end"). Two costs, both observed on 2026-07-27:
    // the operator read a worker pane and asked whether prompts had been
    // INJECTED into a running molecule — they were reading our own brief;
    // and task-20260727-1765 correctly refused the blanket order, because
    // its molecule's real state did not support the transition the brief
    // demanded, and was left `running` with the work done. A control a
    // competent owner mistakes for an attack costs trust on every
    // inspection, and an order that conflicts with good judgement gets
    // resisted by exactly the workers you want.
    //
    // So the anti-stall property is now carried by EXPLANATION, not by
    // coercion: the brief states the contract and the cost of breaking it
    // (unattended pane, held molecule slot, a stalled worker that looks
    // healthy), and a model that understands that does not need to be
    // forbidden from pausing. The behavioural target is unchanged and is
    // asserted as a property in
    // `test_build_prompt_states_completion_contract_and_blocked_path`.
    let _ = writeln!(out, "# Autonomous work mode\n");
    let _ = writeln!(
        out,
        "You are a cosmon worker executing {kind_emoji} {kind_str} `{}`.",
        mol.id
    );
    let _ = writeln!(
        out,
        "Formula: `{}` — Step {}/{}\n",
        mol.formula_id,
        mol.current_step + 1,
        mol.total_steps
    );
    out.push_str(
        "Nobody is reading this pane. cosmon dispatched you into a detached \
         session and tracks the molecule's recorded state, not anything you \
         print here. Two consequences shape the protocol at the end of this \
         brief. First, a question asked here reaches no one, so it is never \
         answered. Second, a worker waiting at the prompt is indistinguishable \
         from a worker that is thinking: it holds a molecule slot and reads as \
         healthy to the fleet until a human happens to look, often hours \
         later. So keep moving, and put anything you would have said to an \
         operator into the lifecycle commands instead, where it is recorded \
         and read.\n\n",
    );

    // ── EXTERNAL ATTRIBUTION ────────────────────────────────────
    // Positive supply for the attribution slot (ADR-128). When the
    // `[attribution]` block is configured, fold its one-line directive in
    // HIGH — before the mission — so the worker has the public maker name
    // in hand *before* it reaches a "built by" / author / copyright slot
    // and would otherwise fill the vacuum from private context. Passive
    // helper: an absent/empty block injects nothing and leaves the prompt
    // byte-identical to a pre-attribution cosmon (mirrors the
    // `CLAUDE_CONFIG_DIR` propagation discipline).
    if let Some(directive) = config.attribution.directive() {
        let _ = writeln!(out, "## External attribution\n\n{directive}\n");
    }

    // ── CANONICAL TEXTS — fetch, never generate ─────────────────
    // Standing guideline folded HIGH (before the mission) so the worker
    // carries it *before* it reaches a slot that wants a licence / legal /
    // boilerplate file. A worker that LLM-generates the full canonical text
    // of a standard licence (CC-BY, GPL, MPL, large SPDX texts) trips the
    // Anthropic OUTPUT content-filter, and the API-client retries the
    // identical blocked generation forever — burning tokens with zero
    // progress. This is prevention for the task-20260622-27d3 pathology;
    // the detection half lives in cosmon-provider's typed, non-retryable
    // `ProviderError::OutputFiltered`. (task-20260623-80f9.)
    out.push_str(
        "## Canonical texts — fetch, never generate\n\n\
         NEVER LLM-generate the body of a standard licence, legal notice, or \
         large canonical/boilerplate text (CC-BY, GPL, MPL, Apache-2.0, full \
         SPDX licence texts, long copyright headers). Emitting long canonical \
         legal text trips the model's OUTPUT content-filter, which blocks the \
         response and can wedge the loop retrying the identical blocked \
         generation. **FETCH it from a canonical source instead** — e.g. \
         `curl -fsSL https://creativecommons.org/licenses/by/4.0/legalcode.txt`, \
         the SPDX text registry, or `choosealicense.com` — and write the \
         fetched bytes verbatim. If a fetch is impossible, reference the \
         licence by its SPDX identifier and STOP; do not transcribe the text \
         from memory.\n\n",
    );

    // ── DIAGNOSIS DISCIPLINE — thin pointer, never inlined ──────
    // A single stable pointer line for the root-cause/perf molecule class
    // (the one that shipped machine-green AND wrong fixes on 2026-07-10).
    // The six clauses + checklist are COGNITION and live in the pointed-to
    // guide, which evolves independently; inlining them would rot the brief
    // DNA and force editing every galaxy's copy on each refinement
    // (Transport ≠ Cognition; CLAUDE.md-is-DNA / Leeloo). Passive standing
    // clause, same shape as the Canonical-texts note above. Source:
    // delib-20260711-f62a Q8 / §C-5 (child C7 = task-20260711-7173).
    out.push_str(
        "## Diagnosis discipline (root-cause & perf molecules)\n\n\
         If this molecule claims to fix a **root cause** or a **performance** \
         regression, follow `docs/guides/diagnosis-discipline.md` before trusting \
         any explanation — instrument the seam, run at real scale, and get a \
         cross-provider refutation. The six clauses and the checklist live in that \
         doc (kept out of this brief by Transport ≠ Cognition), not here.\n\n",
    );

    // ── MISSION (from variables) ────────────────────────────────
    if !mol.variables.is_empty() {
        out.push_str("## Mission\n\n");
        // Topic/title first (most important).
        if let Some(topic) = mol.variables.get("topic") {
            let _ = writeln!(out, "**{topic}**\n");
        }
        let mut vars: Vec<_> = mol
            .variables
            .iter()
            .filter(|(k, _)| *k != "topic")
            .collect();
        vars.sort_by_key(|(k, _)| *k);
        for (k, v) in vars {
            let _ = writeln!(out, "- **{k}**: {v}");
        }
        out.push('\n');
    }

    // ── BRIEFING ────────────────────────────────────────────────
    if let Some(briefing) = briefing {
        if !briefing.is_empty() {
            let _ = writeln!(out, "## Briefing\n\n{briefing}\n");
        }
    }

    // ── ARTIFACT PATHS ──────────────────────────────────────────
    // Adapter-aware, because the two worker classes have *different*
    // writable roots and handing either the other one's root produces a
    // worker that reports a path its file is not at (noogram/cosmon #24).
    //
    // - A coding-agent worker (claude & friends) drives a real shell: it
    //   can write anywhere, so it gets the EXACT absolute, already-resolved
    //   canonical molecule_dir — it never has to re-derive the path from
    //   prose, and never abbreviates to the non-canonical
    //   `.cosmon/molecules/<id>/`. The git worktree (`.worktrees/<id>/`) is
    //   destroyed at `cs done`, so durable artifacts written there are lost.
    //   (advisory backstop for the artifact-path-hygiene class; cf.
    //   idea-20260531-107d, delib-20260410-b79f data-loss recurrence).
    //
    // - A local worker runs inside the confined tool registry
    //   (`local_sandbox_registry`), whose `sanitize_join` REFUSES absolute
    //   paths and `..` escapes. The molecule directory is outside its
    //   sandbox: every write there fails. Naming it as the output location
    //   was the root cause of the false "Code written to <molecule_dir>/…"
    //   report an external tester filed as noogram/cosmon #24 — the worker
    //   echoed the only absolute directory the brief named, while its file
    //   had landed in the worktree. So the local worker is told the truth:
    //   its sandbox root, and that relative paths land under it.
    if crate::egress::adapter_is_local(adapter_name) {
        out.push_str("## Where your output goes\n\n");
        match sandbox_root {
            Some(root) => {
                let _ = writeln!(
                    out,
                    "Your sandbox root — the ONLY directory you can write to — is:\n\n\
                     `{}`\n\n\
                     Give every file a path RELATIVE to that root (`{}`, \
                     `docs/plan.md`). Absolute paths and `..` escapes are refused \
                     by your tools. A file you create as `{}` is at \
                     `{}` — when you report where your output is, report THAT \
                     path and no other.",
                    root.display(),
                    DEFAULT_LOCAL_ARTIFACT_EXAMPLE,
                    DEFAULT_LOCAL_ARTIFACT_EXAMPLE,
                    root.join(DEFAULT_LOCAL_ARTIFACT_EXAMPLE).display(),
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "Give every file a path RELATIVE to your working directory \
                     (`{DEFAULT_LOCAL_ARTIFACT_EXAMPLE}`, `docs/plan.md`). Absolute \
                     paths and `..` escapes are refused by your tools.",
                );
            }
        }
        out.push_str(
            "\ncosmon commits what you produce and merges it back into the \
             project when the molecule is torn down — you do not need to move, \
             copy, or commit anything. Do NOT try to write into the molecule's \
             state directory under `.cosmon/`: it is outside your sandbox and \
             every such write fails.\n\n",
        );
    } else {
        let _ = writeln!(
            out,
            "## Artifact paths — write durable output HERE\n\n\
             Canonical molecule directory (resolved): `{}`\n\n\
             Write all durable artifacts (synthesis.md, frame.md, responses/, \
             outcomes.md, plan.md, …) to that absolute path. NEVER write them to \
             the git worktree (`.worktrees/{}/`) — it is DESTROYED when `cs done` \
             tears the session down, and anything left there is lost.\n",
            molecule_dir.display(),
            mol.id
        );
    }

    // ── FULL STEP CHECKLIST (inline, not separate file) ─────────
    if let Some(formula) = formula {
        out.push_str("## Step Checklist\n\n");
        for (i, step) in formula.steps.iter().enumerate() {
            let check = if i < mol.current_step {
                "[x]"
            } else if i == mol.current_step {
                "[>]"
            } else {
                "[ ]"
            };
            let marker = if i == mol.current_step {
                " ◀ CURRENT"
            } else {
                ""
            };
            let _ = writeln!(out, "- {check} **Step {}: {}**{marker}", i + 1, step.title);
            if i == mol.current_step {
                // Expand current step details.
                let _ = writeln!(out, "  {}", step.description);
                if let Some(ref criteria) = step.exit_criteria {
                    let _ = writeln!(out, "  **Exit criteria:** {criteria}");
                }
            }
        }
        out.push('\n');
    }

    // ── EXECUTION PROTOCOL — adapter/capability-aware split ─────
    // Jesse #4 clause 2 (task-20260721-676d). The `claude` / external-CLI
    // coding-agent path drives tmux + a full shell: it can run the gate
    // toolchain, commit to git, and walk the `cs evolve` / `cs complete`
    // lifecycle verbs. A *local* adapter (`local` / `ollama` / `llama-cpp` /
    // `llama`, classified by `egress::adapter_is_local`) is the in-process /
    // detached Direct-API loop of ADR-100 — a small model on the operator's
    // own hardware that does NOT drive tmux/cargo/git/cs. Handing it the
    // coding-agent contract guaranteed it would fail its own briefing (Jesse:
    // "worker briefing assumes a full coding agent"). So the local worker gets
    // a briefing it CAN satisfy: produce the declared deliverable, written to
    // the canonical molecule directory, and let cosmon drive the lifecycle
    // transitions on its behalf. The coding-agent briefing below is left
    // BYTE-IDENTICAL for every non-local adapter.
    //
    // Orthogonality note: the #4 headline guard (a no-op-with-chatter local
    // mission lands NOT-completed via the real-work / acceptance-artifact
    // check) is a different seam and still holds. This split makes a local
    // success *achievable*; the guard keeps a local *failure* honest.
    if crate::egress::adapter_is_local(adapter_name) {
        build_local_worker_protocol(&mut out, mol);
        return out;
    }

    // ── EXECUTION PROTOCOL (coding agent) ───────────────────────
    out.push_str("## Execution Protocol\n\n");
    out.push_str(
        "**IMPORTANT: Use the `cs` CLI for all cosmon operations. \
Do NOT use MCP cosmon_* tools — the MCP server may be running a stale binary. \
The CLI uses walk-up discovery from your working directory and is always correct. \
When unsure of a command's syntax, run `cs --help` or `cs <command> --help`.**\n\n",
    );
    out.push_str("For EACH step:\n");
    out.push_str("1. Read the project's CLAUDE.md for conventions (if it exists).\n");
    out.push_str("2. Implement the step, meeting its exit criteria.\n");
    out.push_str(&render_gates_instruction(&config.gates));
    out.push_str("4. Commit your changes.\n");

    // Steps 5+ vary based on on_complete config.
    let on_complete = config.worker.on_complete;
    match on_complete {
        OnComplete::CommitPush | OnComplete::CommitPushPr => {
            out.push_str("5. Push your branch: `git push -u origin HEAD`\n");
            let _ = writeln!(
                out,
                "6. Advance: `cs evolve {} --evidence \"<summary>\" --formula .cosmon/formulas/{}.formula.toml`",
                mol.id, mol.formula_id
            );
            out.push_str(
                "7. Go straight into the next step. There is nobody here to \
                 check in with, and a pause between steps is invisible to the \
                 fleet.\n\n",
            );
        }
        OnComplete::Commit => {
            let _ = writeln!(
                out,
                "5. Advance: `cs evolve {} --evidence \"<summary>\" --formula .cosmon/formulas/{}.formula.toml`",
                mol.id, mol.formula_id
            );
            out.push_str(
                "6. Go straight into the next step. There is nobody here to \
                 check in with, and a pause between steps is invisible to the \
                 fleet.\n\n",
            );
        }
    }

    // ── COMPLETION CONTRACT ─────────────────────────────────────
    // Both branches, in one place: the transition that ends the molecule,
    // and the sanctioned path for a state that does not support it.
    push_completion_contract(&mut out, mol, on_complete);

    // ── WHAT STALLS THE FLEET ───────────────────────────────────
    // The former "DO NOT — These are violations" list. Same observed
    // failure modes, each now stated with its cost instead of as a bare
    // prohibition — a worker that knows *why* a pause is harmful does not
    // need to be forbidden from pausing, and the section no longer reads
    // like an instruction someone injected into a running session.
    out.push_str("## What stalls the fleet\n\n");
    out.push_str(
        "Each of these has actually held a molecule slot open on this fleet. \
         They share one shape: the worker addressed an operator who was not \
         there.\n\n",
    );
    out.push_str(
        "- Pausing between steps to summarise what you did. The summary is \
         read by nobody, and the molecule sits at `running` while it waits \
         to be read.\n",
    );
    out.push_str(
        "- Asking \"shall I continue?\" or \"would you like me to proceed?\". \
         No answer is coming. Decide, act, and record the decision in the \
         `--evidence` of your next `cs evolve`, where a human can find it \
         afterwards.\n",
    );
    out.push_str(
        "- Offering alternatives and waiting for a pick. Same shape: pick the \
         one you would defend, do it, and say which and why in the evidence.\n",
    );
    out.push_str(
        "- Sitting at the ❯ prompt for input. This is the mute-hang the fleet \
         cannot distinguish from healthy work; it is the single most \
         expensive failure mode here.\n",
    );

    // Scope boundaries — deliberately NOT bullets of the stall list above.
    // They are not failure modes; they say how far this molecule's
    // integration reaches, which varies with on_complete. Filing them
    // under the stall list was what made the old section read as one
    // undifferentiated wall of prohibitions.
    match on_complete {
        OnComplete::Commit => {
            out.push_str(
                "\n## How far integration goes\n\n\
                 - Do NOT create GitHub PRs — integration is local via molecules.\n\
                 - Do NOT push to remote — commits stay on the local branch; \
                 cosmon merges them when the molecule is harvested.\n\n",
            );
        }
        OnComplete::CommitPush => {
            out.push_str(
                "\n## How far integration goes\n\n\
                 - Do NOT create GitHub PRs — pushing the branch is where this \
                 molecule's integration stops.\n\n",
            );
        }
        OnComplete::CommitPushPr => {
            out.push('\n');
        }
    }

    // ── STARTING POINT ──────────────────────────────────────────
    // Kept LAST on purpose, and the placement is now load-bearing rather
    // than rhetorical. This block is the only line that names which step
    // is current, and the brief is re-read from the tail on a mid-molecule
    // re-prime (`cs prime`) and after a context compaction — the tail is
    // the one region reliably still in view. What changed is the voice: it
    // is a pointer into the checklist above, not a fresh order arriving
    // after the molecule started, which is precisely what an operator
    // reading a live pane on 2026-07-27 mistook for an injected prompt.
    let _ = writeln!(
        out,
        "## ▶ Start here: step {}\n\n\
         Everything you need is above. Start with the work itself rather than \
         a plan of it — a planning summary in this pane is read by nobody, \
         whereas the same reasoning in a `cs evolve --evidence` is kept.",
        mol.current_step + 1
    );

    out
}

/// Append the **completion contract** to `out`: how the molecule ends, and
/// what to do when the real state does not support ending it that way.
///
/// Two branches, deliberately given equal standing.
///
/// The first is the ordinary exit — `cs complete`, preceded by whatever
/// integration `on_complete` configures. This is the transition the fleet
/// waits on; a worker that finishes its work and prints a summary instead
/// leaves the molecule `running` forever.
///
/// The second is the branch the old brief did not have, and its absence
/// cost us a molecule. The text used to say the completion transition was
/// the ONLY valid way to end. On 2026-07-27 `task-20260727-1765` finished
/// and committed its deliverable, found that the molecule's real state did
/// not support the transition, and refused to fabricate one — correctly,
/// on the substance. It was left `running` with the work done, because our
/// own prompt had put a good judgement in conflict with a blanket order
/// and offered no third door. A worker that discovers the state does not
/// support completion is doing its job, and it needs a *sanctioned* way to
/// say so; otherwise the only two moves are a false green or a silent
/// stall, and both are worse than the truth. `cs note` plus `cs collapse`
/// make "not completable" a path through the protocol rather than a
/// violation of it.
fn push_completion_contract(out: &mut String, mol: &MoleculeBrief<'_>, on_complete: OnComplete) {
    use std::fmt::Write;

    out.push_str("## Finishing\n\n");

    match on_complete {
        OnComplete::CommitPushPr => {
            let _ = writeln!(
                out,
                "When every step is done:\n\
                 1. Push your branch: `git push -u origin HEAD`\n\
                 2. Create a pull request: `gh pr create --title \"<title>\" --body \"<summary>\"`\n\
                 3. Record the completion:\n\
                 ```\n\
                 cs complete {} --reason \"<summary>\"\n\
                 ```",
                mol.id
            );
        }
        OnComplete::CommitPush => {
            let _ = writeln!(
                out,
                "When every step is done:\n\
                 1. Push your branch: `git push -u origin HEAD`\n\
                 2. Record the completion:\n\
                 ```\n\
                 cs complete {} --reason \"<summary>\"\n\
                 ```",
                mol.id
            );
        }
        OnComplete::Commit => {
            let _ = writeln!(
                out,
                "When every step is done, record the completion:\n\
                 ```\n\
                 cs complete {} --reason \"<summary>\"\n\
                 ```",
                mol.id
            );
        }
    }

    out.push_str(
        "\nThat command is what ends the molecule. A closing summary written \
         in this pane instead ends nothing: the work is done and the molecule \
         still reads as `running`, so whatever is blocked on it stays \
         blocked. Put the summary in `--reason`, where it is kept.\n\n",
    );

    // ── THE SANCTIONED NOT-COMPLETABLE PATH ─────────────────────
    out.push_str("### When the real state does not support completing\n\n");
    out.push_str(
        "Sometimes it does not, and finding that out is real work, not a \
         failure to follow instructions. The mission may rest on a premise \
         that turned out to be false; a gate may be red for a cause outside \
         this molecule; the deliverable may exist while the exit criteria \
         genuinely are not met.\n\n",
    );
    out.push_str(
        "In that case do NOT call `cs complete` to satisfy this protocol. A \
         completion the state does not support is worse than no completion, \
         because it launders a stall into a green result that the rest of the \
         DAG then builds on. Refusing it is the right call.\n\n",
    );
    out.push_str(
        "It is also not a reason to stop and wait, which is the same silent \
         hang by another route. Say it through the lifecycle, so the finding \
         is recorded rather than stranded in a pane nobody opens:\n\n",
    );
    let _ = writeln!(
        out,
        "1. Commit the real work you did. It must not be lost with the \
         worktree.\n\
         2. Write down what you found:\n\
         ```\n\
         cs note {id} \"<what is actually true, and what it blocks>\"\n\
         ```\n\
         3. End the molecule honestly, naming the cause:\n\
         ```\n\
         cs collapse {id} --reason \"<why completion is not supported>\" \\\n\
         \x20   --reason-kind blocker_stuck\n\
         ```\n\
         Use `gate_failed` instead when a verification gate is what stands in \
         the way, or `resource_exhausted` when you ran out of something you \
         cannot obtain here. Then stop — the molecule is in a terminal state \
         a human can read and act on, which is the outcome you were after \
         when you considered asking.",
        id = mol.id
    );
    out.push('\n');
}

/// Append the **local-worker** execution protocol to `out`.
///
/// A local adapter is the in-process / detached Direct-API loop (ADR-100): a
/// model running on the operator's own hardware with no shell, no tmux, no git
/// and no `cs` command. The coding-agent protocol (gate toolchain, commit,
/// `cs evolve` / `cs complete`) is a contract it can never satisfy — handing it
/// over is exactly the "worker briefing assumes a full coding agent" defect
/// (Jesse #4 clause 2, task-20260721-676d). This protocol asks for the one
/// thing a local model CAN produce: the declared deliverable, written into the
/// canonical molecule directory. cosmon drives the lifecycle transitions on the
/// worker's behalf, so none of the coding-agent-only directives appear here.
///
/// Deliberately free of the tokens the coding-agent path emits (`cargo`,
/// `git commit`, `cs evolve`, `cs complete`, "run all gates") so the two
/// briefings are textually distinguishable — the regression contract in
/// `test_build_prompt_local_adapter_drops_coding_agent_directives`.
fn build_local_worker_protocol(out: &mut String, mol: &MoleculeBrief<'_>) {
    use std::fmt::Write;

    out.push_str("## Execution Protocol (local worker)\n\n");
    out.push_str(
        "You are a **local, in-process worker** — a model running on the \
         operator's own hardware through cosmon's Direct-API loop. You are NOT \
         a coding agent: you have no shell, no terminal, no version control and \
         no `cs` command. Do not attempt to run any build, test, lint, format or \
         documentation tooling; do not commit; do not run any lifecycle command. \
         cosmon records your progress and completion for you.\n\n",
    );
    out.push_str(
        "Your one job is to PRODUCE THE DELIVERABLE this molecule declares and \
         write it into your sandbox root, using a relative path (see \"Where \
         your output goes\" above).\n\n",
    );
    out.push_str("For EACH step:\n");
    out.push_str("1. Read the step's description and exit criteria above.\n");
    out.push_str(
        "2. Write the artifact it asks for as a real file under your sandbox \
         root, with a relative path (Markdown unless the step names another \
         format). Empty chatter is not a deliverable — the file must contain \
         the actual work.\n",
    );
    out.push_str(
        "3. Go straight into the next step. There is nobody here to check in \
         with.\n\n",
    );

    // Completion contract, in the vocabulary this worker actually has. It
    // owns no lifecycle verb, so "finishing" means "the file exists and is
    // real", and the not-completable branch — the same branch the coding
    // agent gets via `cs note` / `cs collapse` — has to be carried by the
    // file itself, which is the only channel out of this worker that
    // anybody reads. Kept free of the coding-agent tokens the regression
    // contract in
    // `test_build_prompt_local_adapter_drops_coding_agent_directives`
    // forbids here.
    out.push_str("## Finishing\n\n");
    out.push_str(
        "You are done when the file exists and contains the actual work. \
         cosmon records the completion for you by looking at what you wrote — \
         a reply that describes the deliverable without writing it lands as a \
         molecule that did nothing.\n\n",
    );
    out.push_str("### When you cannot produce what was asked\n\n");
    out.push_str(
        "If the mission rests on something false, or asks for material you do \
         not have, do not invent a deliverable to satisfy this brief — a \
         fabricated artifact is worse than none, because the work that reads \
         it downstream cannot tell. Do not stop and wait either: nobody is \
         reading this session, so waiting is indistinguishable from working \
         and holds the molecule open.\n\n",
    );
    out.push_str(
        "Write the file anyway, and let it say plainly what you found: what \
         was asked, what is actually true, and what is missing. That is a \
         real deliverable — it is the finding — and it reaches a human, which \
         is what you wanted when you considered asking.\n\n",
    );

    // Kept last: on a re-prime or after truncation, the tail is the region
    // reliably still in view, and this is the only line naming which step
    // is current.
    let _ = writeln!(
        out,
        "## ▶ Start here: step {}\n\n\
         Everything you need is above. Write the artifact rather than a plan \
         of it — the file is the only output of this session that is kept.",
        mol.current_step + 1
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief<'a>(
        id: &'a MoleculeId,
        formula_id: &'a FormulaId,
        variables: &'a HashMap<String, String>,
    ) -> MoleculeBrief<'a> {
        MoleculeBrief {
            id,
            kind: None,
            formula_id,
            current_step: 0,
            total_steps: 2,
            variables,
        }
    }

    /// The U4 falsifier: a full `TacklePlan` built from in-memory values —
    /// no filesystem, no process, no environment. This test could not exist
    /// while the decision half lived inside `cmd::tackle::run`.
    #[test]
    fn tackle_plan_resolves_purely_from_in_memory_state() {
        let id = MoleculeId::new("task-20260904-f4c6").unwrap();
        let formula_id = FormulaId::new("task-work").unwrap();
        let mut variables = HashMap::new();
        variables.insert("topic".to_owned(), "make the tackle plan pure".to_owned());
        let config = ProjectConfig::default();
        let molecule = brief(&id, &formula_id, &variables);

        let selection_req = SelectionRequest {
            adapter_flag: Some("claude"),
            model_flag: None,
            formula: None,
            current_step: 0,
            env_default_adapter: None,
            env_default_model: None,
            project_adapters: None,
            config_path: Path::new("/galaxy/.cosmon/config.toml"),
            global_adapters: None,
            global_config_path: Path::new("/home/.config/cosmon/config.toml"),
            formula_absence: None,
        };
        let prompt_req = PromptRequest {
            molecule,
            formula: None,
            briefing: Some("Extract the pure half."),
            config: &config,
            molecule_dir: Path::new("/galaxy/.cosmon/state/molecules/task-20260904-f4c6"),
            workdir: None,
            no_worktree: false,
            repo_root: Some(Path::new("/galaxy")),
        };

        let plan = TacklePlan::resolve(&selection_req, &prompt_req, Some("main".to_owned()), None)
            .expect("claude is a built-in adapter");

        assert_eq!(plan.adapter.as_str(), "claude");
        assert!(matches!(
            plan.adapter_source,
            AdapterSelectionSource::Cli { .. }
        ));
        // No pin anywhere → the model floor is `None`, source `Default`.
        assert_eq!(plan.preferred_model, None);
        assert!(matches!(
            plan.model_source,
            ModelSelectionSource::Default { .. }
        ));
        assert_eq!(plan.branch_name, "feat/task-20260904-f4c6");
        assert_eq!(
            plan.worktree_path.as_deref(),
            Some(Path::new("/galaxy/.worktrees/task-20260904-f4c6"))
        );
        assert_eq!(plan.base_branch.as_deref(), Some("main"));
        // The prompt is the dry-run rendering: mission + briefing are in it.
        assert!(plan.prompt.contains("make the tackle plan pure"));
        assert!(plan.prompt.contains("Extract the pure half."));
        assert!(plan.prompt.contains("cs complete task-20260904-f4c6"));
    }

    /// An unknown adapter is refused before any plan exists — same typed
    /// refusal the CLI prints, produced without any side effect.
    #[test]
    fn tackle_plan_refuses_unknown_adapter_purely() {
        let req = SelectionRequest {
            adapter_flag: Some("definitely-not-an-adapter"),
            model_flag: None,
            formula: None,
            current_step: 0,
            env_default_adapter: None,
            env_default_model: None,
            project_adapters: None,
            config_path: Path::new("/galaxy/.cosmon/config.toml"),
            global_adapters: None,
            global_config_path: Path::new("/home/.config/cosmon/config.toml"),
            formula_absence: None,
        };
        let err = resolve_selection(&req).expect_err("unknown adapter must refuse");
        assert!(err.to_string().contains("definitely-not-an-adapter"));
    }

    /// With no flag, pin, env, or config, the adapter chain lands on the
    /// built-in floor and the selection says so — the "no config = local
    /// autonomy" invariant, checked from pure values.
    #[test]
    fn selection_floor_is_local_with_default_source() {
        let req = SelectionRequest {
            adapter_flag: None,
            model_flag: None,
            formula: None,
            current_step: 0,
            env_default_adapter: None,
            env_default_model: None,
            project_adapters: None,
            config_path: Path::new("/galaxy/.cosmon/config.toml"),
            global_adapters: None,
            global_config_path: Path::new("/home/.config/cosmon/config.toml"),
            formula_absence: None,
        };
        let selection = resolve_selection(&req).expect("the floor adapter is built-in");
        assert_eq!(selection.adapter.as_str(), BUILTIN_FLOOR_ADAPTER);
        assert!(matches!(
            selection.adapter_source,
            AdapterSelectionSource::Default { .. }
        ));
        assert_eq!(selection.ownership_warning, None);
    }
}
