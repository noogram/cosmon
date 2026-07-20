// SPDX-License-Identifier: AGPL-3.0-only

//! `cs tackle` — one-liner to attack a molecule with an agent.
//!
//! Automates the full launch sequence: resolve molecule, create git worktree
//! for isolation, spawn a tmux session, build a bootstrap prompt from molecule
//! state + formula + briefing, inject it into Claude Code, and mark the
//! molecule as running.
//!
//! ```text
//! cs tackle <mol-id>                     # solo worker
//! cs tackle <mol-id> --fleet research    # with fleet context
//! ```

use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use chrono::Utc;
use cosmon_core::agent::AgentRole;
use cosmon_core::clearance::Clearance;
use cosmon_core::config::{
    AdapterEntry, AdaptersConfig, OnComplete, ProjectConfig, BUILTIN_FLOOR_ADAPTER,
};
use cosmon_core::event_v2::{AdapterSelectionSource, CeilingAction, ModelSelectionSource};
use cosmon_core::fleet::FleetSpec;
use cosmon_core::formula::Formula;
use cosmon_core::id::{AgentId, MoleculeId, WorkerId};
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::spawn_seam::{validate_adapter_name, LoopOwnership, ValidatedAdapterName};
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{DesiredState, WorkerStatus};
use cosmon_filestore::FileStore;
use cosmon_process_witness::process_start_time;
use cosmon_state::events::worker_spawn::{
    emit_adapter_selected, emit_model_ceiling_hit, emit_model_selected,
    emit_worker_spawn_rolled_back,
};
use cosmon_state::{MoleculeData, MoleculeFilter, StateStore, WorkerData};
use cosmon_transport::TmuxBackend;

use super::Context;

/// Tackle a molecule — launch an agent session with full context.
#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Molecule ID, prefix, or fuzzy name (e.g. "ADR-15", "idea-2026").
    pub molecule: String,

    /// Fleet to use (default: molecule's fleet).
    #[arg(long)]
    pub fleet: Option<String>,

    /// Working directory override (default: .worktrees/{mol-id}).
    #[arg(long)]
    pub workdir: Option<String>,

    /// Skip git worktree creation (use current directory).
    #[arg(long)]
    pub no_worktree: bool,

    /// Skip tmux session — print the prompt to stdout instead.
    #[arg(long)]
    pub dry_run: bool,

    /// Permission mode for Claude (default: based on molecule kind).
    #[arg(long)]
    pub permission_mode: Option<String>,

    /// Kill existing tmux session and respawn (instead of attaching).
    #[arg(long)]
    pub force: bool,

    /// Override the tmux session name. ASCII alphanumerics and hyphens
    /// are kept; everything else is replaced with `-`. Max 50 chars.
    /// Default: `{slug}-{shortid}` derived from the molecule topic + id.
    #[arg(long)]
    pub name: Option<String>,

    /// **Deprecated** — silent no-op kept for backward compatibility.
    ///
    /// `cs tackle` is now *always* a leaf-worker dispatch: it never
    /// auto-detects a DAG root and never spawns a resident runtime.
    /// Walking a DAG (1 or N nodes) is exclusively `cs run`'s job.
    /// `--leaf` therefore has no effect; the flag is accepted silently
    /// during a one-month grace window so muscle-memory invocations
    /// keep working. After the grace window, the flag will be removed.
    #[arg(long, hide = true)]
    pub leaf: bool,

    /// **Deprecated** — emits a warning, otherwise no-op.
    ///
    /// Previously bypassed the ADR-048 backlog-sanity guard when
    /// `cs tackle` would auto-route to runtime mode. Since `cs tackle`
    /// is now always leaf, this guard never fires here and the flag is
    /// meaningless. To override the backlog-sanity guard at runtime
    /// bootstrap, pass `--force-runtime` to `cs run` instead. The flag
    /// is accepted with a warning during a one-month grace window.
    #[arg(long, hide = true)]
    pub force_runtime: bool,

    /// Override the ADR-085 stress-test seal at dispatch (Layer 1).
    ///
    /// Without this flag, `cs tackle` of a `--class stress-test` molecule
    /// refuses dispatch unless `prior.md` + `prior.b3` exist on disk and a
    /// matching `cs witness attest` event has been emitted. With it, the
    /// runtime writes a typed [`BypassReceipt`](cosmon_core::molecule_class::BypassReceipt)
    /// to `<molecule_dir>/bypass-receipt.json` and emits
    /// [`EventV2::SealBypassed`](cosmon_core::event_v2::EventV2::SealBypassed),
    /// then proceeds with dispatch. **Requires `--bypass-reason "<…>"`** —
    /// silent overrides are forbidden by ADR-085 §3.5.
    #[arg(long, requires = "bypass_reason")]
    pub bypass_seal: bool,

    /// One-line reason recorded in the [`BypassReceipt`](cosmon_core::molecule_class::BypassReceipt)
    /// when `--bypass-seal` is used.
    ///
    /// Free-text but non-empty; the runtime refuses a blank reason because
    /// the entire point of the receipt is to surface accountability for
    /// the override (ADR-085 §3.5).
    #[arg(long, value_name = "TEXT")]
    pub bypass_reason: Option<String>,

    /// Worker-Spawn Port Adapter to dispatch (ADR-097 / C6; ADR-108 Q5a
    /// chain).
    ///
    /// Resolution order (highest priority first): this flag → formula-step
    /// `adapter = "<name>"` pin → `$COSMON_DEFAULT_ADAPTER` env var →
    /// per-galaxy `.cosmon/config.toml::[adapters.default]` → global
    /// `~/.config/cosmon/config.toml::[adapters.default]` → built-in
    /// `"local"` (the Ollama-backed in-process loop).
    /// Values are looked up against the registered Adapter table (`claude`,
    /// `aider`, `openai`, `anthropic`, `llama-cpp`, `local`, …). An unknown
    /// name aborts the dispatch with a typed `AdapterNotFound` carrying the
    /// list of available names — no silent fallback. To restore the legacy
    /// Claude-Code default pass `--adapter claude`, `export
    /// COSMON_DEFAULT_ADAPTER=claude`, or set `[adapters.default] =
    /// "claude"` in either config file.
    ///
    /// Every invocation (with or without the flag) emits an
    /// [`EventV2::AdapterSelected`](cosmon_core::event_v2::EventV2::AdapterSelected)
    /// envelope so the cat-test (`jq -c 'select(.type == "adapter_selected")'`)
    /// can answer "which Adapter ran for this molecule?" without parsing
    /// shell history.
    #[arg(long, value_name = "NAME")]
    pub adapter: Option<String>,

    /// Per-molecule model pin — the model sibling of `--adapter`
    /// (see ADR-097).
    ///
    /// Resolution order (highest priority first): this flag → formula-step
    /// `model = "<id>"` pin → `$COSMON_DEFAULT_MODEL` (else the legacy
    /// `$ANTHROPIC_MODEL`) env var → per-galaxy
    /// `.cosmon/config.toml::[adapters.<name>].default_model` → global
    /// `~/.config/cosmon/config.toml::[adapters.<name>].default_model` →
    /// **floor `None`** (cosmon pins no model; the adapter's own default
    /// applies — byte-identical to today's no-pin behaviour).
    ///
    /// **Strong is never inherited.** Every dispatch resolves the model
    /// fresh; a strong (frontier) model is reachable only from this flag or
    /// a formula-step pin — a positive per-molecule act — never from a
    /// config/env *default* that could silently make an entire fleet
    /// expensive (the `/model`-hack leak this axis exists to close).
    ///
    /// The id is carried **opaquely**: cosmon does not check that it is
    /// legal for the resolved adapter — the backend rejects an invalid
    /// `(adapter, model)` pair at launch (composition validation lands in
    /// C5). Config `default_model` rows are scoped per adapter because a
    /// model id only has meaning inside its adapter.
    #[arg(long, value_name = "MODEL_ID")]
    pub model: Option<String>,

    /// Forensic-only role-of-origin hint propagated through to
    /// [`EventV2::AdapterSelected`](cosmon_core::event_v2::EventV2::AdapterSelected)
    /// (ADR-097 / C6).
    ///
    /// Cosmon does not interpret this value — it is the academy-shim's
    /// channel for preserving the driver's vocabulary (a `--role
    /// researcher` invocation on the driver side becomes
    /// `role_hint: "researcher"` on the cosmon event), so the role of
    /// origin survives the seam between driver (roles) and cosmon
    /// (adapters). Optional; absent for direct operator invocations.
    #[arg(long, value_name = "ROLE")]
    pub role_hint: Option<String>,

    /// Loud opt-in fallback from the local default to a remote oracle after
    /// a *decidable* local hard-failure (Q5b).
    ///
    /// Pass a [`LocalFailureCause`](cosmon_core::egress::LocalFailureCause)
    /// token — `crash`, `oom`, `timeout`, `connection-refused`, or any
    /// bespoke string (recorded verbatim as `Other`). This flag is the ONLY
    /// path from a local hard-failure to a remote oracle: there is no
    /// automatic in-loop fallback. It is meaningful only alongside a remote
    /// `--adapter` (claude / openai / anthropic / aider) — combining it with
    /// a local adapter is a contradiction and aborts the dispatch.
    ///
    /// When set, `cs tackle` mints an
    /// [`EventV2::LocalFallback`](cosmon_core::event_v2::EventV2::LocalFallback)
    /// line in the *same atom* as the `RemoteEgressOptIn` egress grant, so a
    /// remote call carrying a fallback cause can never reach the wire without
    /// a matching loud audit record — silent fallback is impossible by
    /// construction. Soft "the output looked bad"
    /// judgement is NOT a valid cause: that is undecidable (Rice) and belongs
    /// to acceptance tests, not this routing flag.
    #[arg(long, value_name = "CAUSE")]
    pub fallback_from_local: Option<String>,

    /// Actor class recording **who** dispatched this molecule — the
    /// anti-preemption lease.
    ///
    /// Accepts `human` (the default when the flag is absent — a direct
    /// operator invocation) or `runtime:<pid>` (the resident runtime
    /// `cs run` passes its own process id). The value is stamped onto the
    /// molecule's [`tackled_by`](cosmon_state::MoleculeData::tackled_by)
    /// field when the molecule flips to `Running`, so the walker can
    /// enforce "manual always wins": a human-claimed molecule is never
    /// raffled by the runtime, even if it briefly returns to `Pending` on a
    /// revision. This is `cs tackle`'s only role in the lease — recording
    /// the claim; honouring it is the walker's job.
    #[arg(long = "by", value_name = "ACTOR", default_value = "human")]
    pub by: String,
}

/// Private payload for the detached local-worker transport.
///
/// The parent `cs tackle` serializes the fully resolved launch inputs before
/// it returns. This keeps adapter/model selection in the dispatch process and
/// gives the detached process exactly one job: run the already-authorized
/// local loop and publish its terminal transition.
#[derive(clap::Args)]
pub struct LocalWorkerArgs {
    #[arg(long, hide = true)]
    job: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct LocalWorkerJob {
    adapter_name: String,
    worker_id: String,
    session_name: String,
    worktree_path: PathBuf,
    prompt: String,
    molecule_id: String,
    molecule_dir: PathBuf,
    state_dir: PathBuf,
    adapter_entry: Option<AdapterEntry>,
    preferred_model: Option<String>,
}

/// Maximum tmux session name length accepted by `--name`.
const MAX_SESSION_NAME_LEN: usize = 50;

/// Sanitize and validate a user-supplied session name.
///
/// Replaces disallowed characters with hyphens, collapses runs, trims
/// leading/trailing hyphens, lowercases, and caps at
/// [`MAX_SESSION_NAME_LEN`]. Errors when nothing meaningful remains.
fn sanitize_session_name(raw: &str) -> anyhow::Result<String> {
    let mut buf = String::with_capacity(raw.len());
    let mut last_dash = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            buf.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            buf.push('-');
            last_dash = true;
        }
    }
    let trimmed = buf.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!(
            "--name \"{raw}\" sanitises to an empty string"
        ));
    }
    if trimmed.len() > MAX_SESSION_NAME_LEN {
        return Err(anyhow::anyhow!(
            "--name must be at most {MAX_SESSION_NAME_LEN} chars (got {})",
            trimmed.len()
        ));
    }
    Ok(trimmed)
}

/// Execute the `tackle` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    // First-run consent hook (delib fe35 §c). Fires once per user: if
    // `~/.config/cosmon/consent.toml` is missing and stdin is a TTY, the
    // operator sees a short French prompt asking whether to share
    // encrypted bundles with the developers. On non-tty invocations (CI,
    // scripts, inner worker shells) the hook auto-records a decline so
    // unattended dispatch is never blocked. Best-effort: any failure to
    // persist the answer is logged but never aborts the tackle — consent
    // is a UX layer, not a safety gate.
    if let Err(e) = super::opt_in_share::ensure_consent() {
        eprintln!("cs tackle: could not record consent (non-fatal): {e}");
    }

    // Guard: require project identity before touching transport.
    super::require_project_identity(ctx)?;

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    // Anti-preemption lease (task-20260531-a12f): parse the actor class
    // *before* any side effect so a malformed `--by` aborts fail-fast. The
    // default is `human` (a direct operator invocation); the resident
    // runtime passes `--by runtime:<pid>`. This value is the only thing
    // `cs tackle` contributes to the lease — it records the claim; the
    // walker (`cs run`) honours it.
    let tackled_by: cosmon_core::tackle::TackledBy = args
        .by
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --by value: {e}"))?;

    // 1. Resolve molecule (exact, prefix, or fuzzy).
    let mol = resolve_molecule(&store, &args.molecule)?;
    let mol_id = mol.id.clone();

    // Guard: only tackle alive molecules.
    if !mol.status.is_alive() {
        return Err(anyhow::anyhow!(
            "molecule {} is {} — cannot tackle a terminal molecule",
            mol_id,
            mol.status
        ));
    }

    // Gödel self-reference guards: refuse dispatch when the calling
    // session is a broker or when spawn depth exceeds the configured
    // maximum. These are structural halting conditions — no override flag.
    let env_lookup = |k: &str| std::env::var(k).ok();
    super::guard::refuse_broker_spawn(&env_lookup)?;
    let sr_config = {
        let cfg_path = cosmon_filestore::resolve_config_path(None);
        cosmon_filestore::load_project_config(&cfg_path)
            .unwrap_or_default()
            .self_reference_guard
    };
    super::guard::refuse_excessive_depth(&env_lookup, sr_config.max_depth)?;

    // Convoy-cascade prophylaxis (warn-level): if the target is pending,
    // stale (>2h), and carries no `temp:*` tag, emit a stderr nag. Does
    // not refuse dispatch — the operator may legitimately resurrect a
    // forgotten pending, but they must see what they are doing.
    super::guard::warn_if_stale_untagged(&mol);

    // ADR-085 §M4 — stress-test seal gate (Layer 1).
    //
    // Refuse dispatch on a `--class stress-test` molecule that is missing
    // any of: `prior.md`, `prior.b3`, or a matching `SealAttested` event.
    // `--bypass-seal --bypass-reason "<…>"` overrides the refusal but
    // mints a typed `BypassReceipt` + `SealBypassed` event so the override
    // is durably auditable.
    enforce_stress_test_seal(
        &store,
        &state_dir,
        &mol,
        args.bypass_seal,
        args.bypass_reason.as_deref(),
    )?;

    // Unified dispatch (delib-20260426-1bcd #2 / task-20260426-c33f):
    // `cs tackle` is *always* a leaf dispatch. It never inspects the
    // molecule's outgoing `Blocks` edges, never auto-spawns a resident
    // runtime, never prints a fan-out banner. Walking a DAG of N≥1
    // nodes is exclusively `cs run`'s perimeter. The two paths
    // (auto-detect + `--leaf` escape hatch) collapsed into a single
    // verb because `--leaf` had become a reflex — when the "edge case"
    // flag is the operator's default, the auto-detection is inverted.
    //
    // `--leaf` is still accepted (silent no-op) and `--force-runtime`
    // is accepted with a deprecation warning during a one-month grace
    // window so existing scripts and muscle memory keep working.
    if args.force_runtime {
        eprintln!(
            "warning: `cs tackle --force-runtime` is deprecated and now a no-op. \
             `cs tackle` no longer routes to runtime mode; pass --force-runtime \
             to `cs run` instead to override the backlog-sanity guard."
        );
    }

    // 2. Load formula for context.
    let formula = load_formula_for_molecule(&state_dir, &mol);

    // Briefless-molecule guard (task-20260711-919a). Refuse to dispatch a
    // molecule whose formula declares required, default-free variables that
    // are now missing or blank — a worker would spawn with an empty Mission.
    // This is the dispatch chokepoint shared by manual `cs tackle` and the
    // runtime's `cs run` (which calls `cs tackle`), so one refusal covers
    // both. Load-bearing for the observed pathology: empty-topic `task-work`
    // molecules dispatched after a `cs reconcile` cleared their variables.
    // Corollary of the frontier stuck-frozen fix (9b86) — ready ≠ dispatchable.
    super::guard::refuse_briefless_dispatch(&mol, formula.as_ref())?;

    // Gate routing: if the current step has a `command` field, execute the
    // shell command directly instead of launching a Claude worker. If it has
    // a `native` field, call the registered Rust function. If it has a
    // `[steps.query]` block, evaluate the typed query. If it has `[steps.llm]`,
    // run the checkpointed LLM step. Each path bypasses TransportBackend
    // entirely — no tmux, no worktree session.
    if let Some(ref formula) = formula {
        if let Some(step) = formula.steps.get(mol.current_step) {
            if step.is_gate() {
                return execute_gate(ctx, &store, mol, formula, step);
            }
            if step.is_query() {
                return execute_query(ctx, &store, mol, formula, step);
            }
            if step.is_llm() {
                return execute_llm(ctx, &store, mol, formula, step);
            }
            if step.is_native() {
                // Cascade: native steps are cheap in-process calls, so drain
                // every consecutive native step in one invocation rather than
                // requiring one `cs tackle` per step. Stops at the first
                // non-native step, at molecule completion, or on failure.
                execute_native(ctx, &store, mol, formula, step)?;
                loop {
                    let current = store.load_molecule(&mol_id)?;
                    if !current.status.is_alive() || current.status != MoleculeStatus::Running {
                        return Ok(());
                    }
                    let Some(next_step) = formula.steps.get(current.current_step) else {
                        return Ok(());
                    };
                    if !next_step.is_native() {
                        return Ok(());
                    }
                    execute_native(ctx, &store, current, formula, next_step)?;
                }
            }
        }
    }

    // 3. Load project config (.cosmon/config.toml).
    let config_path = cosmon_filestore::resolve_config_path(None);
    let project_config = cosmon_filestore::load_project_config(&config_path).unwrap_or_default();

    // 3a. Resolve the Worker-Spawn Port Adapter (ADR-097 / C6).
    //
    //     Resolution order (Q5a, task-20260530-c089): --adapter flag →
    //     formula step `adapter = "<name>"` → [adapters.default] in config →
    //     built-in "local" floor. An unknown name aborts with a typed
    //     AdapterNotFound *before* any worktree / tmux side effect lands —
    //     fail-fast on a bad flag.
    //
    //     The formula-step source lets a workflow step pin a specific
    //     adapter (e.g. a deep-think panel needs frontier reasoning) above
    //     the galaxy default but below the operator's explicit flag. We only
    //     reach this block for a worker-spawn step: gate / native / query /
    //     llm steps returned earlier (those execution kinds bypass the
    //     Adapter seam), so the step we read here is always a spawn step.
    //
    //     `AdapterSelected` is emitted here, before the dry-run
    //     short-circuit, so the trace is non-empty on every
    //     Adapter-bound `cs tackle` invocation (including dry-runs
    //     used by integration tests).
    let formula_step_adapter: Option<(&str, &str, &str)> = formula.as_ref().and_then(|f| {
        f.steps.get(mol.current_step).and_then(|step| {
            step.adapter
                .as_deref()
                .map(|name| (name, f.name.as_str(), step.id.as_str()))
        })
    });
    // The model sibling of `formula_step_adapter` (delib-20260704-b476 C1):
    // `(model_id, formula_name, step_id)` for the currently executing step's
    // `model = "<id>"` pin, or `None`. Read from the same step, ranks below
    // `--model` but above every default in `resolve_model_selection`.
    let formula_step_model: Option<(&str, &str, &str)> = formula.as_ref().and_then(|f| {
        f.steps.get(mol.current_step).and_then(|step| {
            step.model
                .as_deref()
                .map(|id| (id, f.name.as_str(), step.id.as_str()))
        })
    });
    // Q5a extension (task-20260531-c99e): two operator-preference tiers
    // layered into the chain — a session-scoped env hammer
    // ($COSMON_DEFAULT_ADAPTER) above both config files, and a global
    // ~/.config/cosmon/config.toml [adapters.default] below the per-galaxy
    // config but above the built-in floor. Both are best-effort reads; a
    // missing or garbled file falls through, it never aborts dispatch.
    let env_default = std::env::var("COSMON_DEFAULT_ADAPTER").ok();
    let global_cfg_path = global_adapter_config_path();
    let global_adapters = load_global_adapters(&global_cfg_path);
    let (adapter_name, selection_source) = resolve_adapter_selection(
        args.adapter.as_deref(),
        formula_step_adapter,
        env_default.as_deref(),
        project_config.adapters.as_ref(),
        &config_path,
        global_adapters.as_ref(),
        &global_cfg_path,
    );
    // Compose the full dispatch registry: built-in Adapter names ∪ TOML
    // `[adapters]` extras. ADR-099 / TS-0 — `validate_adapter_name`
    // returns a [`ValidatedAdapterName`] whose only consumer is the
    // spawn seam, so any future addition to the tackle chain that
    // bypasses this gate stops compiling.
    // ADR-100 R2 wave 2: `openai` is a built-in adapter alongside claude/aider.
    // `anthropic` is the wave-3 stub — registered so validate_adapter_name names
    // it in the diagnostic on `--adapter anthropic`, but spawn_and_prompt
    // refuses with the typed "not yet implemented" diagnostic the stub carries.
    // C3 (`task-20260519-a226`, `delib-20260519-a20b`): `llama-cpp` is the
    // canonical name for the in-process llama.cpp adapter; `llama` is the
    // legacy alias preserved for operator vocabulary. The dispatch registry
    // must list the adapter name independently of the `llama` cargo
    // feature — `validate_adapter_name` is feature-flag oblivious, and a
    // missing `LlamaProvider` surfaces downstream as a typed
    // `FeatureNotCompiled` rather than as `UnknownAdapter` (ADR-100 R2,
    // tolnay rule #6).
    let mut declared_names: Vec<String> = vec![
        "claude".to_owned(),
        "aider".to_owned(),
        // Gap#5 (`task-20260615-df30`, parent `delib-20260615-73f9`):
        // codex is an **external CLI** adapter — the `codex` binary on PATH
        // driven through `codex exec` in a tmux pane. It was already
        // advertised (`root_help`, `man/cs.1`), exit-classified
        // (`adapter_exit.rs`), preflight-probed (`preflight.rs`), and
        // tmux-supervised (`adapter_uses_tmux`), but missing from exactly
        // two places: this registry and the `spawn_and_prompt` match. Once
        // both land, the catch-all build-time-bug arm stops being reachable
        // for codex and the rest works for free. This is also the
        // copy-paste template for the opencode arm (`task-20260615-556a`).
        "codex".to_owned(),
        // `task-20260615-556a` (parent `delib-20260615-73f9`, ADR-125):
        // opencode is the external-CLI sibling of codex — greenfield, so it
        // got the full 5-touch onboarding (BUILT_IN_AXES, this registry,
        // adapter_uses_tmux, preflight, the spawn arm + OpencodeProbe). Same
        // `(TmuxPane, External, Vendor)` Valence; the spawn arm clones the
        // codex one.
        "opencode".to_owned(),
        "openai".to_owned(),
        "anthropic".to_owned(),
        "llama-cpp".to_owned(),
        "llama".to_owned(),
        // `task-20260530-821f`: the walking-skeleton local-default
        // adapter. In-process (no tmux), drives the harness spine
        // through `OpenAIProvider` pointed at Ollama's OpenAI-compat
        // `/v1` endpoint. This is the built-in `cs tackle` default
        // (see `resolve_adapter_selection`), so it must be in the
        // dispatch registry even when no `[adapters.local]` row exists.
        BUILTIN_FLOOR_ADAPTER.to_owned(),
        // `task-20260707-7d27` (academy banc Mode C, hole #1): `ollama` is a
        // canonical alias of the `local` floor. It must be in the built-in
        // registry so `--adapter ollama` validates *without* requiring an
        // `[adapters.ollama]` TOML row — and, crucially, so it reaches the
        // floor dispatch arm below instead of the "validated but not wired"
        // catch-all it hit before this fix.
        "ollama".to_owned(),
    ];
    if let Some(adapters) = project_config.adapters.as_ref() {
        declared_names.extend(AdaptersConfig::available_names(adapters));
    }
    let (adapter, _supervision, loop_ownership_from_validator) =
        match validate_adapter_name(&adapter_name, &declared_names) {
            Ok(triple) => triple,
            Err(e) => return Err(anyhow::anyhow!("{e}")),
        };
    // ADR-103: per-Adapter `[adapters.<name>] ownership = "cosmon"`
    // overrides the built-in default — the installation-perimeter
    // escape hatch for TOML-only adapters. Built-in names ignore the
    // TOML row (the validator owns the answer for them).
    let loop_ownership = resolve_loop_ownership(
        adapter.as_str(),
        loop_ownership_from_validator,
        project_config
            .adapters
            .as_ref()
            .and_then(|cfg| cfg.entry(adapter.as_str())),
    );
    // Best-effort: a write failure on `events.jsonl` must not block
    // dispatch (same discipline as the four WS-1..WS-5 helpers).
    emit_adapter_selected(
        &state_dir,
        &mol_id,
        adapter.as_str(),
        selection_source,
        args.role_hint.as_deref(),
        loop_ownership,
    );

    // 3a''. Resolve the per-molecule model pin (delib-20260704-b476 C1).
    //
    //     The model axis is the structural sibling of the adapter axis:
    //     `resolve_model_selection` walks the same six-level chain, scoped
    //     to the just-resolved `adapter` (a model id only has meaning inside
    //     its adapter, so the config tiers read
    //     `[adapters.<name>].default_model`). The result is an
    //     `Option<String>` — `None` when nothing pinned a model, in which
    //     case cosmon injects no `ANTHROPIC_MODEL` and the adapter's own
    //     default applies (von-neumann's floor, byte-identical to the
    //     pre-C1 no-pin path). The env tier honours `$COSMON_DEFAULT_MODEL`
    //     first and the legacy `$ANTHROPIC_MODEL` (the rpp-adapter carrier)
    //     second, so the existing rpp dispatch path keeps its model pin.
    //
    //     Strong is never inherited: a strong model is reachable only from
    //     `--model` or a formula-step pin (the safe-default guard that
    //     rejects a *strong* config/env default lands in C4). The
    //     `ModelSelectionSource` is carried forward for the typed
    //     `ModelSelected` event (C2), emitted just below.
    let env_model = env_default_model();
    let (mut preferred_model, mut model_source) = resolve_model_selection(
        args.model.as_deref(),
        formula_step_model,
        env_model.as_ref().map(|(v, k)| (v.as_str(), *k)),
        adapter.as_str(),
        project_config.adapters.as_ref(),
        &config_path,
        global_adapters.as_ref(),
        &global_cfg_path,
    );

    // 3a''-C4. Fail-closed strong-dispatch ceiling + safe-default guard
    //     (delib-20260704-b476 C4, carnot's safety property + kahneman's
    //     Ghost A/B). Runs *before* the C2 attribution event so the event
    //     records the *effective* (possibly downgraded) model, and *before*
    //     any worktree / tmux side effect so an abort is fail-fast.
    //
    //     Two guards, folded into `strong_gate`:
    //     - **Safe-default (Ghost A/B):** a *strong* model reached from a
    //       config/env *default* (not `--model` / formula-pin) is dropped to
    //       the safe floor — strong is only reachable from a positive
    //       per-molecule act. No `ModelCeilingHit` is minted (this is the
    //       structural guard, not a budget hit); the downgrade is loud on
    //       stderr and the C2 event now carries `source = default`.
    //     - **Ceiling (carnot):** the (K+1)th strong dispatch inside the
    //       rolling window fails closed — downgrade to the floor or abort,
    //       per `[model_budget] on_overflow`. The running count is a fold over
    //       `events.jsonl` (`load_strong_dispatch_records`), never a counter
    //       file. Absent `[model_budget]` (cap `None`) → no ceiling, honour
    //       the pin (opt-in per galaxy, byte-identical to today).
    //
    //     Non-strong pins (the common path) skip every branch: no log read,
    //     no counting, no guard.
    let current_strong_set = adapter_strong_set(
        project_config.adapters.as_ref(),
        global_adapters.as_ref(),
        adapter.as_str(),
    );
    let current_is_strong = preferred_model
        .as_deref()
        .is_some_and(|m| cosmon_core::model_budget::is_strong_model(&current_strong_set, m));
    if current_is_strong {
        let budget = &project_config.model_budget;
        let now = chrono::Utc::now();
        let window = chrono::Duration::hours(i64::from(budget.window_hours));
        // Fold the local log for the in-window strong count, but keep the
        // `unreadable → Unavailable` distinction so the ceiling fails CLOSED on
        // an unreadable log rather than treating it as a zero count.
        let history = match load_strong_dispatch_records(&state_dir) {
            Ok(records) => {
                let strong_count = cosmon_core::model_budget::count_strong_in_window(
                    &records,
                    now,
                    window,
                    |adapter_name, model| {
                        let set = adapter_strong_set(
                            project_config.adapters.as_ref(),
                            global_adapters.as_ref(),
                            adapter_name,
                        );
                        cosmon_core::model_budget::is_strong_model(&set, model)
                    },
                );
                cosmon_core::model_budget::LocalHistory::Counted(strong_count)
            }
            Err(()) => cosmon_core::model_budget::LocalHistory::Unavailable,
        };
        match cosmon_core::model_budget::strong_gate_with_history(
            true,
            &model_source,
            history,
            budget.strong_dispatch_cap,
            budget.on_overflow,
        ) {
            // Not reachable (`current_is_strong` is true), but total-match by
            // design so a future gate variant is a compile error, not a silent
            // fall-through.
            cosmon_core::model_budget::StrongGate::NotStrong
            | cosmon_core::model_budget::StrongGate::AllowStrong => {}
            cosmon_core::model_budget::StrongGate::Downgrade { reason } => {
                let refused = preferred_model.take().unwrap_or_default();
                let fallback_reason = match reason {
                    cosmon_core::model_budget::DowngradeReason::NonPositiveSource => {
                        eprintln!(
                            "cs tackle: safe-default guard — '{refused}' is a strong \
                             model but was reached from a config/env default, not \
                             --model / a formula-pin; dropping to the adapter's own \
                             default (strong is reachable only from a positive \
                             per-molecule act)."
                        );
                        format!(
                            "safe-default guard: strong model '{refused}' reached from \
                             a non-positive source (config/env default); strong is \
                             reachable only from --model or a formula-step pin"
                        )
                    }
                    cosmon_core::model_budget::DowngradeReason::CeilingReached {
                        strong_count,
                        cap,
                    } => {
                        emit_model_ceiling_hit(
                            &state_dir,
                            &mol_id,
                            adapter.as_str(),
                            &refused,
                            strong_count,
                            cap,
                            budget.window_hours,
                            CeilingAction::Downgraded,
                        );
                        eprintln!(
                            "cs tackle: model budget ceiling — {strong_count} strong \
                             dispatch(es) in the last {}h ≥ cap {cap}; downgrading \
                             '{refused}' to the adapter's own default (on_overflow = \
                             downgrade).",
                            budget.window_hours
                        );
                        format!(
                            "model budget ceiling: {strong_count} strong dispatch(es) \
                             in the last {}h ≥ cap {cap}; strong pin '{refused}' \
                             downgraded to the floor",
                            budget.window_hours
                        )
                    }
                    cosmon_core::model_budget::DowngradeReason::HistoryUnavailable { cap } => {
                        eprintln!(
                            "cs tackle: model budget ceiling — local strong-dispatch \
                             history is unreadable, so the in-window count is unknown; \
                             failing closed and downgrading '{refused}' to the adapter's \
                             own default (cap {cap}, on_overflow = downgrade). Fix the \
                             events.jsonl permissions/corruption to route strong again."
                        );
                        format!(
                            "model budget ceiling: local strong-dispatch history \
                             unreadable (unknown count); strong pin '{refused}' \
                             downgraded to the floor (fail-closed, cap {cap})"
                        )
                    }
                };
                model_source = ModelSelectionSource::Default { fallback_reason };
            }
            cosmon_core::model_budget::StrongGate::AbortHistoryUnavailable { cap } => {
                let refused = preferred_model.as_deref().unwrap_or_default().to_owned();
                return Err(anyhow::anyhow!(
                    "model budget ceiling: local strong-dispatch history is unreadable, \
                     so the in-window count is unknown; refusing to spawn strong model \
                     '{refused}' (cap {cap}, on_overflow = abort). Failing closed rather \
                     than route on an unknown count — fix events.jsonl \
                     permissions/corruption, or lower the pin.",
                ));
            }
            cosmon_core::model_budget::StrongGate::Abort { strong_count, cap } => {
                let refused = preferred_model.as_deref().unwrap_or_default().to_owned();
                emit_model_ceiling_hit(
                    &state_dir,
                    &mol_id,
                    adapter.as_str(),
                    &refused,
                    strong_count,
                    cap,
                    budget.window_hours,
                    CeilingAction::Aborted,
                );
                return Err(anyhow::anyhow!(
                    "model budget ceiling reached: {strong_count} strong dispatch(es) \
                     in the last {}h ≥ cap {cap}; refusing to spawn strong model \
                     '{refused}' (on_overflow = abort). Downgrade the pin, widen \
                     [model_budget].strong_dispatch_cap, or wait for the window to roll.",
                    budget.window_hours
                ));
            }
        }
    }

    // 3a'''. Attribute the model choice as a typed event (delib-20260704-b476
    //     C2), the model sibling of `AdapterSelected`. Co-minted with the
    //     spawn and *before* the availability probe, so the attribution is
    //     ex-ante and deterministic: the source is the resolution-chain origin
    //     (flag / formula-pin / env / config / floor), and `model` is the
    //     pinned id (`None` at the floor). This promotes the old
    //     `model-selection.json` sidecar onto the wire so the ceiling guard
    //     (C4) can fold strong-dispatch counts over `events.jsonl` and
    //     `cs ensemble` / `cs observe` can surface model + source. Best-effort:
    //     a write failure on `events.jsonl` must not block dispatch (same
    //     trace-not-lock discipline as `emit_adapter_selected`).
    emit_model_selected(
        &state_dir,
        &mol_id,
        adapter.as_str(),
        preferred_model.as_deref(),
        model_source,
    );

    // 3a''. Adapter preflight (task-20260719-f45b). Prove the resolved
    //       adapter can actually *serve* the resolved model before the
    //       molecule is committed to it.
    //
    //       Evidence this exists: on 2026-07-19 two molecules
    //       (task-20260719-059b, task-20260719-e02c) were dispatched to
    //       `--adapter local` against an Ollama that was running but had
    //       nothing pulled. Each worker died ~30 s after `worker_spawned`
    //       and the patrol auto-collapsed the molecule. Collapse is
    //       terminal, so the work was lost and had to be re-nucleated by
    //       hand under a new id.
    //
    //       Note what is deliberately NOT guarded here: `preferred_model
    //       == None`. The model chain's floor is documented and tested to
    //       be `None` meaning "let the adapter use its own default"
    //       (`model_chain_floor_is_none_not_a_strong_constant`,
    //       `model_floor_none_is_honest`). Refusing on `None` would
    //       reject every healthy bare `--adapter local` dispatch while
    //       still missing the real fault — an explicit `--model` naming
    //       something unpulled dies identically. The serveable-model
    //       check catches both; the None-check catches neither.
    //
    //       Placed here on purpose: before the worktree lands (step 4) and
    //       before the status flips to Running, so a refusal leaves the
    //       molecule pending and re-tacklable with zero cleanup.
    //       Gated on exactly the adapter names the `local` spawn arm
    //       handles (`BUILTIN_FLOOR_ADAPTER | "ollama"`) — deliberately
    //       *not* `egress::adapter_is_local`, which also matches
    //       `llama-cpp` / `llama`. Those take a different spawn arm with
    //       their own resolution, so preflighting them with the Ollama
    //       resolvers would check an endpoint they never dial.
    if matches!(adapter.as_str(), BUILTIN_FLOOR_ADAPTER | "ollama")
        && std::env::var(SKIP_PREFLIGHT_ENV).ok().as_deref() != Some("1")
    {
        let adapter_entry = project_config
            .adapters
            .as_ref()
            .and_then(|cfg| cfg.entry(adapter.as_str()));
        let base_url = resolve_local_base_url(adapter_entry);
        let effective_model = resolve_local_model(preferred_model.as_deref(), adapter_entry);
        if let Err(e) = preflight_local_adapter_model(
            &base_url,
            &effective_model,
            std::time::Duration::from_secs(PREFLIGHT_TIMEOUT_SECS),
        ) {
            return Err(anyhow::anyhow!(
                "{e}\n\n(bypass with {SKIP_PREFLIGHT_ENV}=1 if you intend to \
                 dispatch anyway)"
            ));
        }
    }

    // 3a'. Q5b fail-policy gate (task-20260530-c089). Parse and validate the
    //      `--fallback-from-local <cause>` opt-in *before* any worktree / tmux
    //      side effect lands. Two refusals, both fail-fast:
    //
    //      - a blank cause (`--fallback-from-local ""`) is rejected — the
    //        whole point of the loud line is a named, decidable cause.
    //      - a fallback onto a *local* adapter is a contradiction: you cannot
    //        "fall back to a remote oracle" while still pointing at the local
    //        default. The operator must name the remote adapter they are
    //        escalating to (`--adapter claude --fallback-from-local timeout`).
    //
    //      The parsed cause is carried to the egress-grant atom below; the
    //      `LocalFallback` line is minted there, in the same block as
    //      `RemoteEgressOptIn`, so a fallback can never be silent.
    let fallback_cause: Option<cosmon_core::egress::LocalFailureCause> =
        match args.fallback_from_local.as_deref() {
            None => None,
            Some(raw) => {
                let cause =
                    cosmon_core::egress::LocalFailureCause::parse_token(raw).ok_or_else(|| {
                        anyhow::anyhow!(
                            "--fallback-from-local requires a non-empty cause \
                             (crash / oom / timeout / connection-refused / …)"
                        )
                    })?;
                if cosmon_core::egress::adapter_is_local(adapter.as_str()) {
                    return Err(anyhow::anyhow!(
                        "--fallback-from-local names an escalation to a REMOTE oracle, \
                         but the resolved adapter '{}' is local — pass a remote \
                         --adapter (e.g. --adapter claude --fallback-from-local {})",
                        adapter.as_str(),
                        cause.token()
                    ));
                }
                Some(cause)
            }
        };

    // 3b. Load FleetSpec — from fleet.toml if present, else default singleton.
    //     This ensures the dispatch path always has a FleetSpec in hand (ADR-040).
    let _fleet_spec = load_fleet_spec(&state_dir);

    // 4. Read briefing.md if it exists; auto-inject from fleet template if absent.
    let mol_dir = store.molecule_dir(&mol_id);
    let briefing_path = mol_dir.join("briefing.md");
    let briefing = match fs::read_to_string(&briefing_path).ok() {
        Some(text) => Some(text),
        None => try_inject_fleet_briefing(&state_dir, &mol, &briefing_path),
    };

    // 5. Build bootstrap prompt.
    let prompt = build_prompt(
        &mol,
        formula.as_ref(),
        briefing.as_deref(),
        &project_config,
        &mol_dir,
    );

    // 6. Dry-run: just print the prompt.
    if args.dry_run {
        if ctx.json {
            let out = serde_json::json!({
                "command": "tackle",
                "molecule_id": mol_id.as_str(),
                "prompt": prompt,
                "dry_run": true,
            });
            println!("{out}");
        } else {
            println!("{prompt}");
        }
        return Ok(());
    }

    // 6b. Preflight runtime prerequisites (W3 / delib-20260610-d108).
    //     Before the first side effect (worktree → tmux → fleet write),
    //     verify the tools this dispatch needs are on PATH: git for the
    //     worktree, and tmux + the adapter CLI for tmux-backed adapters.
    //     On a stranger's machine missing any of them, `cs tackle` used to
    //     die in an opaque `SpawnFailed("tmux new-session failed: …")` or a
    //     dead `[exited]` carcass pane. The preflight turns that into one
    //     actionable line per missing tool — what is missing and how to get
    //     it — and aborts cleanly before anything is created. `--dry-run`
    //     returned above, so prompt inspection never trips this gate.
    let needs_git = !args.no_worktree;
    super::preflight::check(&adapter, needs_git)?;
    super::preflight::check_configured_toolchain(&project_config.gates)?;

    // 7. Create git worktree for isolation.
    //    DAG-aligned branching: if this molecule has a BlockedBy dependency
    //    whose branch exists, branch from it instead of HEAD. This way the
    //    worktree sees the predecessor's output without a merge into main.
    //    The git DAG mirrors the cosmon DAG.
    //
    //    Write-discipline guard (ADR-110 Phase 1 Commit 1, invariant
    //    I2 ISOLATION + I1 WRITER-UNIQUE):
    //
    //    `--no-worktree` parks the worker on the *main checkout* — directly
    //    contradicting the rule that every worker writes inside a disjoint
    //    `.worktrees/<mol>/`. Drain v1.4 and v1.5 cassures (2026-05-22/23)
    //    were both rooted in workers mutating the shared main checkout while
    //    a concurrent `cs done` was mid-merge. The structural fix is to
    //    refuse `--no-worktree` by default and require an explicit operator
    //    gesture (env var) for the legitimate exceptions (existing tests,
    //    one-off operator scripts that genuinely need to act on main).
    //
    //    Escape hatch: `COSMON_ALLOW_NO_WORKTREE=1`. Out-of-band on purpose
    //    — discoverable from the error message, not a documented flag, so
    //    workers cannot opportunistically opt out. The `--dry-run` path above
    //    returns before this guard, so dry-run fixtures keep working.
    if args.no_worktree && std::env::var("COSMON_ALLOW_NO_WORKTREE").as_deref() != Ok("1") {
        return Err(anyhow::anyhow!(
            "`--no-worktree` would tackle {mol_id} on the main checkout, \
             violating the worker isolation discipline (ADR-110 / I2). \
             Re-run without `--no-worktree` so a dedicated worktree under \
             `.worktrees/{mol_id}/` is created, or set \
             COSMON_ALLOW_NO_WORKTREE=1 in the environment if you really \
             intend to write on main (tests + one-off operator scripts only)."
        ));
    }
    // C1-F3 egress preflight (task-20260712-8d2d). Resolve the egress posture
    // *before* creating any worktree so a `deny-external` dispatch that cannot
    // be kernel-enforced on this host is decided cleanly, with no orphaned
    // worktree to unwind. On a hardened Linux kernel (unprivileged user
    // namespaces disabled) or any non-Linux host the netns jail cannot be
    // built; without this preflight the strict-local worker used to ship broken
    // — every `exec_command` died opaquely with "shell died during init". The
    // operator owns the policy: default is degrade-to-advisory (identical to a
    // macOS dev host, caught by the same cutover gate); COSMON_EGRESS_REQUIRE_NETNS=1
    // refuses instead. `Refused` is handled here (pre-worktree, clean);
    // `DegradedAdvisory` is stamped as a loud audit line just before spawn.
    //
    // RÉSIDUEL SÉCU B (task-20260713-d436). An **exposed / multi-tenant**
    // launch — a worker spawned through the RPP API rather than by a local
    // operator, marked by `COSMON_API_REQUEST=1` — MUST fail closed. A
    // strict-local (`deny-external`) policy that cannot be kernel-enforced on
    // this host has to be *refused*, never silently degraded to an unconfined
    // passthrough shell: on a hosted, multi-tenant box an unjailed local model
    // could reach the network and exfiltrate a neighbour's state. The gap the
    // re-review (task-20260713-c5ad §FIX B) named was exactly this — `cs
    // tackle` set `COSMON_EGRESS_POLICY` but never the enforcement marker, so
    // on macOS / any non-Linux host the exposed strict worker took
    // `DegradedAdvisory` (advisory, unjailed). We close it by *projecting* the
    // exposed posture onto the enforcement requirement here, which turns the
    // preflight's `DegradedAdvisory` into a `Refused` for the multi-tenant
    // case, without touching the trusted single-operator default (a remote
    // `AllowAll` worker still preflights `Ready` — `preflight` short-circuits
    // before the requirement is consulted).
    let launch_exposed = egress_launch_is_exposed(|k| std::env::var(k).ok());
    let egress_require_netns =
        cosmon_agent_harness::egress_probe::require_netns_from_env() || launch_exposed;
    // Propagate the marker into this process's environment so an in-process
    // `exec_command` (direct-API adapters run the agent loop in-process) reads
    // the same fail-closed posture the tackle-side preflight enforces — the two
    // gates must never disagree. Mirrors the `COSMON_EGRESS_POLICY` set_var a
    // few lines below; for a tmux-backed worker the frozen server env drops it
    // (harmless — those are remote opt-ins, not strict-local workers).
    if launch_exposed {
        std::env::set_var(cosmon_core::egress::REQUIRE_NETNS_ENV, "1");
    }
    let egress_netns_available = cosmon_agent_harness::egress_probe::netns_available();
    // Exposed multi-tenant axis (task-20260713-8acc): a hosted RPP dispatch on
    // a host that cannot kernel-enforce `deny-external` is refused fail-closed,
    // regardless of the operator's require-netns knob. On a macOS host this is
    // the honest interim guard until native seatbelt / Network-Extension
    // enforcement lands (ADR-155); a tenant's strict-local worker must never
    // run advisory-only egress on an exposed endpoint (§8u).
    let egress_exposed = cosmon_agent_harness::egress_probe::exposed_multitenant_from_env();
    {
        let preflight_base_url = project_config
            .adapters
            .as_ref()
            .and_then(|cfg| cfg.entry(adapter.as_str()))
            .and_then(|entry| entry.base_url.as_deref());
        let preflight_policy = cosmon_core::egress::AutonomyPosture::for_adapter_with_base_url(
            adapter.as_str(),
            preflight_base_url,
        )
        .policy();
        if let cosmon_core::egress::EgressPreflight::Refused { message } =
            cosmon_core::egress::EgressJail::preflight(
                preflight_policy,
                egress_netns_available,
                egress_require_netns,
                egress_exposed,
            )
        {
            return Err(anyhow::anyhow!("{message}"));
        }
    }

    let repo_root = find_repo_root()?;
    let branch_name = format!("feat/{mol_id}");
    let start_point = resolve_branch_start_point(&repo_root, &mol);
    let worktree_path = if args.no_worktree {
        args.workdir
            .as_deref()
            .map_or_else(|| repo_root.clone(), PathBuf::from)
    } else {
        let wt_dir = args.workdir.as_deref().map_or_else(
            || repo_root.join(".worktrees").join(mol_id.as_str()),
            PathBuf::from,
        );
        create_worktree(&repo_root, &wt_dir, &branch_name, start_point.as_deref())?;
        wt_dir
    };

    // 7b. Install SessionStart hook in worktree for propulsion re-injection.
    // install_session_hook(&worktree_path, mol_id.as_str());

    // 8. Spawn tmux session with Claude Code.
    //    Session name is the functional slug ({slug}-{shortid}) so
    //    `tmux ls` is visually scannable. Falls back to the raw
    //    molecule ID when no topic is available, and `--name` lets the
    //    caller override the whole thing. The branch and worktree paths
    //    still use `mol_id` — those are stable git/fs refs and any
    //    in-flight worker would break if they were renamed.
    let socket = super::tmux_socket_name(ctx);
    let session_name = if let Some(ref raw) = args.name {
        sanitize_session_name(raw)?
    } else {
        cosmon_core::slugify::session_name_for(mol.display_topic(), mol_id.as_str())
    };
    let backend = TmuxBackend::new(&socket);
    let wid = cosmon_core::id::WorkerId::new(&session_name)?;

    // Idempotency: if a session already exists, handle it.
    let already_running = backend.is_alive(&wid).unwrap_or(false);
    if already_running && !args.force {
        report_existing_session(
            ctx,
            &mol,
            &session_name,
            &socket,
            &worktree_path,
            &branch_name,
        );
        return Ok(());
    }
    if already_running {
        let _ = backend.terminate(&wid);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    // Compute molecule state directory so the worker can access it without
    // calling `cs observe`. Injected as COSMON_MOL_DIR env var.
    let mol_state_dir = store.molecule_dir(&mol_id);

    // Autonomy guard (task-20260530-d8bc / delib-20260530-0877). Resolve the
    // egress posture from the selected adapter and act *before* spawning:
    //
    // - A **local** adapter (llama-cpp / ollama) runs strict: we set
    //   `COSMON_EGRESS_POLICY=deny-external` in this process's environment so
    //   the in-process `exec_command` tool spawns its shell inside an
    //   egress-denied network namespace (on a capable kernel). A weak local
    //   model that emits `exec_command { "claude -p '…'" }` then hits a
    //   *refused syscall*, not a *detected anomaly* — the routing hole turing
    //   named is closed by construction, not by a string scan.
    //
    // - A **remote** adapter (claude / openai / anthropic / aider) is a
    //   conscious operator opt-in. We stamp the `RemoteEgressOptIn` atom into
    //   `events.jsonl` *here, before spawn*, so the egress grant and the audit
    //   record are minted together and can never diverge. There is no window
    //   in which a worker reaches the network without a matching line on the
    //   wire — and the cutover audit can trust that every worker WITHOUT such
    //   a line ran strict-local.
    //
    // The env var is set for every posture (it is a no-op `allow-all` for the
    // remote case) so the in-process enforcement path is uniform. For the
    // tmux-backed claude worker the var does not propagate (the tmux server
    // froze its env at startup — see CLAUDE.md §multi-account), which is
    // harmless: claude is a remote opt-in, not a strict-local worker.
    // The endpoint stamped on the `RemoteEgressOptIn` atom must follow the
    // adapter's *configured* `base_url`, not just its name. The `openai`
    // adapter is OpenAI-compatible and routinely repointed via
    // `[adapters.openai].base_url` — to xAI, Moonshot, and now Mistral
    // (`api.mistral.ai`, the EU-sovereign warm standby of
    // delib-20260614-61f9). Reading the config base_url here keeps the audit
    // honest about where egress was actually opened (buterin's named gap).
    let adapter_base_url = project_config
        .adapters
        .as_ref()
        .and_then(|cfg| cfg.entry(adapter.as_str()))
        .and_then(|entry| entry.base_url.as_deref());
    let posture = cosmon_core::egress::AutonomyPosture::for_adapter_with_base_url(
        adapter.as_str(),
        adapter_base_url,
    );
    let egress_policy = posture.policy();
    std::env::set_var(
        cosmon_core::egress::EgressPolicy::ENV_VAR,
        egress_policy.token(),
    );
    // C1-F3 (task-20260712-8d2d): if `deny-external` cannot be kernel-enforced
    // on this host, the worker degrades to advisory mode. Make the degradation
    // loud — a warning on stderr and a durable `EgressUnenforceable` audit line
    // minted before spawn — so the cutover gate refuses to flip the
    // hosted-tenant default while any spawn carries it. (`Refused` was already
    // handled and returned before the worktree was created; `Ready` is silent.)
    if let cosmon_core::egress::EgressPreflight::DegradedAdvisory { reason } =
        cosmon_core::egress::EgressJail::preflight(
            egress_policy,
            egress_netns_available,
            egress_require_netns,
            egress_exposed,
        )
    {
        eprintln!("cs tackle: egress degraded to advisory — {reason}");
        cosmon_state::events::autonomy::emit_egress_unenforceable(
            &state_dir,
            &mol_id,
            adapter.as_str(),
            &reason,
        );
    }
    if let cosmon_core::egress::AutonomyPosture::RemoteOptIn { ref endpoint } = posture {
        cosmon_state::events::autonomy::emit_remote_egress_opt_in(
            &state_dir,
            &mol_id,
            adapter.as_str(),
            endpoint.as_ref(),
        );
        // Q5b atom (task-20260530-c089): when this remote opt-in is a
        // conscious fallback from a local hard-failure, the `LocalFallback`
        // line is minted *here*, in the same block as the egress grant.
        // There is no other code path that emits it — so a remote call
        // carrying a fallback cause can never reach the wire without this
        // matching loud audit record. Silent fallback is impossible by
        // construction (turing's Q5b verdict). The `from_adapter` is the
        // local default that did not hold; we record the floor's canonical
        // name [`BUILTIN_FLOOR_ADAPTER`] as the `from_adapter` (the operator
        // escalated *away* from it, regardless of which concrete local
        // adapter was configured).
        if let Some(ref cause) = fallback_cause {
            cosmon_state::events::autonomy::emit_local_fallback(
                &state_dir,
                &mol_id,
                BUILTIN_FLOOR_ADAPTER,
                adapter.as_str(),
                cause,
            );
        }
    }

    // Failure of spawn_and_prompt used to propagate via `?` straight to the
    // caller, leaving the branch and worktree we just created orphaned on
    // disk. A re-invocation would then see "branch already exists" and
    // either reuse a stale worktree or confuse the operator. `cs tackle`'s
    // symmetry contract is: either everything commits (Running molecule +
    // worker + branch + worktree) or nothing persists. On spawn failure
    // we now undo `create_worktree`'s side effects before surfacing the
    // error.
    if let Err(e) = spawn_and_prompt(
        &backend,
        &wid,
        &session_name,
        &worktree_path,
        &prompt,
        args.permission_mode.as_deref(),
        &mol,
        &mol_state_dir,
        &state_dir,
        &adapter,
        project_config.adapters.as_ref(),
        preferred_model.as_deref(),
        &current_strong_set,
    ) {
        cleanup_partial_tackle(
            &backend,
            &wid,
            &repo_root,
            &worktree_path,
            &branch_name,
            args.no_worktree,
        );
        return Err(e);
    }

    // Two post-spawn steps below presuppose a tmux-backed worker —
    // install_harvest_hook (kernel-level pane-died witness) and the
    // liveness re-check (catches a tmux worker that died between
    // `spawn_and_prompt` returning Ok and the fleet write). Direct-API
    // adapters (openai, anthropic, ADR-100 R2) run the agent loop
    // in-process and never create a tmux session — `backend.is_alive`
    // against the `*-inprocess` sentinel socket would always report
    // Dead and trigger a spurious tear-down of the worktree, eating
    // the synthesis the agent loop just wrote. Gating on
    // [`adapter_uses_tmux`] keeps the supervision invariant for tmux
    // adapters AND honours the in-process completion for Direct-API.
    //
    // This is the tactical GAP #1 fix from the academy smoke chronicle
    // `2026-05-18-grok-direct-api-smoke-result.md`. The longer-term
    // move (cosmon-ward GAP #3) promotes the verdict to a typed
    // `SupervisionMode` on [`ValidatedAdapterName`] so each branch of
    // the post-spawn pipeline is forced by the compiler to handle both
    // modes. See chronicle `2026-05-18-supervision-mode-tactical-gap1.md`
    // for the pattern divergence and the dette restante.
    if adapter_uses_tmux(&adapter) {
        // Arm the worker-exit → `cs done` bridge. The hook fires
        // whenever the session's pane dies (worker exits cleanly,
        // crashes, or is killed) and exec's `cs harvest` from the main
        // repo — outside the worktree — so the `cs done = not-the-worker`
        // invariant holds by construction. Idempotent on harvest side.
        //
        // ADR-052 child #4 (as amended by GAP #2 / SF-6, chronicle
        // `2026-05-18-cleanup-preserve-on-success.md`): the hook is the
        // strongly-preferred witness, but its installation failure no
        // longer wipes the worktree. The agent's spawn already
        // succeeded by the time we reach this block (spawn_and_prompt
        // returned Ok — the HTTP call completed, the agent loop wrote
        // an artefact, the tmux session is alive). Tearing the
        // worktree down here destroys real work as collateral damage
        // for a *supervision* failure that came after the work landed.
        //
        // The new contract: emit a forensic `SF6SupervisionSetupFailed`
        // event recording exactly which hook failed under which
        // adapter, log a loud warning to stderr, and *continue* — the
        // molecule will be registered as Running, the worktree and
        // branch survive, and the operator can either inspect the
        // worker's output by hand or rely on the periodic
        // `cs patrol --harvest` polling sweep as fallback supervision.
        // L9-aligned: work performed must be comptabilised; the SF-6
        // emission is the structural counter-measure that prevents the
        // operator from mistaking an unsupervised molecule for a
        // normally-supervised one.
        if let Err(e) = install_harvest_hook(&backend, &session_name, &mol_id, &repo_root) {
            emit_supervision_setup_failed_event(
                &mol_id,
                &wid,
                adapter.as_str(),
                "pane_died",
                &e.to_string(),
            );
            eprintln!(
                "cs tackle: warning — failed to install pane-died hook on \
                 {session_name}: {e}. Worker spawned and worktree preserved; \
                 supervision is missing (SF-6 event emitted). Worker exits \
                 will be detected by the periodic `cs patrol --harvest` \
                 sweep rather than the event-driven hook."
            );
        }

        // Final liveness re-check: a tight race still exists between
        // `spawn_and_prompt` returning Ok and us taking the fleet lock
        // — the worker process might receive SIGSEGV / be kill -9'd /
        // crash on a second-turn input in those few milliseconds. If
        // that happened, writing `molecule.status = Running` + a
        // `WorkerData` entry would restate the surface lie. So we
        // re-observe just before committing, and if the session has
        // died in the meantime we tear down the partial state and
        // return an error WITHOUT touching the molecule or the fleet.
        let still_alive = backend.is_alive(&wid).unwrap_or(false);
        let status = if still_alive {
            cosmon_transport::readiness::detect_status(&backend, &wid)
                .unwrap_or(cosmon_transport::readiness::SessionStatus::Unknown)
        } else {
            cosmon_transport::readiness::SessionStatus::Dead
        };
        if !still_alive || status == cosmon_transport::readiness::SessionStatus::Dead {
            cleanup_partial_tackle(
                &backend,
                &wid,
                &repo_root,
                &worktree_path,
                &branch_name,
                args.no_worktree,
            );
            return Err(anyhow::anyhow!(
                "cs tackle: session {session_name} died between spawn and \
                 fleet-write (status={status}); no Running state written, \
                 partial tmux/branch/worktree cleaned up"
            ));
        }
    }

    // 9. Update molecule status to Running, bind worker, and register in fleet.
    //    Hold the fleet lock for molecule save + fleet registration so
    //    concurrent tackles don't clobber fleet.json.
    let updated = {
        let _g = store.lock_fleet()?;
        let mut updated = mol;
        if updated.status == MoleculeStatus::Pending || updated.status == MoleculeStatus::Queued {
            updated.status = MoleculeStatus::Running;
        }
        // Bind the inline live-process record (delib-20260426-1bcd #1
        // fold-in). `bind_process` mirrors `assigned_worker` and
        // `session_name` for backwards compatibility with readers that
        // have not migrated yet. The validated adapter is stamped on
        // the record so observer-side commands (`cs ensemble`, `cs peek`)
        // can branch on the adapter's `SupervisionMode` without
        // re-running the dispatch logic — see GAP #7
        // (chronicle `2026-05-18-gap7-observer-side-fix.md`).
        let process = cosmon_core::process::MoleculeProcess::new(wid.clone(), session_name.clone())
            .with_adapter_name(adapter.as_str());
        updated.bind_process(process);
        // Record the dispatch claim (anti-preemption lease). A `human`
        // claim is sticky — the resident runtime will never preempt it;
        // a `runtime:<pid>` claim does not block re-dispatch.
        updated.mark_tackled(tackled_by.clone());
        store.save_molecule(&mol_id, &updated)?;

        // Register the tackle-created worker in the fleet so patrol and propel
        // can find it. Tackle workers are transient (tmux session ↔ worker),
        // but they deserve a proper WorkerData entry for the duration of the run.
        register_tackle_worker(
            &store,
            &wid,
            &worktree_path,
            &repo_root,
            &updated,
            &adapter,
            loop_ownership,
        )?;

        updated
    };

    // 9b'. Post-lock read-modify-write race detection (task-20260519-81d2).
    //
    // The `with_fleet_lock` block above held an exclusive flock for the
    // molecule save + worker registration, but every other in-tree
    // mutator that touches `state.json` (notably `cs tag`, `cs link`,
    // `cs decay`, `cs nucleate --blocks`) reads + modifies + writes
    // **without** taking that lock. A concurrent invocation that loaded
    // the molecule pre-flip can therefore stomp on our Running write
    // moments after we release the lock — the canonical read-modify-write
    // race. The empirically observed symptom (idea-20260518-52e9, 2026-05-18
    // 06:09): tmux session up, `worker_spawned` event durably on the wire,
    // fleet.json shows `desired = running`, but `state.json` still reads
    // `pending` with `total_steps = 0`; the worker spawns, writes its
    // capture artefact, and then `cs evolve` refuses because the molecule
    // never moved out of `pending`.
    //
    // We catch the divergence by reading the molecule back after the lock
    // and, if a concurrent writer has reverted our flip, rolling back the
    // partial spawn so the operator sees a hard error instead of a
    // stranded worker that the supervision layer cannot heal. Only the
    // tmux path is checked — the in-process branch (Direct-API adapters)
    // runs its agent loop synchronously inside `spawn_and_prompt` and its
    // race window is already closed by the time we land here, while the
    // imminent `finalize_inprocess_molecule` call overwrites `status` to
    // `Completed` anyway.
    //
    // Caveat: this check still happens **outside** the fleet lock, so a
    // racer that writes between our read-back and the next observer can
    // still produce the same symptom; the proper structural fix is to
    // make every read-modify-write writer take `with_fleet_lock` (TODO
    // bead: see task-20260519-81d2 chronicle). The check narrows the
    // window from `cs tackle`'s entire setup phase down to a single
    // file read, which is sufficient to surface the failure mode the
    // operator would otherwise discover only when `cs evolve` refuses
    // inside the worker.
    if adapter_uses_tmux(&adapter) {
        let observed = store.load_molecule(&mol_id).map_err(|e| {
            anyhow::anyhow!("cs tackle: post-lock read-back of {mol_id} failed: {e}")
        })?;
        if matches!(
            observed.status,
            MoleculeStatus::Pending | MoleculeStatus::Queued
        ) {
            // ADR-097 / WS-1'' (delib-20260519-e6db W3 / adversary
            // F4.1) — emit the terminal partner of WorkerSpawnAttempted
            // *before* the WorkerId is removed from the fleet, so the
            // telemetry context (mol_id, worker_id, adapter_name) is
            // still on the wire. The TLA+ invariant
            // I3 — no_rollback_without_terminal_event hinges on this
            // ordering: a rollback that races the WorkerId removal
            // leaves WS-1 ambiguous.
            emit_worker_spawn_rolled_back(
                &state_dir,
                &mol_id,
                &wid,
                adapter.as_str(),
                &observed.status.to_string(),
            );

            // Roll back the worker registration (best-effort: another
            // writer may have already mutated fleet.json), then tear
            // down tmux + worktree + branch via the same helper that
            // pre-lock spawn failures use.
            // Best-effort rollback: swallow lock / save errors (ADR-131
            // Decision 2 — lexical guard, errors intentionally discarded as
            // the original `let _ = with_fleet_lock(…)` did).
            if let Ok(_g) = store.lock_fleet() {
                let mut fleet = store.load_fleet().unwrap_or_default();
                fleet.workers.remove(&wid);
                let _ = store.save_fleet(&fleet);
            }
            cleanup_partial_tackle(
                &backend,
                &wid,
                &repo_root,
                &worktree_path,
                &branch_name,
                args.no_worktree,
            );
            return Err(anyhow::anyhow!(
                "cs tackle: molecule {mol_id} was flipped to Running inside the \
                 fleet lock but a concurrent non-locking writer (likely `cs tag`, \
                 `cs link`, or another read-modify-write path) reverted it to {} \
                 after the lock released. Worker dispatch rolled back: tmux \
                 session terminated, worktree and branch removed, fleet entry \
                 cleared. Retry `cs tackle {mol_id}` once the conflicting writer \
                 has settled.",
                observed.status,
            ));
        }
    }

    // 9b''. First-turn realized-model watcher (round-4 / COND-1, D4).
    //
    // D4 demands `ModelObserved` on the FIRST assistant turn carrying a
    // concrete model id — not "at the next `cs wait` poll, if anyone runs
    // one". Neither `cs wait` nor `cs run` is guaranteed to exist for this
    // dispatch, so the emission consumer is attached to the dispatch itself:
    // a detached `cs realized-watch` re-exec that ticks the idempotent
    // capture core against the worktree we just created. Pane-independent by
    // construction (session-log resolution by cwd), so a worker that crashes
    // right after its first turn still gets its observation post-mortem.
    // Session-log adapters only: in-process providers (openai/anthropic/…)
    // emit at their own response seam.
    if matches!(adapter.as_str(), "claude" | "codex") {
        spawn_realized_watcher(&state_dir, &mol_id, &worktree_path);
    }

    // 9c. In-process Direct-API completion emit — GAP #6 fix.
    //
    // Direct-API adapters (openai, anthropic) run the agent loop
    // *inside* `spawn_and_prompt`. By the time we reach this point the
    // synthesis has already been written and the in-process tokio
    // runtime has joined. Unlike tmux-backed adapters — whose
    // pane-died hook (`install_harvest_hook`) eventually exec's
    // `cs harvest` which in turn observes a Completed molecule and
    // fires `cs done` — there is no asynchronous witness for the
    // in-process branch. Without an explicit emit here, the molecule
    // sits forever in `Running`: `cs wait` times out (academy GAP #8),
    // `cs ensemble` paints the row as a dead pane (academy GAP #7),
    // and operators rightly conclude the pipeline is broken.
    //
    // The contract divergence: for tmux, the harvest hook owns the
    // completion-emit. For in-process, **`spawn_and_prompt` owns the
    // completion-emit** (driven from this call site). The canonical
    // sequence — Running→Completed transition, MoleculeStatusChanged
    // event, MoleculeCompleted event, log.md + briefing.md update,
    // pow seal — lives in `complete::complete_one`, so we call it
    // verbatim rather than re-implementing.
    //
    // L9 (work performed must be comptabilised): if the agent loop
    // ran and wrote a synthesis, the molecule MUST move to a
    // terminal state. Failing to do so loses real cognitive work
    // behind a stuck `Running` row.
    //
    // See chronicle `2026-05-18-gap6-inprocess-completion.md` and
    // smoke chronicle `2026-05-18-grok-direct-api-smoke-result-2.md`
    // §"Ce qui n'a pas marché" #2.
    if adapter_completes_inline(&adapter) {
        finalize_inprocess_molecule(&store, &state_dir, &mol_id, &adapter)?;
    }

    // 10. Output.
    //
    // For in-process Direct-API adapters the molecule has already been
    // flipped to Completed by step 9b. Reflect the post-spawn status
    // (re-read from the store) so the JSON envelope and the human
    // surface tell the same story `cs wait` will read a moment later.
    let final_status = if adapter_completes_inline(&adapter) {
        store
            .load_molecule(&mol_id)
            .map_or(updated.status, |m| m.status)
    } else {
        updated.status
    };

    if ctx.json {
        let out = serde_json::json!({
            "command": "tackle",
            "molecule_id": mol_id.as_str(),
            "status": final_status.to_string(),
            "tmux_session": session_name,
            "worktree": worktree_path.to_string_lossy(),
            "branch": branch_name,
            "attach": format!("tmux -L {socket} attach -t {session_name}"),
            "spawned_at": Utc::now().to_rfc3339(),
        });
        println!("{out}");
    } else {
        let kind_emoji = updated.kind.map_or("", |k| k.emoji());
        println!(
            "{kind_emoji} Tackling {session_name} ({})",
            updated.formula_id
        );
        println!("  molecule: {mol_id}");
        println!("  branch:   {branch_name}");
        println!("  worktree: {}", worktree_path.display());
        println!("  session:  {session_name}");
        println!("  attach:   tmux -L {socket} attach -t {session_name}");
        if final_status == MoleculeStatus::Completed {
            println!(
                "  status:   completed (in-process agent loop returned; run `cs done {mol_id}` to merge)"
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Gate execution — shell command steps (bypass TransportBackend)
// ---------------------------------------------------------------------------

/// Execute a shell gate step: run the command, capture output, advance or
/// collapse the molecule based on exit code.
///
/// Gate steps bypass `TransportBackend` entirely — no tmux, no worktree
/// session. The command runs as a child process of the current `cs tackle`
/// invocation. Output is captured to `MOLECULE_DIR/gate-output.log`.
fn execute_gate(
    ctx: &Context,
    store: &FileStore,
    mol: MoleculeData,
    formula: &Formula,
    step: &cosmon_core::formula::Step,
) -> anyhow::Result<()> {
    let mol_id = mol.id.clone();
    let command = step.command.as_deref().unwrap_or("");
    let timeout_secs = step.gate_timeout_secs();
    let mol_dir = store.molecule_dir(&mol_id);
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");

    emit_gate_started(&events_path, &mol_id, &step.id, command);

    // Mark molecule as Running.
    let mut updated = mol;
    if updated.status == MoleculeStatus::Pending || updated.status == MoleculeStatus::Queued {
        updated.status = MoleculeStatus::Running;
        updated.updated_at = Utc::now();
        store.save_molecule(&mol_id, &updated)?;
    }

    let work_dir = find_repo_root()?;
    // Trust gate (B5, RCE-by-clone): a gate step's `command` is a
    // repo-supplied shell string. Refuse to run it against an untrusted
    // clone until the operator vouches for the repository (`cs trust`).
    cosmon_cli::trust::ensure_trusted(&work_dir)?;
    let start = std::time::Instant::now();
    let child_result = std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(&work_dir)
        .output();
    let duration = start.elapsed();
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let timed_out = duration.as_secs() > timeout_secs;

    match child_result {
        Ok(output) => {
            write_gate_log(&mol_dir, &mol_id, &step.id, command, &output, duration_ms);
            let exit_code = output.status.code().unwrap_or(-1);

            if output.status.success() && !timed_out {
                handle_gate_success(
                    ctx,
                    store,
                    &events_path,
                    &mol_id,
                    &updated,
                    formula,
                    step,
                    exit_code,
                    duration_ms,
                )?;
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                handle_gate_failure(
                    ctx,
                    store,
                    &events_path,
                    &mol_id,
                    &mut updated,
                    step,
                    exit_code,
                    &stderr,
                    timed_out,
                    &duration,
                    timeout_secs,
                )?;
            }
        }
        Err(e) => {
            let reason = format!("gate command failed to spawn: {e}");
            emit_gate_failed(&events_path, &mol_id, &step.id, -1, &reason);
            updated.status = MoleculeStatus::Collapsed;
            updated.updated_at = Utc::now();
            store.save_molecule(&mol_id, &updated)?;
            return Err(anyhow::anyhow!(reason));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Native execution — direct Rust function call (bypass TransportBackend)
// ---------------------------------------------------------------------------

/// Execute a native step: call the registered Rust function, capture output,
/// advance or collapse the molecule based on the `Result`.
///
/// Native steps share the gate step's contract — no tmux, no worktree,
/// `gate-output.log` written, same `advance_gate_step` on success, same
/// collapse on failure. They differ only in the executor: a direct call
/// into an in-process `fn` looked up by key, instead of `sh -c`.
#[allow(clippy::too_many_lines)]
fn execute_native(
    ctx: &Context,
    store: &FileStore,
    mol: MoleculeData,
    formula: &Formula,
    step: &cosmon_core::formula::Step,
) -> anyhow::Result<()> {
    use crate::native;

    let mol_id = mol.id.clone();
    let native_key = step.native.as_deref().unwrap_or("");
    let mol_dir = store.molecule_dir(&mol_id);
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");

    let _ = cosmon_state::event_log::emit_one(
        &events_path,
        cosmon_core::event_v2::EventV2::NativeStarted {
            molecule_id: mol_id.clone(),
            step_id: step.id.clone(),
            native_fn: native_key.to_owned(),
        },
        None,
    );

    let Some(func) = native::lookup(native_key) else {
        let reason = format!("native function not registered: {native_key}");
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::NativeFailed {
                molecule_id: mol_id.clone(),
                step_id: step.id.clone(),
                error: reason.clone(),
            },
            None,
        );
        let mut updated = mol;
        updated.status = MoleculeStatus::Collapsed;
        updated.updated_at = Utc::now();
        store.save_molecule(&mol_id, &updated)?;
        return Err(anyhow::anyhow!(reason));
    };

    // Mark molecule as Running.
    let mut updated = mol;
    if updated.status == MoleculeStatus::Pending || updated.status == MoleculeStatus::Queued {
        updated.status = MoleculeStatus::Running;
        updated.updated_at = Utc::now();
        store.save_molecule(&mol_id, &updated)?;
    }

    let ctx_native = native::NativeCtx {
        mol_dir: mol_dir.clone(),
        step_id: step.id.clone(),
        work_dir: find_repo_root()?,
    };

    let start = std::time::Instant::now();
    let result = func(&ctx_native);
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    match result {
        Ok(out) => {
            native::write_log(&mol_dir, &step.id, native_key, &out, duration_ms);
            let _ = cosmon_state::event_log::emit_one(
                &events_path,
                cosmon_core::event_v2::EventV2::NativeCompleted {
                    molecule_id: mol_id.clone(),
                    step_id: step.id.clone(),
                    duration_ms,
                },
                None,
            );
            let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
            let formula_path = formulas_dir.join(format!("{}.formula.toml", updated.formula_id));
            advance_gate_step(ctx, store, &mol_id, &updated, formula, &formula_path)?;
            if ctx.json {
                let out_json = serde_json::json!({
                    "command": "tackle",
                    "molecule_id": mol_id.as_str(),
                    "mode": "native",
                    "step": step.id,
                    "native_fn": native_key,
                    "duration_ms": duration_ms,
                    "result": "completed",
                });
                println!("{out_json}");
            } else {
                println!(
                    "⚡ Native step passed: {mol_id} (step: {}, fn: {native_key}, {duration_ms}ms)",
                    step.id,
                );
            }
            Ok(())
        }
        Err(e) => {
            let err_msg = e.to_string();
            let fake_out = native::NativeOutput {
                log: format!("FAILED: {err_msg}"),
            };
            native::write_log(&mol_dir, &step.id, native_key, &fake_out, duration_ms);
            let tail = if err_msg.len() > 500 {
                err_msg[err_msg.len() - 500..].to_owned()
            } else {
                err_msg.clone()
            };
            let _ = cosmon_state::event_log::emit_one(
                &events_path,
                cosmon_core::event_v2::EventV2::NativeFailed {
                    molecule_id: mol_id.clone(),
                    step_id: step.id.clone(),
                    error: tail.clone(),
                },
                None,
            );
            updated.status = MoleculeStatus::Collapsed;
            updated.updated_at = Utc::now();
            store.save_molecule(&mol_id, &updated)?;
            if ctx.json {
                let out_json = serde_json::json!({
                    "command": "tackle",
                    "molecule_id": mol_id.as_str(),
                    "mode": "native",
                    "step": step.id,
                    "native_fn": native_key,
                    "duration_ms": duration_ms,
                    "result": "collapsed",
                    "error": tail,
                });
                println!("{out_json}");
            } else {
                eprintln!(
                    "💥 Native step failed: {mol_id} (step: {}, fn: {native_key}): {tail}",
                    step.id,
                );
            }
            Err(anyhow::anyhow!("native step failed: {err_msg}"))
        }
    }
}

// ---------------------------------------------------------------------------
// Query execution — typed query over molecule state / events / artifacts
// ---------------------------------------------------------------------------

/// Execute a `[steps.query]` step: resolve the source, evaluate the
/// dot-path expression, bind the result into the molecule's `variables`
/// map, emit a `QueryStepEvaluated` event, and advance the molecule.
///
/// Replaces shell-outs of the form `cs --json observe ${id} | jq …`. The
/// failure surface is now a typed event a watchdog can consume, not a
/// silent pipe-failure.
fn execute_query(
    ctx: &Context,
    store: &FileStore,
    mol: MoleculeData,
    formula: &Formula,
    step: &cosmon_core::formula::Step,
) -> anyhow::Result<()> {
    let mol_id = mol.id.clone();
    let mol_dir = store.molecule_dir(&mol_id);
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");

    let Some(query) = step.query.as_ref() else {
        anyhow::bail!("execute_query called on non-query step (internal bug)");
    };

    // Mark molecule as Running.
    let mut updated = mol;
    if updated.status == MoleculeStatus::Pending || updated.status == MoleculeStatus::Queued {
        updated.status = MoleculeStatus::Running;
        updated.updated_at = Utc::now();
        store.save_molecule(&mol_id, &updated)?;
    }

    // Resolve source.
    let (source_label, doc) = match resolve_query_source(store, &mol_id, &mol_dir, &query.source) {
        Ok(v) => v,
        Err(e) => {
            let reason = format!("query source resolution failed: {e}");
            updated.status = MoleculeStatus::Collapsed;
            updated.collapse_reason = Some(reason.clone());
            updated.updated_at = Utc::now();
            store.save_molecule(&mol_id, &updated)?;
            return Err(anyhow::anyhow!(reason));
        }
    };

    // Evaluate the expression.
    let resolved = match crate::dotpath::evaluate(&query.expr, &doc) {
        Ok(v) => v.clone(),
        Err(e) => {
            let reason = format!("query evaluation failed: {e}");
            updated.status = MoleculeStatus::Collapsed;
            updated.collapse_reason = Some(reason.clone());
            updated.updated_at = Utc::now();
            store.save_molecule(&mol_id, &updated)?;
            return Err(anyhow::anyhow!(reason));
        }
    };

    // Serialise the result back into the molecule's variables map. Strings
    // are stored verbatim (without the JSON quotes); other shapes are
    // JSON-encoded so the operator can read them back without ambiguity.
    let serialised = match &resolved {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    };

    updated
        .variables
        .insert(query.output_var.clone(), serialised.clone());
    updated.updated_at = Utc::now();
    store.save_molecule(&mol_id, &updated)?;

    // Write the captured output to a per-step artifact for the audit trail
    // (parity with `gate-output.log`).
    let log_content = format!(
        "# Query step {} (mol: {mol_id})\n# Source: {source_label}\n# Expr: {}\n# Output var: {}\n\n{}",
        step.id, query.expr, query.output_var, serialised,
    );
    let _ = fs::write(mol_dir.join("query-output.log"), log_content);

    // Emit the typed event.
    let preview = if serialised.len() > 512 {
        format!("{}…", &serialised[..512])
    } else {
        serialised.clone()
    };
    let _ = cosmon_state::event_log::emit_one(
        &events_path,
        cosmon_core::event_v2::EventV2::QueryStepEvaluated {
            molecule_id: mol_id.clone(),
            step_id: step.id.clone(),
            expr: query.expr.clone(),
            source: source_label.clone(),
            output_var: query.output_var.clone(),
            result_preview: preview,
        },
        None,
    );

    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let formula_path = formulas_dir.join(format!("{}.formula.toml", updated.formula_id));
    advance_gate_step(ctx, store, &mol_id, &updated, formula, &formula_path)?;

    if ctx.json {
        let out = serde_json::json!({
            "command": "tackle",
            "molecule_id": mol_id.as_str(),
            "mode": "query",
            "step": step.id,
            "expr": query.expr,
            "source": source_label,
            "output_var": query.output_var,
            "result": serialised,
        });
        println!("{out}");
    } else {
        println!(
            "🔎 Query step passed: {mol_id} (step: {}, {}={serialised})",
            step.id, query.output_var,
        );
    }
    Ok(())
}

/// Resolve a [`cosmon_core::formula::QuerySource`] to a JSON document
/// the dot-path evaluator can consume.
///
/// Today's surface is small: molecule `state.json`, `prompt.md`,
/// `briefing.md` (returned as `{"text": ...}` so dot-paths can still
/// access the body), and `events.jsonl` (returned as a JSON array).
fn resolve_query_source(
    store: &FileStore,
    mol_id: &MoleculeId,
    mol_dir: &Path,
    source: &cosmon_core::formula::QuerySource,
) -> anyhow::Result<(String, serde_json::Value)> {
    use cosmon_core::formula::QuerySource;
    match source {
        QuerySource::CurrentMoleculeState => {
            let mol = store.load_molecule(mol_id)?;
            let json = serde_json::to_value(&mol)?;
            Ok((format!("molecule:{mol_id}"), json))
        }
        QuerySource::MoleculeState(target_id) => {
            let mol = store.load_molecule(target_id)?;
            let json = serde_json::to_value(&mol)?;
            Ok((format!("molecule:{target_id}"), json))
        }
        QuerySource::Prompt => {
            let path = mol_dir.join("prompt.md");
            let text = fs::read_to_string(&path).map_err(|e| {
                anyhow::anyhow!("failed to read prompt.md at {}: {e}", path.display())
            })?;
            Ok(("prompt".to_owned(), serde_json::json!({ "text": text })))
        }
        QuerySource::Briefing => {
            let path = mol_dir.join("briefing.md");
            let text = fs::read_to_string(&path).map_err(|e| {
                anyhow::anyhow!("failed to read briefing.md at {}: {e}", path.display())
            })?;
            Ok(("briefing".to_owned(), serde_json::json!({ "text": text })))
        }
        QuerySource::Events => {
            let path = mol_dir.join("events.jsonl");
            let text = fs::read_to_string(&path).map_err(|e| {
                anyhow::anyhow!("failed to read events.jsonl at {}: {e}", path.display())
            })?;
            let mut arr: Vec<serde_json::Value> = Vec::new();
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    arr.push(v);
                }
            }
            Ok(("events".to_owned(), serde_json::Value::Array(arr)))
        }
    }
}

// ---------------------------------------------------------------------------
// LLM execution — checkpointed streaming step
// ---------------------------------------------------------------------------

/// Execute a `[steps.llm]` step: stream a completion from the registered
/// provider into the configured output path with checkpoint flushing,
/// retrying from the on-disk prefix on per-checkpoint stalls. Emits a
/// typed `ExternalChannelTimeout` event on stalls, retries up to
/// `max_retries`, and advances the molecule on success or collapses on
/// budget exhaustion.
#[allow(clippy::too_many_lines)]
fn execute_llm(
    ctx: &Context,
    store: &FileStore,
    mol: MoleculeData,
    formula: &Formula,
    step: &cosmon_core::formula::Step,
) -> anyhow::Result<()> {
    use crate::llm::{lookup_provider, run_attempt, FileSink, RunOutcome, SystemClock};
    use cosmon_core::event_v2::ExternalChannelTimeoutKind;

    let mol_id = mol.id.clone();
    let mol_dir = store.molecule_dir(&mol_id);
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");

    let Some(spec) = step.llm.as_ref() else {
        anyhow::bail!("execute_llm called on non-llm step (internal bug)");
    };

    // Resolve provider before mutating molecule state — fail fast.
    let Some(provider) = lookup_provider(&spec.provider) else {
        let reason = format!(
            "unknown llm provider \"{}\" (registered providers: mock)",
            spec.provider,
        );
        let mut updated = mol;
        updated.status = MoleculeStatus::Collapsed;
        updated.collapse_reason = Some(reason.clone());
        updated.updated_at = Utc::now();
        store.save_molecule(&mol_id, &updated)?;
        return Err(anyhow::anyhow!(reason));
    };

    // Mark Running.
    let mut updated = mol;
    if updated.status == MoleculeStatus::Pending || updated.status == MoleculeStatus::Queued {
        updated.status = MoleculeStatus::Running;
        updated.updated_at = Utc::now();
        store.save_molecule(&mol_id, &updated)?;
    }

    // Resolve prompt body.
    let prompt = match (spec.prompt.as_deref(), spec.prompt_file.as_ref()) {
        (Some(s), _) => s.to_owned(),
        (None, Some(rel)) => {
            let path = mol_dir.join(rel);
            fs::read_to_string(&path).map_err(|e| {
                anyhow::anyhow!("failed to read prompt_file {}: {e}", path.display())
            })?
        }
        (None, None) => {
            anyhow::bail!("internal bug: llm step missing prompt (validator should reject)");
        }
    };

    // Output path is relative to the molecule directory.
    let output_full = mol_dir.join(&spec.output_path);
    let mut sink = FileSink::open(&output_full)?;

    let clock = SystemClock;
    let mut attempt = 1u32;
    let outcome = loop {
        let attempt_outcome = run_attempt(spec, &prompt, &mut sink, provider.as_ref(), &clock)?;
        match attempt_outcome {
            RunOutcome::Completed { .. } => break attempt_outcome,
            RunOutcome::Stalled {
                bytes_flushed,
                age_s,
            } => {
                let _ = cosmon_state::event_log::emit_one(
                    &events_path,
                    cosmon_core::event_v2::EventV2::ExternalChannelTimeout {
                        molecule_id: mol_id.clone(),
                        step_id: step.id.clone(),
                        provider: spec.provider.clone(),
                        kind: ExternalChannelTimeoutKind::Checkpoint,
                        age_s: Some(age_s),
                        bytes_flushed,
                        attempt,
                    },
                    None,
                );
                if attempt >= spec.max_retries {
                    break attempt_outcome;
                }
                attempt += 1;
            }
            RunOutcome::ProviderAborted {
                bytes_flushed,
                ref detail,
            } => {
                let _ = cosmon_state::event_log::emit_one(
                    &events_path,
                    cosmon_core::event_v2::EventV2::ExternalChannelTimeout {
                        molecule_id: mol_id.clone(),
                        step_id: step.id.clone(),
                        provider: spec.provider.clone(),
                        kind: ExternalChannelTimeoutKind::ProviderAborted,
                        age_s: None,
                        bytes_flushed,
                        attempt,
                    },
                    None,
                );
                let _ = detail;
                if attempt >= spec.max_retries {
                    break attempt_outcome;
                }
                attempt += 1;
            }
            RunOutcome::TotalBudgetExceeded { bytes_flushed } => {
                let _ = cosmon_state::event_log::emit_one(
                    &events_path,
                    cosmon_core::event_v2::EventV2::ExternalChannelTimeout {
                        molecule_id: mol_id.clone(),
                        step_id: step.id.clone(),
                        provider: spec.provider.clone(),
                        kind: ExternalChannelTimeoutKind::TotalBudget,
                        age_s: None,
                        bytes_flushed,
                        attempt,
                    },
                    None,
                );
                break attempt_outcome;
            }
        }
    };

    match outcome {
        RunOutcome::Completed {
            bytes_flushed,
            checkpoints,
        } => {
            let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
            let formula_path = formulas_dir.join(format!("{}.formula.toml", updated.formula_id));
            advance_gate_step(ctx, store, &mol_id, &updated, formula, &formula_path)?;
            if ctx.json {
                let out = serde_json::json!({
                    "command": "tackle",
                    "molecule_id": mol_id.as_str(),
                    "mode": "llm",
                    "step": step.id,
                    "provider": spec.provider,
                    "model": spec.model,
                    "bytes_flushed": bytes_flushed,
                    "checkpoints": checkpoints,
                    "result": "completed",
                });
                println!("{out}");
            } else {
                println!(
                    "🤖 LLM step passed: {mol_id} (step: {}, {bytes_flushed} bytes, {checkpoints} checkpoints)",
                    step.id,
                );
            }
            Ok(())
        }
        RunOutcome::Stalled { .. }
        | RunOutcome::ProviderAborted { .. }
        | RunOutcome::TotalBudgetExceeded { .. } => {
            let reason = format!("llm step failed after {attempt} attempt(s): {outcome:?}");
            let mut updated2 = store.load_molecule(&mol_id)?;
            updated2.status = MoleculeStatus::Collapsed;
            updated2.collapse_reason = Some(reason.clone());
            updated2.updated_at = Utc::now();
            store.save_molecule(&mol_id, &updated2)?;
            Err(anyhow::anyhow!(reason))
        }
    }
}

/// Emit a `GateStarted` event.
fn emit_gate_started(events_path: &Path, mol_id: &MoleculeId, step_id: &str, command: &str) {
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        cosmon_core::event_v2::EventV2::GateStarted {
            molecule_id: mol_id.clone(),
            step_id: step_id.to_owned(),
            command: command.to_owned(),
        },
        None,
    );
}

/// Emit a `GateFailed` event.
fn emit_gate_failed(
    events_path: &Path,
    mol_id: &MoleculeId,
    step_id: &str,
    exit_code: i32,
    stderr_tail: &str,
) {
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        cosmon_core::event_v2::EventV2::GateFailed {
            molecule_id: mol_id.clone(),
            step_id: step_id.to_owned(),
            exit_code,
            stderr_tail: stderr_tail.to_owned(),
        },
        None,
    );
}

/// Write captured gate output to `MOLECULE_DIR/gate-output.log`.
fn write_gate_log(
    mol_dir: &Path,
    mol_id: &MoleculeId,
    step_id: &str,
    command: &str,
    output: &std::process::Output,
    duration_ms: u64,
) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);
    let log_content = format!(
        "# Gate: {mol_id} (step: {step_id})\n\
         # Command: {command}\n\
         # Exit code: {exit_code}\n\
         # Duration: {duration_ms}ms\n\n\
         --- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}",
    );
    let _ = fs::write(mol_dir.join("gate-output.log"), &log_content);
}

/// Handle a successful gate step: emit event, advance molecule.
#[allow(clippy::too_many_arguments)]
fn handle_gate_success(
    ctx: &Context,
    store: &FileStore,
    events_path: &Path,
    mol_id: &MoleculeId,
    mol: &MoleculeData,
    formula: &Formula,
    step: &cosmon_core::formula::Step,
    exit_code: i32,
    duration_ms: u64,
) -> anyhow::Result<()> {
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        cosmon_core::event_v2::EventV2::GateCompleted {
            molecule_id: mol_id.clone(),
            step_id: step.id.clone(),
            exit_code,
            duration_ms,
        },
        None,
    );

    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let formula_path = formulas_dir.join(format!("{}.formula.toml", mol.formula_id));
    advance_gate_step(ctx, store, mol_id, mol, formula, &formula_path)?;

    if ctx.json {
        let out = serde_json::json!({
            "command": "tackle",
            "molecule_id": mol_id.as_str(),
            "mode": "gate",
            "step": step.id,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "result": "completed",
        });
        println!("{out}");
    } else {
        println!(
            "⚡ Gate passed: {mol_id} (step: {}, {duration_ms}ms)",
            step.id,
        );
    }
    Ok(())
}

/// Handle a failed gate step: emit event, collapse molecule.
#[allow(clippy::too_many_arguments)]
fn handle_gate_failure(
    ctx: &Context,
    store: &FileStore,
    events_path: &Path,
    mol_id: &MoleculeId,
    mol: &mut MoleculeData,
    step: &cosmon_core::formula::Step,
    exit_code: i32,
    stderr: &str,
    timed_out: bool,
    duration: &std::time::Duration,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let stderr_tail = {
        let s = stderr.trim();
        if s.len() > 500 {
            s[s.len() - 500..].to_owned()
        } else {
            s.to_owned()
        }
    };
    let reason = if timed_out {
        format!(
            "gate timed out after {}s (limit: {timeout_secs}s)",
            duration.as_secs(),
        )
    } else {
        format!(
            "gate failed (exit {exit_code}): {}",
            stderr_tail.lines().last().unwrap_or("(no stderr)"),
        )
    };

    emit_gate_failed(
        events_path,
        mol_id,
        &step.id,
        if timed_out { -1 } else { exit_code },
        &stderr_tail,
    );

    mol.status = MoleculeStatus::Collapsed;
    mol.updated_at = Utc::now();
    store.save_molecule(mol_id, mol)?;

    if ctx.json {
        let out = serde_json::json!({
            "command": "tackle",
            "molecule_id": mol_id.as_str(),
            "mode": "gate",
            "step": step.id,
            "exit_code": exit_code,
            "result": "collapsed",
            "reason": reason,
        });
        println!("{out}");
    } else {
        eprintln!("💥 Gate failed: {mol_id} — {reason}");
    }
    Ok(())
}

/// Advance a molecule past a completed gate step.
///
/// Uses the same `evolve` logic as `cs evolve` to advance `current_step` and
/// mark completion if this was the last step.
fn advance_gate_step(
    ctx: &Context,
    store: &FileStore,
    mol_id: &MoleculeId,
    mol: &MoleculeData,
    formula: &Formula,
    _formula_path: &Path,
) -> anyhow::Result<()> {
    use cosmon_core::evolve;

    let request = evolve::EvolveRequest {
        evidence: "gate step completed (exit 0)".to_owned(),
        timestamp: Utc::now(),
    };
    let outcome = evolve::evolve(
        mol.status,
        mol.current_step,
        &mol.completed_steps,
        formula,
        &request,
    )?;

    // Persist the step advancement.
    let mut updated = store.load_molecule(mol_id)?;
    let step_id = cosmon_core::id::StepId::new(&outcome.completed_step.id)?;
    updated.completed_steps.push(step_id);
    updated.updated_at = Utc::now();

    let is_completed = matches!(outcome.new_state, evolve::NewState::Completed);

    match &outcome.new_state {
        evolve::NewState::Active { current_step, .. } => {
            updated.current_step = *current_step;
        }
        evolve::NewState::Completed => {
            if formula.freeze_on_last_step {
                updated.status = MoleculeStatus::Frozen;
            } else {
                updated.status = MoleculeStatus::Completed;
            }
        }
        _ => {}
    }

    store.save_molecule(mol_id, &updated)?;

    // Emit step-completed event.
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");
    let _ = cosmon_state::event_log::emit_one(
        &events_path,
        cosmon_core::event_v2::EventV2::MoleculeStepCompleted {
            molecule_id: mol_id.clone(),
            step: outcome.completed_step.index,
            total: formula.steps.len(),
            duration_ms: None,
            step_hash: None,
        },
        None,
    );

    if is_completed {
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::MoleculeCompleted {
                molecule_id: mol_id.clone(),
                duration_ms: None,
                reason: "all gate steps completed".to_owned(),
            },
            None,
        );
        if !ctx.json {
            println!("✅ Molecule {mol_id} completed (all steps done)");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Molecule resolution (exact, prefix, fuzzy)
// ---------------------------------------------------------------------------

/// Resolve a molecule by exact ID, prefix, or substring match.
fn resolve_molecule(store: &FileStore, query: &str) -> anyhow::Result<MoleculeData> {
    // Try exact match first.
    if let Ok(exact_id) = MoleculeId::new(query) {
        if let Ok(mol) = store.load_molecule(&exact_id) {
            return Ok(mol);
        }
    }

    // List all molecules and search.
    let all = store.list_molecules(&MoleculeFilter::default())?;

    // Prefix match.
    let prefix_matches: Vec<_> = all
        .iter()
        .filter(|m| m.id.as_str().starts_with(query))
        .collect();

    if prefix_matches.len() == 1 {
        return Ok(prefix_matches[0].clone());
    }

    // Substring match across id, formula_id, title, and topic.
    let query_lower = query.to_lowercase();
    let substr_matches: Vec<_> = all
        .iter()
        .filter(|m| {
            let id_lower = m.id.as_str().to_lowercase();
            let formula_lower = m.formula_id.as_str().to_lowercase();
            let title_lower = m
                .variables
                .get("title")
                .map_or(String::new(), |s| s.to_lowercase());
            let topic_lower = m
                .variables
                .get("topic")
                .map_or(String::new(), |s| s.to_lowercase());
            id_lower.contains(&query_lower)
                || formula_lower.contains(&query_lower)
                || title_lower.contains(&query_lower)
                || topic_lower.contains(&query_lower)
        })
        .collect();

    match substr_matches.len() {
        0 => Err(anyhow::anyhow!("no molecule matching \"{query}\"")),
        1 => Ok(substr_matches[0].clone()),
        n => {
            // If prefix matched multiple, report those. Otherwise report substring matches.
            let matches = if prefix_matches.len() > 1 {
                &prefix_matches
            } else {
                &substr_matches
            };
            let lines: Vec<_> = matches
                .iter()
                .map(|m| {
                    let label = m
                        .variables
                        .get("topic")
                        .or_else(|| m.variables.get("title"))
                        .map_or_else(|| m.formula_id.as_str().to_owned(), String::clone);
                    format!("  {} ({})", m.id, label)
                })
                .collect();
            Err(anyhow::anyhow!(
                "ambiguous query \"{query}\" matches {n} molecules:\n{}",
                lines.join("\n")
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// FleetSpec loading
// ---------------------------------------------------------------------------

/// Load a [`FleetSpec`] for the current project.
///
/// Tries `.cosmon/fleet.toml` first (relative to the state directory's parent,
/// which is `.cosmon/`). If the file is missing or fails to parse, falls back
/// to [`FleetSpec::default_singleton()`] — a single-agent fleet that matches
/// today's solo-worker behavior exactly.
///
/// This function is infallible: the fleet-of-one path never errors.
fn load_fleet_spec(state_dir: &Path) -> FleetSpec {
    let fleet_toml_path = state_dir.parent().unwrap_or(state_dir).join("fleet.toml");

    match fs::read_to_string(&fleet_toml_path) {
        Ok(text) => FleetSpec::parse(&text).unwrap_or_else(|_| FleetSpec::default_singleton()),
        Err(_) => FleetSpec::default_singleton(),
    }
}

// ---------------------------------------------------------------------------
// Fleet briefing auto-injection
// ---------------------------------------------------------------------------

/// Try to auto-inject a briefing from the fleet template when `briefing.md`
/// does not exist.
///
/// Looks for `.cosmon/fleet.toml` (or the path given by the molecule's
/// `fleet_template` variable), parses it as a [`FleetSpec`], and searches
/// the `[[agents]]` entries for one whose name matches the molecule's
/// `formula_id`. If found and the agent has a `prompt`, the prompt is
/// written to `briefing_path` with a standard header so the bootstrap
/// prompt picks it up.
///
/// This is a **fallback**: if `briefing.md` already exists (written by a
/// parent planner), the caller never reaches this function.
fn try_inject_fleet_briefing(
    state_dir: &Path,
    mol: &MoleculeData,
    briefing_path: &Path,
) -> Option<String> {
    // Resolve fleet.toml: check molecule variable override, else .cosmon/fleet.toml.
    let fleet_toml_path = mol.variables.get("fleet_template").map_or_else(
        || {
            // state_dir is .cosmon/state/ — parent is .cosmon/
            state_dir.parent().unwrap_or(state_dir).join("fleet.toml")
        },
        PathBuf::from,
    );

    let toml_text = fs::read_to_string(&fleet_toml_path).ok()?;
    let spec = FleetSpec::parse(&toml_text).ok()?;

    // Convention: fleet agent name == molecule formula_id.
    let formula_name = mol.formula_id.as_str();
    let agent = spec
        .agents
        .iter()
        .find(|a| a.name.as_str() == formula_name)?;

    let prompt = agent.prompt.as_deref()?;
    if prompt.is_empty() {
        return None;
    }

    // Build the briefing with standard structure.
    let briefing = format!(
        "# Molecule: {mol_id}\n\n## Role\n\n{prompt}\n",
        mol_id = mol.id,
        prompt = prompt,
    );

    // Write to disk so subsequent reads (e.g. resume) find it.
    if let Some(parent) = briefing_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::write(briefing_path, &briefing).is_ok() {
        eprintln!("auto-injected briefing from fleet agent \"{}\"", agent.name);
        Some(briefing)
    } else {
        // Write failed — proceed without briefing (current behavior).
        None
    }
}

// ---------------------------------------------------------------------------
// Formula loading
// ---------------------------------------------------------------------------

/// Try to load the formula for a molecule from .cosmon/formulas/.
fn load_formula_for_molecule(_state_dir: &std::path::Path, mol: &MoleculeData) -> Option<Formula> {
    let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);
    let formula_path = formulas_dir.join(format!("{}.formula.toml", mol.formula_id));

    let toml_text = fs::read_to_string(&formula_path).ok()?;
    Formula::parse(&toml_text).ok()
}

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

/// Render the "run verification gates" step of the worker prompt.
///
/// If the project has configured any gate commands under `[gates]` in
/// `.cosmon/config.toml`, render them as an explicit numbered list so the
/// worker runs exactly what the project author specified. Otherwise fall
/// back to a neutral, language-agnostic instruction — cosmon does not
/// assume any particular toolchain.
fn render_gates_instruction(gates: &cosmon_core::config::GatesConfig) -> String {
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
#[allow(clippy::too_many_lines, clippy::comparison_chain)]
fn build_prompt(
    mol: &MoleculeData,
    formula: Option<&Formula>,
    briefing: Option<&str>,
    config: &ProjectConfig,
    molecule_dir: &Path,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let kind_str = mol
        .kind
        .map_or_else(|| "molecule".to_owned(), |k| k.to_string());
    let kind_emoji = mol
        .kind
        .map_or("🔧", cosmon_core::kind::MoleculeKind::emoji);

    // ── AUTONOMOUS WORK MODE HEADER ─────────────────────────────
    let _ = writeln!(out, "# 🚨 AUTONOMOUS WORK MODE — NON-NEGOTIABLE 🚨\n");
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
        "This is physics, not politeness. A molecule in motion stays in motion. \
         Every moment you wait is a moment the pipeline stalls.\n\n",
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
         the SPDX text registry, or `choosetenant_auditornse.com` — and write the \
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
    // Hand the worker the EXACT absolute, already-resolved canonical
    // molecule_dir so it never has to re-derive the path from prose and
    // never abbreviates to the non-canonical `.cosmon/molecules/<id>/`.
    // The git worktree (`.worktrees/<id>/`) is destroyed at `cs done`, so
    // durable artifacts written there are lost. Generic across all formulas
    // (advisory backstop for the artifact-path-hygiene class; cf.
    // idea-20260531-107d, delib-20260410-b79f data-loss recurrence).
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

    // ── EXECUTION PROTOCOL ──────────────────────────────────────
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
            out.push_str("7. Immediately start the next step. Do NOT pause.\n\n");
        }
        OnComplete::Commit => {
            let _ = writeln!(
                out,
                "5. Advance: `cs evolve {} --evidence \"<summary>\" --formula .cosmon/formulas/{}.formula.toml`",
                mol.id, mol.formula_id
            );
            out.push_str("6. Immediately start the next step. Do NOT pause.\n\n");
        }
    }

    // Completion instructions vary based on on_complete config.
    match on_complete {
        OnComplete::CommitPushPr => {
            let _ = writeln!(
                out,
                "**When ALL steps are done:**\n\
                 1. Push your branch: `git push -u origin HEAD`\n\
                 2. Create a pull request: `gh pr create --title \"<title>\" --body \"<summary>\"`\n\
                 3. Complete the molecule:\n\
                 ```\n\
                 cs complete {} --reason \"<summary>\"\n\
                 ```\n\
                 There is NO other valid way to end. No summary. No \"let me know\".\n",
                mol.id
            );
        }
        OnComplete::CommitPush => {
            let _ = writeln!(
                out,
                "**When ALL steps are done:**\n\
                 1. Push your branch: `git push -u origin HEAD`\n\
                 2. Complete the molecule:\n\
                 ```\n\
                 cs complete {} --reason \"<summary>\"\n\
                 ```\n\
                 There is NO other valid way to end. No summary. No \"let me know\".\n",
                mol.id
            );
        }
        OnComplete::Commit => {
            let _ = writeln!(
                out,
                "**When ALL steps are done, your ONLY valid exit is:**\n\
                 ```\n\
                 cs complete {} --reason \"<summary>\"\n\
                 ```\n\
                 There is NO other valid way to end. No summary. No \"let me know\".\n",
                mol.id
            );
        }
    }

    // ── DO NOT LIST (targets specific Claude failure modes) ─────
    out.push_str("## DO NOT — These are violations\n\n");
    out.push_str("- Do NOT pause between steps to summarize what you did.\n");
    out.push_str("- Do NOT ask \"shall I continue?\" or \"would you like me to proceed?\".\n");
    out.push_str("- Do NOT describe what you are about to do — just DO IT.\n");
    out.push_str("- Do NOT offer alternatives or ask for confirmation.\n");
    out.push_str("- Do NOT wait for user input at the ❯ prompt between steps.\n");

    // DO NOT items vary based on on_complete config.
    match on_complete {
        OnComplete::Commit => {
            out.push_str("- Do NOT create GitHub PRs — integration is local via molecules.\n");
            out.push_str("- Do NOT push to remote — commits stay on the local branch.\n\n");
        }
        OnComplete::CommitPush => {
            out.push_str("- Do NOT create GitHub PRs — only push the branch.\n\n");
        }
        OnComplete::CommitPushPr => {
            out.push('\n');
        }
    }

    // ── FINAL IMPERATIVE ────────────────────────────────────────
    let _ = writeln!(
        out,
        "## ▶ Execute step {} NOW.\n\n\
         Begin immediately. No preamble. No planning summary. Just start working.",
        mol.current_step + 1
    );

    out
}

// ---------------------------------------------------------------------------
// Git worktree
// ---------------------------------------------------------------------------

/// Resolve the git branch to start from, based on the molecule's
/// `BlockedBy` dependencies. If a dependency's branch (`feat/{dep_id}`)
/// exists locally, we branch from it so the worktree inherits the
/// predecessor's output. If multiple blocker branches exist, we pick the
/// one with the **most recent tip commit** (highest committer timestamp).
/// Falls back to `None` (= branch from HEAD/main) if no dependency branch
/// exists.
///
/// This aligns the git DAG with the cosmon DAG: the reviewer's branch
/// is a child of the writer's branch, so `wiki/article.md` is already
/// present without needing a merge into main first.
///
/// # Multi-blocker selection rule (decision task-20260712-2686, C6-2)
///
/// The intended rule is **most-recent-by-commit-timestamp**, matching this
/// function's historical contract and `docs/architectural-invariants.md`
/// §3c ("finds the most recent completed blocker's branch"). The prior
/// implementation returned the *first* blocker in `blocked_by()` iteration
/// order whose branch existed — an artefact of link-insertion order, not a
/// meaningful choice, and a silent drift from both the docstring and the
/// doctrine (C6-2). Iteration order must **not** decide the branch point.
///
/// A **fundamental git limitation** bounds this: a branch has a single
/// parent, so a worktree can inherit at most *one* sibling blocker's output.
/// When ≥2 blocker branches are still live (neither merged into `main`),
/// the selected start-point carries only the freshest one's output; the
/// others stay invisible until they merge. This case only arises on a
/// **manual** `cs tackle` with multiple un-`done` blockers — under `cs run`,
/// merge-before-dispatch (§3d) deletes each blocker branch before dispatch,
/// so this function returns `None` and `base = main` already holds every
/// merged output. We emit a warning (below) so the operator knows to prefer
/// `cs run` for true multi-blocker convergence.
/// Whether this `cs tackle` is an **exposed / multi-tenant** launch — a worker
/// spawned through the RPP API rather than by a local operator.
///
/// Signalled by `COSMON_API_REQUEST=1`, the envelope marker the rpp-adapter
/// sets on *every* subprocess it spawns (see
/// `cosmon-rpp-adapter::subprocess` — `.env(env::COSMON_API_REQUEST, "1")`).
/// It is the canonical "came through the hosted API" bit: always present on the
/// tenant path, absent for a local operator's `cs tackle`. Used by the egress
/// preflight (RÉSIDUEL SÉCU B, task-20260713-d436) to force hard enforcement so
/// a strict-local policy that cannot be kernel-enforced on an exposed host is
/// refused rather than degraded to an unconfined passthrough shell.
///
/// `env_lookup` is injected so the predicate is unit-testable without mutating
/// the process environment (same seam as `tackle_env`'s helpers).
fn egress_launch_is_exposed<F>(env_lookup: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    env_lookup("COSMON_API_REQUEST").as_deref() == Some("1")
}

fn resolve_branch_start_point(repo_root: &std::path::Path, mol: &MoleculeData) -> Option<String> {
    let blockers = mol.blocked_by();
    if blockers.is_empty() {
        return None;
    }

    // Collect every blocker whose `feat/{id}` branch exists locally, paired
    // with its tip's committer timestamp (unix seconds). `git log -1 --format=%ct
    // <ref>` both probes existence (non-zero exit if the ref is missing) and
    // reads the timestamp in a single call.
    let mut live: Vec<(String, i64)> = Vec::new();
    for dep_id in blockers {
        let branch = format!("feat/{dep_id}");
        let out = std::process::Command::new("git")
            .args([
                "-C",
                &repo_root.to_string_lossy(),
                "log",
                "-1",
                "--format=%ct",
                &format!("refs/heads/{branch}"),
            ])
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                if let Ok(ts) = String::from_utf8_lossy(&o.stdout).trim().parse::<i64>() {
                    live.push((branch, ts));
                }
            }
        }
    }

    if live.len() >= 2 {
        eprintln!(
            "cs tackle: {} live blocker branches found; a git worktree inherits \
             a single parent, so branching from the most-recent blocker leaves \
             the others' output invisible until they merge. Prefer `cs run` \
             (merge-before-dispatch) for true multi-blocker convergence.",
            live.len()
        );
    }

    // Pick the most-recent tip. Ties are broken by `blocked_by()` order —
    // we only replace `best` on a *strictly* greater timestamp, so the
    // first-declared blocker wins an exact tie. Deterministic either way.
    let mut best: Option<(String, i64)> = None;
    for (branch, ts) in live {
        if best.as_ref().is_none_or(|(_, best_ts)| ts > *best_ts) {
            best = Some((branch, ts));
        }
    }
    best.map(|(branch, _)| branch)
}

/// Find the git repository root from CWD.
pub(super) fn find_repo_root() -> anyhow::Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git: {e}"))?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "not in a git repository: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

/// Create a git branch and worktree, idempotently.
///
/// When `start_point` is `Some("feat/mol-xxx")`, the branch is created
/// from that ref instead of HEAD. This aligns the git DAG with the
/// cosmon DAG: a reviewer's worktree branches from the writer's branch,
/// so it sees the writer's output without requiring a merge into main
/// first. Information flows through branch topology.
pub(super) fn create_worktree(
    repo_root: &std::path::Path,
    worktree_path: &std::path::Path,
    branch: &str,
    start_point: Option<&str>,
) -> anyhow::Result<()> {
    // If worktree already exists, reuse it.
    if worktree_path.exists() {
        return Ok(());
    }

    // Create branch from start_point (blocker's branch) or HEAD (main).
    // Pre-fix (task-20260416-ef31): the result of `git branch` was
    // silently discarded. A disk-full / permission / corrupt-repo failure
    // would fall through, `git worktree add` would then also fail
    // confusingly, and the tmux session still got written with a surface
    // "Running" row — one of the mechanisms behind the surface-lie class.
    // We now check every non-"already exists" failure and surface it.
    let lossy = repo_root.to_string_lossy();
    let mut args: Vec<String> = vec![
        "-C".to_owned(),
        lossy.into_owned(),
        "branch".to_owned(),
        branch.to_owned(),
    ];
    if let Some(sp) = start_point {
        args.push(sp.to_owned());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // `LC_ALL=C` pins git's stderr to the English locale so the
    // "already exists" idempotence probe below survives non-English
    // operator locales. See done.rs::try_merge_branch for the structural
    // rationale and the 2026-05-22 (drain-worker f877) discovery.
    let branch_out = std::process::Command::new("git")
        .env("LC_ALL", "C")
        .args(refs)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git branch: {e}"))?;
    if !branch_out.status.success() {
        let stderr = String::from_utf8_lossy(&branch_out.stderr);
        // The ONLY tolerated failure is "branch already exists" — tackle is
        // idempotent when re-invoked on the same molecule, so the branch
        // may legitimately predate this call (e.g. `--force` respawn,
        // partial prior tackle, manual `git branch`). Any other failure is
        // unexpected and MUST surface: proceeding would silently paper
        // over a disk-full / corrupt-repo / permission problem and then
        // cascade into a surface lie downstream.
        if !stderr.contains("already exists") {
            return Err(anyhow::anyhow!(
                "git branch {branch} failed: {}",
                stderr.trim()
            ));
        }
    }

    // Create worktree directory parent.
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // `LC_ALL=C` pins git's stderr to the English locale so the
    // "already checked out" / "already exists" idempotence probe below
    // survives non-English operator locales (drain-worker f877,
    // 2026-05-22).
    let output = std::process::Command::new("git")
        .env("LC_ALL", "C")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            branch,
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git worktree add: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If worktree already checked out, that's fine.
        if stderr.contains("already checked out") || stderr.contains("already exists") {
            pin_operator_identity(repo_root, worktree_path);
            return Ok(());
        }
        return Err(anyhow::anyhow!(
            "git worktree add failed: {}",
            stderr.trim()
        ));
    }

    // Pin the operator identity at the worktree seam (delib-20260717-194b, F2).
    // This is the single choke point every adapter passes through, so feature
    // commits are BORN operator-authored — no post-hoc rewrite, no SHA churn,
    // no ancestry-guard breakage. The `cs done` author-slot assertion (F4) is
    // the backstop for when this silently no-ops (env precedence, a late
    // amend); pinning here reduces the failure *rate*, the assertion *closes*
    // the hole. Best-effort: a failure to resolve or set identity never blocks
    // tackle (the assertion catches the residue).
    pin_operator_identity(repo_root, worktree_path);

    Ok(())
}

/// Pin the operator's git identity onto a freshly-created worktree
/// (delib-20260717-194b, F2).
///
/// Resolves the operator identity from `repo_root`'s effective git config
/// (`user.name` / `user.email`, which walks local → global → system) and writes
/// it into the worktree so every worker git process — claude, codex, aider,
/// gemini — commits with the operator in the author AND committer slots. The
/// maker (Noogram) and the real adapter are credited ONLY on `Co-Authored-By:`
/// trailers, never in the author slot (direction-of-control, tolnay Q3).
///
/// Best-effort and non-fatal: when no identity is configured (a bare CI
/// checkout) nothing is written and the worktree inherits whatever the repo
/// config already carries. The `cs done` author-slot assertion is the
/// load-bearing backstop; this is defense-in-depth that lowers the failure
/// rate at the source.
fn pin_operator_identity(repo_root: &std::path::Path, worktree_path: &std::path::Path) {
    for key in ["user.name", "user.email"] {
        if let Some(value) = git_config_value(repo_root, key) {
            let _ = std::process::Command::new("git")
                .args([
                    "-C",
                    &worktree_path.to_string_lossy(),
                    "config",
                    key,
                    &value,
                ])
                .output();
        }
    }
}

/// Read a single git config value from `repo_root`'s effective config.
///
/// Returns `None` when the key is unset or the probe fails, so the caller can
/// fall back cleanly rather than inventing a value.
fn git_config_value(repo_root: &std::path::Path, key: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "config", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

// ---------------------------------------------------------------------------
// Fleet registration
// ---------------------------------------------------------------------------

/// Register a tackle-created worker in the fleet.
///
/// Tackle workers are bound 1-to-1 to a tmux session (`cosmon-{mol_id}`)
/// and a molecule. Registering them in fleet.json lets `cs patrol`,
/// `cs patrol --propel`, `cs resume`, and `cs ensemble` see and manage
/// them uniformly with spawn/deploy workers.
///
/// `adapter` is the Worker-Spawn Port Adapter that actually produced the
/// worker (ADR-097 / C8). Pre-TS-0 (ADR-099) this was a `&str`; the
/// [`ValidatedAdapterName`] newtype now forces every caller to thread
/// the value through `validate_adapter_name`, so the byte sequence
/// carried by the emitted `EventV2::WorkerSpawned` is the same one
/// that traversed the validation gate — the cat-test cross-reference
/// `adapter_selected.adapter_name == worker_spawned.adapter_name` is
/// satisfied by construction, not by convention.
///
/// `loop_ownership` is the per-Adapter axis carried jointly with the
/// validated name (ADR-103). The emitted `EventV2::WorkerSpawned`
/// carries the wire-string projection so the cat-test extends to a
/// second invariant: `adapter_selected.loop_ownership ==
/// worker_spawned.loop_ownership`.
///
/// Idempotent: overwrites an existing entry with the same `worker_id`.
/// Detach a `cs realized-watch` child for this dispatch (round-4 / COND-1).
///
/// Re-execs the current binary so the watcher and the dispatcher can never
/// skew versions, parks the child in its own process group (it must survive
/// `cs tackle` returning and any signal aimed at the operator's shell), and
/// silences its stdio — the watcher speaks only through `events.jsonl`.
/// Best-effort: a spawn failure costs the first-turn guarantee for this run
/// (the `cs wait`/`cs run` pollers and the completion seam still capture),
/// never the dispatch itself.
fn spawn_realized_watcher(state_dir: &Path, mol_id: &MoleculeId, worktree_path: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = ProcessCommand::new(exe);
    command
        .args(cosmon_cli::realized_watcher::watcher_argv(
            mol_id.as_str(),
            worktree_path,
            state_dir,
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    // The child is deliberately not waited on: it outlives this process and
    // is reparented to init — no zombie, no supervision coupling.
    let _ = command.spawn();
}

pub(super) fn register_tackle_worker(
    store: &FileStore,
    wid: &WorkerId,
    worktree_path: &Path,
    repo_root: &Path,
    mol: &MoleculeData,
    adapter: &ValidatedAdapterName,
    loop_ownership: LoopOwnership,
) -> anyhow::Result<()> {
    let mut fleet = store.load_fleet().unwrap_or_default();
    let agent_id = AgentId::new("tackle")?;
    let role = mol.assigned_role.unwrap_or(AgentRole::Implementation);
    let mut worker = WorkerData::new(
        wid.clone(),
        agent_id,
        role,
        Clearance::Write,
        WorkerStatus::Active,
    );
    worker.desired = DesiredState::Running;
    worker.repo = Some(cosmon_filestore::make_relative(worktree_path, repo_root));
    worker.current_molecule = Some(mol.id.clone());
    fleet.workers.insert(wid.clone(), worker);
    store.save_fleet(&fleet)?;

    // Emit EventV2::WorkerSpawned. This event IS the passive "worker created
    // at ..." metadata — its envelope timestamp is the authoritative
    // spawned_at for the worker. We deliberately do NOT also emit a seed
    // WorkerHeartbeat here: a heartbeat means "the live process just proved
    // it exists" (1 bit of real entropy). Emitting one from the spawner
    // impersonates liveness — it produced the exact failure mode diagnosed
    // in task-4046 (silent exec failure, heartbeat still on the wire). The
    // only legitimate heartbeat emitters are the worker process itself and
    // its bridge (`cs heartbeat`).
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");
    let _ = cosmon_state::event_log::emit_one(
        &events_path,
        cosmon_core::event_v2::EventV2::WorkerSpawned {
            worker_id: wid.clone(),
            molecule: Some(mol.id.clone()),
            session_name: wid.as_str().to_owned(),
            role: role.to_string(),
            adapter_name: adapter.as_str().to_owned(),
            loop_ownership: cosmon_core::event_v2::LoopOwnershipTag::from(loop_ownership),
        },
        None,
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// SessionStart hook (propulsion re-injection)
// ---------------------------------------------------------------------------

/// Install a Claude Code `SessionStart` hook in the worktree that calls
/// `cs prime --hook` at every turn boundary. This re-injects the current
/// step context so the worker never "forgets" to continue.
#[allow(dead_code)]
fn install_session_hook(worktree_path: &Path, mol_id: &str) {
    let claude_dir = worktree_path.join(".claude");
    let _ = std::fs::create_dir_all(&claude_dir);

    // Write settings.local.json with the SessionStart hook.
    let settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "type": "command",
                "command": format!("cs prime {mol_id}"),
            }]
        }
    });

    let settings_path = claude_dir.join("settings.local.json");
    if let Ok(json) = serde_json::to_string_pretty(&settings) {
        let _ = std::fs::write(&settings_path, json);
    }
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

/// Report that a session already exists (idempotent success).
fn report_existing_session(
    ctx: &Context,
    mol: &MoleculeData,
    session_name: &str,
    socket: &str,
    worktree_path: &std::path::Path,
    branch_name: &str,
) {
    if ctx.json {
        let out = serde_json::json!({
            "command": "tackle",
            "molecule_id": mol.id.as_str(),
            "status": mol.status.to_string(),
            "tmux_session": session_name,
            "worktree": worktree_path.to_string_lossy(),
            "branch": branch_name,
            "already_running": true,
            "attach": format!("tmux -L {socket} attach -t {session_name}"),
        });
        println!("{out}");
    } else {
        let kind_emoji = mol.kind.map_or("", |k| k.emoji());
        println!(
            "{kind_emoji} Session already running for {} ({})",
            mol.id, mol.formula_id
        );
        println!("  session:  {session_name}");
        println!("  attach:   tmux -L {socket} attach -t {session_name}");
        println!("  respawn:  cs tackle {} --force", mol.id);
    }
}

/// Spawn a worker session via the Worker-Spawn Port Adapter named
/// `adapter`, wait for readiness, and send the bootstrap prompt.
///
/// `permission_mode_override` lets callers force a non-default mode
/// (e.g. `cs resurrect` reuses the molecule's default). When `None`
/// the per-kind default applies. The override flows only through the
/// Claude branch — Aider derives its permission flags from
/// [`Clearance`] (cf. [`cosmon_transport::aider::AiderPermissionFlags`]).
///
/// # ADR-097 / C8 — multi-Adapter dispatch
///
/// Pre-C8 this function unconditionally invoked `claude`, regardless
/// of the `--adapter` flag the operator passed: `AdapterSelected`
/// emitted `aider` while the tmux pane ran Claude. A academy smoke test
/// that ran Claude while the event claimed aider was the forcing
/// function. C8 replaced the implicit Claude routing with an
/// explicit `match` on the adapter name.
///
/// # ADR-099 / TS-0 — dispatch-site stability
///
/// The `adapter` parameter is `&ValidatedAdapterName`, not `&str`. The
/// only constructor for [`ValidatedAdapterName`] is
/// [`validate_adapter_name`], so this signature makes
/// "spawn-without-validation" a compile error. The catch-all match arm
/// below is genuinely unreachable from in-tree callers and exists as a
/// completeness guard: if a future PR adds a new adapter to the
/// registry but forgets to wire its branch here, the error fires at
/// runtime rather than dispatching silently through Claude. That is a
/// distinct invariant from the validation one TS-0 closes — keep both.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_and_prompt(
    backend: &TmuxBackend,
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    permission_mode_override: Option<&str>,
    mol: &MoleculeData,
    mol_state_dir: &std::path::Path,
    state_dir: &std::path::Path,
    adapter: &ValidatedAdapterName,
    adapters_cfg: Option<&AdaptersConfig>,
    // The per-molecule model pin resolved by `resolve_model_selection`
    // (delib-20260704-b476 C1), or `None` when nothing pinned a model.
    // Adapter-uniform: each arm carries it in its own way — the claude arm
    // through the `ANTHROPIC_MODEL` closure-shadow, the Direct-API arms as
    // the top-priority override above their `[adapters.<name>].default_model`.
    // The id is opaque; an invalid `(adapter, model)` pair is rejected by
    // the backend at launch (composition validation is C5).
    preferred_model: Option<&str>,
    // The resolved adapter's strong cost-class set — threaded to the claude
    // branch's probe-fallback layer so a cheap pin never silently escalates
    // to a strong model on a transient outage (task-20260705-ba98). Only the
    // claude branch pre-flights a fallback chain, so the other arms ignore it.
    strong_set: &[String],
) -> anyhow::Result<()> {
    // Per-Adapter override row (`[adapters.openai]`, `[adapters.anthropic]`)
    // — keys the Direct-API branches lift the api_key_env / base_url /
    // default_model from. `None` means "fall back to env-var + compile-time
    // defaults", which is the historical pre-C6 behaviour.
    let adapter_entry = adapters_cfg.and_then(|cfg| cfg.entry(adapter.as_str()));
    match adapter.as_str() {
        "claude" => spawn_claude_and_prompt(
            backend,
            wid,
            session_name,
            worktree_path,
            prompt,
            permission_mode_override,
            mol,
            mol_state_dir,
            preferred_model,
            strong_set,
        ),
        "aider" => spawn_aider_and_prompt(
            backend,
            wid,
            session_name,
            worktree_path,
            prompt,
            mol,
            adapter_entry,
            preferred_model,
        ),
        // Gap#5 (`task-20260615-df30`) — codex joins claude/aider as the
        // third external-CLI subprocess adapter. Same tmux-pane shape as
        // aider: spawn `codex exec '<prompt>'` into a pane, then assert
        // liveness through the substrate-agnostic `LiveProbe` contract.
        "codex" => spawn_codex_and_prompt(
            backend,
            wid,
            session_name,
            worktree_path,
            prompt,
            mol,
            adapter_entry,
            preferred_model,
        ),
        // `task-20260615-556a` — opencode joins claude/aider/codex as the
        // fourth external-CLI subprocess adapter. Same tmux-pane shape as
        // codex: spawn `opencode run '<prompt>'` into a pane, then assert
        // liveness through the substrate-agnostic `LiveProbe` contract.
        "opencode" => {
            spawn_opencode_and_prompt(backend, wid, session_name, worktree_path, prompt, mol)
        }
        "openai" => spawn_openai_session(
            wid,
            session_name,
            worktree_path,
            prompt,
            mol,
            mol_state_dir,
            adapter_entry,
            preferred_model,
        ),
        "anthropic" => spawn_anthropic_session(
            wid,
            session_name,
            worktree_path,
            prompt,
            mol,
            mol_state_dir,
            adapter_entry,
            preferred_model,
        ),
        // C5 of delib-20260519-a20b — `llama-cpp` (canonical) and
        // `llama` (legacy alias per ADR-106) both reach the same arm.
        // The in-process llama.cpp adapter was removed in the
        // pre-publication scope trim (delib-20260622-187a B1 / ADR-126),
        // so `spawn_llama_session` is now always the typed
        // `FeatureNotCompiled` stub. The registry row stays present so
        // the validation gate's promise ("an adapter listed in the
        // registry dispatches or fails loudly, never silently") holds.
        "llama-cpp" | "llama" => spawn_llama_session(
            wid,
            session_name,
            worktree_path,
            prompt,
            mol,
            mol_state_dir,
            adapter_entry,
            preferred_model,
        ),
        // `task-20260530-821f` (parent `delib-20260530-0877`) — the
        // walking-skeleton local-default branch. Reuses the proven
        // `OpenAIProvider` + `run_agent_loop` (R-openai route from the
        // synthesis) pointed at Ollama's OpenAI-compat `/v1` endpoint:
        // ZERO new provider code, multi-turn tool-calling already in
        // place via `cosmon_agent_harness::run_loop`. This is the
        // built-in `cs tackle` default (no `--adapter` flag), so the
        // loop runs in-process with NO `claude` subprocess. Matched via
        // the floor constant so the dispatch arm tracks the floor name.
        // `task-20260707-7d27` (hole #1): the `local` floor and its
        // `ollama` alias share the identical `OpenAIProvider`-against-Ollama
        // spawn path. The validated name (`adapter.as_str()`) is threaded
        // through so the floor's telemetry stamps the name the operator
        // actually selected — `local` or `ollama` — keeping the ADR-099
        // cat-test (`adapter_selected == worker_spawned`) intact for both.
        BUILTIN_FLOOR_ADAPTER | "ollama" => spawn_detached_local_worker(
            adapter.as_str(),
            wid,
            session_name,
            worktree_path,
            prompt,
            mol,
            mol_state_dir,
            state_dir,
            adapter_entry,
            preferred_model,
        ),
        // `validate_adapter_name` already refused any name not in the
        // dispatch registry; reaching the catch-all means a new
        // adapter was added to the registry without a matching branch
        // here — completeness invariant, not user error.
        other => Err(anyhow::anyhow!(
            "cs tackle: adapter '{other}' validated by validate_adapter_name but \
             not wired in spawn_and_prompt — this is a build-time bug, not \
             a runtime path. Add a match arm in spawn_and_prompt."
        )),
    }
}

/// `FeatureNotCompiled` stub for the `llama-cpp` adapter. The in-process
/// llama.cpp loop (the `cosmon-llama` / `cosmon-llama-sys` crates and the
/// `cosmon-provider` `llama` feature) was removed in the pre-publication
/// scope trim (delib-20260622-187a B1 / ADR-126). The `llama-cpp` row stays
/// in the dispatch registry (so `validate_adapter_name` never confuses
/// *unknown adapter* with *not compiled*); reaching this arm emits the typed
/// `FeatureNotCompiled` diagnostic ADR-100 R2 names. A Rust-native local-model
/// path for the "local-first autonomy" story will be reconsidered separately.
#[allow(clippy::too_many_arguments)]
fn spawn_llama_session(
    _wid: &cosmon_core::id::WorkerId,
    _session_name: &str,
    _worktree_path: &std::path::Path,
    _prompt: &str,
    _mol: &MoleculeData,
    _mol_state_dir: &std::path::Path,
    _adapter_entry: Option<&AdapterEntry>,
    _preferred_model: Option<&str>,
) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "cs tackle: the in-process `--adapter llama-cpp` loop was removed in \
         the cosmon scope trim (ADR-126); no local llama.cpp adapter ships in \
         this build. Use `--adapter ollama` for a local OpenAI-compatible \
         endpoint, or another configured adapter."
    ))
}

/// Does `adapter` rely on tmux for its worker process and pane-died
/// supervision?
///
/// `claude` and `aider` spawn a real tmux session that cosmon supervises
/// through the kernel-level `pane-died` hook. `openai` and `anthropic`
/// are **Direct-API** adapters (ADR-100 R2) — the agent loop runs
/// in-process inside the `cs tackle` invocation, returns a synthesis
/// synchronously, and never creates a tmux session. The sentinel
/// socket `openai-inprocess` / `anthropic-inprocess` lives in the
/// `WorkerSpawnAttempted` envelope as honest evidence that no tmux is
/// involved.
///
/// This predicate is the structural gate for every tmux-postulated
/// step of the post-spawn pipeline:
/// - [`install_harvest_hook`] (only invoked for tmux-backed adapters)
/// - the post-spawn liveness re-check (`backend.is_alive` + readiness
///   probe) at the `tackle` call site
///
/// The pre-fix tmux-everywhere assumption was inscribed when Claude
/// Code was the only citizen of `spawn_and_prompt`; ADR-100 R2 broke
/// the symmetry and this helper makes the asymmetry typed at the
/// adapter boundary. The longer-term move (cosmon-ward GAP #3) is to
/// promote the decision into a `SupervisionMode` enum stored on
/// [`ValidatedAdapterName`] so the compiler — not a `match` on a
/// string — forces each branch of the pipeline to handle both modes.
/// See chronicle `2026-05-18-supervision-mode-tactical-gap1.md`.
pub(super) fn adapter_uses_tmux(adapter: &ValidatedAdapterName) -> bool {
    // `codex` is the third tmux-pane Adapter (delib-20260518-5178 §S7).
    // Per §D4 of that synthesis it deliberately reuses
    // `SupervisionMode::TmuxPane` rather than introducing a new
    // variant — so the answer here is also `true`. `opencode` is the
    // fourth, on the same footing (delib-20260615-73f9 / ADR-125,
    // task-20260615-556a) — an external CLI in a pane, supervised through
    // `pane-died`, not a Direct-API in-process loop.
    matches!(adapter.as_str(), "claude" | "aider" | "codex" | "opencode")
}

/// Does this adapter finish its agent loop before `cs tackle` returns?
///
/// `local` used to be included here by accident because every non-tmux
/// adapter was synchronous. It now has a detached worker transport, so only
/// the two direct API adapters retain the inline completion contract.
fn adapter_completes_inline(adapter: &ValidatedAdapterName) -> bool {
    matches!(adapter.as_str(), "openai" | "anthropic")
}

/// Drive the canonical Running → Completed transition for an in-process
/// Direct-API molecule once `spawn_and_prompt` has returned Ok.
///
/// **Pattern divergence from tmux adapters** — for `claude` / `aider`,
/// the `pane-died` hook installed by [`install_harvest_hook`] is the
/// async witness that eventually exec's `cs harvest`, which observes
/// the worker's self-completion (via `cs complete`) and then triggers
/// `cs done`. For `openai` / `anthropic` (Direct-API, ADR-100 R2), the
/// agent loop runs **inside** the `cs tackle` invocation: there is no
/// pane to die, no async exit signal, and no `cs complete` invoked by
/// the worker. Without an explicit emit at the spawn-call site, the
/// molecule sits indefinitely in `Running` and the entire pipeline
/// downstream of `cs wait` stalls (academy GAP #6 → #7 → #8 cascade).
///
/// The new contract this helper inscribes: **in-process `spawn_and_prompt`
/// owns the completion emit.** The canonical sequence —
/// Running→Completed status flip, `MoleculeStatusChanged` and
/// `MoleculeCompleted` events on events.jsonl, log.md append, briefing.md
/// rewrite, proof-of-work seal — is implemented exactly once in
/// [`super::complete::complete_one`]; we delegate to it verbatim so the
/// in-process completion is byte-identical to a manual `cs complete`.
///
/// Errors from `complete_one` are propagated so the operator sees
/// completion-emit failures immediately rather than discovering them
/// hours later via a stuck `cs wait`. The fleet lock has already been
/// released by the caller — `complete_one` re-acquires it for its own
/// load → save cycle.
///
/// See an internal chronicle for the L9
/// rationale and the failure mode this prevents.
pub(super) fn finalize_inprocess_molecule(
    store: &FileStore,
    state_dir: &Path,
    mol_id: &MoleculeId,
    adapter: &ValidatedAdapterName,
) -> anyhow::Result<()> {
    let reason = format!(
        "in-process agent loop returned Ok ({} adapter, ADR-100 Direct-API)",
        adapter.as_str()
    );
    super::complete::complete_one(store, state_dir, mol_id, &reason).map(|_| ())
}

/// Claude branch of [`spawn_and_prompt`] — the historical path.
#[allow(clippy::too_many_arguments)]
fn spawn_claude_and_prompt(
    backend: &TmuxBackend,
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    permission_mode_override: Option<&str>,
    mol: &MoleculeData,
    mol_state_dir: &std::path::Path,
    // The chain-resolved model pin (delib-20260704-b476 C1), or `None` for
    // "no pin — the adapter's own default applies". Fed to
    // `resolve_worker_model` as the `preferred` model to pre-flight, then
    // injected through the `ANTHROPIC_MODEL` closure-shadow at spawn.
    preferred_model: Option<&str>,
    // The adapter's operator-declared strong cost-class set, threaded into
    // the probe-fallback layer so a cheap pin never silently escalates to a
    // strong model (task-20260705-ba98).
    strong_set: &[String],
) -> anyhow::Result<()> {
    use cosmon_transport::readiness::{ClaudeTuiProbe, LiveProbe, Liveness};
    let claude_bin = which_claude().unwrap_or_else(|| "claude".to_owned());
    let perm_mode = permission_mode_override.unwrap_or(default_permission_mode(mol));
    // Inject COSMON_MOL_DIR so the worker process knows the molecule state
    // directory without needing to call `cs observe`. Also inject
    // COSMON_PARENT_MOL_ID so any `cs nucleate` the worker issues can
    // auto-attach a DecayedFrom edge back to this molecule — this is the
    // structural enforcement of the lineage-conservation contract: a worker
    // that forgets `--blocked-by`/`--decayed-from` no longer orphans its
    // children because the env layer picks up the slack.
    //
    // CLAUDE_CONFIG_DIR (when set in the operator's shell) is propagated
    // through the same env-prefix mechanism so multi-account drivers like
    // `claude-account` / `pizzaiolo` can pin each worker to a specific
    // OAuth identity. The tmux server captures its env at startup and
    // hides every later shell override from `new-session`, so without an
    // explicit prefix here the variable is silently dropped. See
    // [`cosmon_cli::tackle_env::build_claude_command`] for the assembly
    // rules and [`cosmon_transport::tmux`] for why tmux behaves this way.
    let mol_dir_str = mol_state_dir.to_string_lossy();
    let parent_id_str = mol.id.as_str();

    // Resolve the Claude account config dir ONCE (it may advance the
    // round-robin balancer via `cb next`). The same value is used to
    // probe the model AND to launch the worker, so the worker runs under
    // the account we just probed, and the balancer is advanced exactly
    // once per spawn.
    let config_dir = cosmon_cli::tackle_env::resolve_claude_config_dir(
        cosmon_cli::tackle_env::run_cb_next,
        &|k| std::env::var(k).ok(),
    );

    // Model fallback chain (task-20260614-3116). The preferred model
    // (`ANTHROPIC_MODEL`, exported by the rpp-adapter from the `rpp.toml`
    // `claude_model` pin, default `claude-fable-5`) is no longer trusted
    // blindly: when the instance's Claude account has lost access to it,
    // the worker `claude` CLI does NOT exit on `model_not_found` — it
    // sits idle forever (a false-active worker the liveness probe cannot
    // tell apart from a slow one). We pre-flight the chain here and spawn
    // on the first model that actually answers; if none do, we fail fast
    // with a cause instead of spawning a doomed session. `effective_model`
    // is `Some(chosen)` when a pin resolved, or `None` on operator opt-out
    // (`claude_model = ""`), which preserves today's no-pin behaviour.
    let effective_model = resolve_worker_model(
        preferred_model,
        &claude_bin,
        mol_state_dir,
        config_dir.as_deref(),
        strong_set,
    )?;

    let claude_cmd = cosmon_cli::tackle_env::build_claude_command(
        &mol_dir_str,
        parent_id_str,
        &claude_bin,
        perm_mode,
        // The account was already resolved above; do not call `cb next`
        // a second time (it would double-advance the balancer).
        || None,
        // Feed back the already-resolved config dir and the probe-selected
        // model; every other variable falls through to the real env.
        |k| match k {
            "ANTHROPIC_MODEL" => effective_model.clone(),
            "CLAUDE_CONFIG_DIR" => config_dir.clone(),
            other => std::env::var(other).ok(),
        },
    );

    backend.spawn_worker(session_name, &worktree_path.to_string_lossy(), &claude_cmd)?;

    // First stage: a side-effect-free liveness poll (the substrate-agnostic
    // `poll_until_live` driver over `ClaudeTuiProbe`). Replaces the pre-fix
    // 1s blind sleep. The fix for task-4046 is to demand EVIDENCE that the
    // worker actually started, not just the absence of a tmux spawn error.
    // The driver requires at least one `Liveness::Live` observation within
    // the postcondition window — any of {Loading, TrustPrompt, Ready,
    // Working, Blocked} proves the process printed something a live claude
    // would print. If the window expires with only Dead / Indeterminate, we
    // kill the carcass session and bail; the operator gets the truth.
    if let Err(e) = observe_spawn_postcondition(backend, wid) {
        maybe_terminate(backend, wid);
        return Err(e);
    }

    // Second stage: block until the worker is alive and accepting work via
    // the substrate-agnostic `LiveProbe` contract (task-20260426-d781). The
    // Claude TUI's trust/permission-prompt handshake lives behind
    // `ClaudeTuiProbe::await_live` (it delegates to `wait_ready`); this call
    // site only knows `Live` vs not-`Live`. Before the task-4046 fix the
    // result was discarded (`let _status = ...`) — a classic surface-lie
    // pattern: success was inferred from the absence of a returned error,
    // not from the presence of observed liveness. We now match on the
    // verdict. Dead / Indeterminate-on-timeout / a probe error all tear down
    // the partial tmux state and surface a diagnostic pointing the operator
    // at `tmux -L <socket> capture-pane` so they can see what the session
    // actually said.
    let probe = ClaudeTuiProbe;
    match probe.await_live(
        backend,
        wid,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(500),
    ) {
        Ok(Liveness::Live) => {}
        Ok(Liveness::Dead) => {
            maybe_terminate(backend, wid);
            return Err(anyhow::anyhow!(
                "cs tackle: claude session {session_name} died during startup; \
                 no worker registered. Inspect with \
                 `tmux -L {} capture-pane -pS - -t {session_name}` \
                 (set COSMON_SPAWN_NO_TEARDOWN=1 to keep the carcass)",
                backend.socket()
            ));
        }
        Ok(Liveness::Indeterminate) => {
            maybe_terminate(backend, wid);
            return Err(anyhow::anyhow!(
                "cs tackle: claude session {session_name} did not reach a \
                 known state within 30s (status=unknown). Likely the binary \
                 failed to start or printed nothing recognisable. Inspect \
                 with `tmux -L {} capture-pane -pS - -t {session_name}` \
                 then retry with --force \
                 (set COSMON_SPAWN_NO_TEARDOWN=1 to keep the carcass)",
                backend.socket()
            ));
        }
        Err(e) => {
            maybe_terminate(backend, wid);
            return Err(anyhow::anyhow!(
                "cs tackle: readiness wait failed for {session_name}: {e}. \
                 Inspect with `tmux -L {} capture-pane -pS - -t {session_name}`",
                backend.socket()
            ));
        }
    }

    backend.send_input(wid, prompt)?;
    Ok(())
}

/// Per-model timeout for the pre-flight availability probe.
///
/// The probe runs `claude -p` (print mode, one turn) which makes a single
/// API round-trip and exits. A model that is unreachable either errors
/// quickly (→ non-zero exit) or — the false-active symptom this whole fix
/// exists for — *hangs*. We bound it: a probe that has not finished within
/// this window is killed and treated as unavailable, so a hanging model
/// can never be selected (the very failure we are guarding against).
const MODEL_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// Resolve the effective model for a claude worker by pre-flighting the
/// fallback chain, or fail fast when no model in the chain answers
/// Returns `Ok(Some(model))` for the probe-selected model, `Ok(None)`
/// when the operator opted out of pinning (`claude_model = ""` → no
/// `ANTHROPIC_MODEL` in the env, preserve today's no-pin behaviour), or
/// `Err` when a pin was requested but the entire chain probed unreachable
/// — in which case the caller must NOT spawn (the worker would freeze).
///
/// The `preferred` model is the chain-resolved pin from
/// [`resolve_model_selection`] (delib-20260704-b476 C1) — `--model` flag,
/// formula-step pin, `$COSMON_DEFAULT_MODEL` / `$ANTHROPIC_MODEL`, or a
/// config `default_model`, in precedence order — or `None` when nothing
/// pinned a model (the floor, byte-identical to today's no-pin path). Before
/// C1 this was read inline from `$ANTHROPIC_MODEL`; the resolution chain now
/// feeds it as a parameter so a per-molecule `--model` never mutates shared
/// session state (strong is never inherited).
///
/// Rollback hatch: `COSMON_MODEL_FALLBACK=0` skips probing entirely and
/// passes the preferred model through verbatim (pre-fix behaviour), for
/// the case where the probe itself is the problem on some host.
fn resolve_worker_model(
    preferred: Option<&str>,
    claude_bin: &str,
    mol_state_dir: &std::path::Path,
    config_dir: Option<&str>,
    // The adapter's operator-declared strong cost-class set
    // (`[adapters.claude].strong`, delib-20260704-b476 T1). Folded into
    // cosmon's intrinsic `DEFAULT_STRONG_MODELS` so the probe-fallback tail
    // never silently escalates a cheap pin to a strong model — the
    // `task-20260705-ba98` fix. Empty when none is declared (fail-open).
    strong_set: &[String],
) -> anyhow::Result<Option<String>> {
    use cosmon_core::model_chain::{decide_worker_model, DecidedModel};

    // Rollback hatch — pass the preferred model through unprobed.
    if std::env::var("COSMON_MODEL_FALLBACK").as_deref() == Ok("0") {
        return Ok(preferred
            .filter(|v| !v.is_empty())
            .map(std::borrow::ToOwned::to_owned));
    }

    // Probe with the same Claude account the worker will run under, so
    // the verdict reflects the worker's real auth path.
    let decided = decide_worker_model(preferred, strong_set, |model| {
        probe_claude_model(claude_bin, model, config_dir)
    });

    match decided {
        Ok(DecidedModel::OptOut) => Ok(None),
        Ok(DecidedModel::Selected { model, selection }) => {
            record_model_selection(
                mol_state_dir,
                &serde_json::json!({
                    "outcome": "selected",
                    "chosen": model,
                    "probes": selection.probes,
                }),
            );
            if selection.probes.len() > 1 {
                eprintln!(
                    "cs tackle: preferred model unavailable; fell back to \
                     `{model}` (model-fallback fix task-20260614-3116). \
                     See {}/model-selection.json",
                    mol_state_dir.display()
                );
            }
            Ok(Some(model))
        }
        Err(no_model) => {
            record_model_selection(
                mol_state_dir,
                &serde_json::json!({
                    "outcome": "no-model-available",
                    "chosen": serde_json::Value::Null,
                    "probes": no_model.probed,
                }),
            );
            Err(anyhow::anyhow!(
                "cs tackle: {no_model}. Refusing to spawn a worker that would \
                 freeze on `model_not_found` (model-fallback fix \
                 task-20260614-3116); the instance's Claude account has no \
                 reachable model. Fix account access or set `claude_model` in \
                 rpp.toml, then retry. Trail: {}/model-selection.json",
                mol_state_dir.display()
            ))
        }
    }
}

/// Persist the model-selection audit trail to the molecule state dir for
/// operator observability. Best-effort: a write failure must not abort a
/// spawn that is otherwise fine, so the error is logged, not propagated.
fn record_model_selection(mol_state_dir: &std::path::Path, value: &serde_json::Value) {
    let path = mol_state_dir.join("model-selection.json");
    let body = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    if let Err(e) = std::fs::write(&path, body) {
        eprintln!(
            "cs tackle: could not write model-selection trail to {}: {e}",
            path.display()
        );
    }
}

/// Probe whether `model` is usable by the worker's `claude` CLI.
///
/// Runs `claude --model <model> -p <trivial-prompt>` (print mode: one
/// turn, then exit) under the resolved `CLAUDE_CONFIG_DIR`, bounded by
/// [`MODEL_PROBE_TIMEOUT`]. Verdict:
/// - exit 0 → `ProbeOutcome::Available`;
/// - non-zero exit → unavailable, carrying the trimmed stderr tail as
///   the cause (e.g. the `model_not_found` message);
/// - timeout (killed) → unavailable, the false-active symptom itself;
/// - spawn failure (binary missing) → unavailable, carrying the io error.
///
/// This is the production prober; the selection logic is independently
/// unit-tested in `cosmon-core::model_chain` with an injected mock.
fn probe_claude_model(
    claude_bin: &str,
    model: &str,
    config_dir: Option<&str>,
) -> cosmon_core::model_chain::ProbeOutcome {
    use cosmon_core::model_chain::ProbeOutcome;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(claude_bin);
    cmd.arg("--model")
        .arg(model)
        .arg("-p")
        .arg("ping")
        .env("ANTHROPIC_MODEL", model)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(dir) = config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ProbeOutcome::Unavailable(format!("probe spawn failed: {e}")),
    };

    // std has no wait-with-timeout; poll try_wait until the deadline.
    let deadline = std::time::Instant::now() + MODEL_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return ProbeOutcome::Available;
                }
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut s| {
                        use std::io::Read as _;
                        let mut buf = String::new();
                        let _ = s.read_to_string(&mut buf);
                        buf
                    })
                    .unwrap_or_default();
                let tail: String = stderr.trim().chars().rev().take(200).collect::<String>();
                let tail: String = tail.chars().rev().collect();
                let detail = if tail.is_empty() {
                    format!("exit {status}")
                } else {
                    format!("exit {status}: {tail}")
                };
                return ProbeOutcome::Unavailable(detail);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ProbeOutcome::Unavailable(format!(
                        "probe timed out after {}s (model did not answer — the \
                         false-active symptom)",
                        MODEL_PROBE_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return ProbeOutcome::Unavailable(format!("probe wait failed: {e}")),
        }
    }
}

/// Aider branch of [`spawn_and_prompt`] (ADR-097 / C8).
///
/// Aider's CLI is fundamentally different from Claude's TUI — it
/// accepts the prompt as an `aider --message '<prompt>'` argument at
/// invocation time, so there is no bracketed-paste / second-Enter
/// dance to drive.
///
/// # Readiness
///
/// Liveness is asserted through the **same** substrate-agnostic
/// `LiveProbe` contract the Claude path uses — here the aider
/// implementation [`cosmon_transport::readiness::AiderProbe`], which
/// demands *evidence* aider actually printed its banner (or settled on
/// its `>` REPL prompt) before declaring the worker live. This replaces
/// the bespoke `2s` / `is_alive` loop that merely checked the tmux
/// session existed — a check that passes even for an `[exited]` carcass
/// pane, the surface lie. Routing through `await_live` means
/// `cs tackle` now waits for the real postcondition — *"aider answered
/// and is ready for input"* — for every adapter, and the aider prompt
/// is no longer fired at a REPL that never came up (the paste-stall at
/// `tmux.rs:441`).
#[allow(clippy::too_many_arguments)]
fn spawn_aider_and_prompt(
    backend: &TmuxBackend,
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    mol: &MoleculeData,
    adapter_entry: Option<&AdapterEntry>,
    preferred_model: Option<&str>,
) -> anyhow::Result<()> {
    use cosmon_transport::aider;
    use cosmon_transport::readiness::LiveProbe as _;

    // Project the molecule's effective clearance through the
    // Aider-specific flag bundle (see
    // [`cosmon_transport::aider::AiderPermissionFlags`] — `Write` is
    // the safe default; `cs resurrect` keeps Claude's flow).
    let clearance = aider_clearance_for(mol);
    let config = aider::session_config(
        backend.socket(),
        session_name,
        worktree_path,
        clearance,
        aider_model(adapter_entry, preferred_model),
        Some(prompt.to_owned()),
    );

    aider::spawn_aider_session(&config)
        .map_err(|e| anyhow::anyhow!("cs tackle: aider spawn failed: {e}"))?;

    // Postcondition: demand evidence aider actually came up, via the same
    // `LiveProbe` contract the Claude path uses (task-20260607-3345 / B5).
    // `AiderProbe::observe` returns `Live` only on aider's own banner / REPL
    // prompt — never from the mere existence of the tmux session — so an
    // `[exited]` carcass pane (binary missing, crash on launch) is caught
    // here instead of letting the prompt fire into a dead REPL. The default
    // `await_live` polls without perturbing the worker; aider needs no
    // prompt-answering handshake (`--yes-always` auto-confirms). A single
    // one-shot inline observation loop, no background watcher (godel's Q6).
    let probe = cosmon_transport::readiness::AiderProbe;
    match probe.await_live(
        backend,
        wid,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(200),
    ) {
        Ok(cosmon_transport::readiness::Liveness::Live) => Ok(()),
        Ok(other) => {
            let _ = backend.terminate(wid);
            Err(anyhow::anyhow!(
                "cs tackle: aider session {session_name} never produced live \
                 output within 30s (last verdict={other}). Treating as a failed \
                 spawn; tearing down. Inspect with \
                 `tmux -L {} capture-pane -pS - -t {session_name}`",
                backend.socket()
            ))
        }
        Err(e) => {
            let _ = backend.terminate(wid);
            Err(anyhow::anyhow!(
                "cs tackle: aider readiness wait failed for {session_name}: {e}. \
                 Inspect with `tmux -L {} capture-pane -pS - -t {session_name}`",
                backend.socket()
            ))
        }
    }
}

/// Codex branch of [`spawn_and_prompt`].
///
/// codex is `OpenAI`'s external CLI — a Node.js wrapper around a native
/// binary. Like aider it is **not** a Direct-API in-process adapter: it
/// runs its own agent loop inside a tmux pane and cosmon supervises the
/// pane, not the loop (see the 2026-05-19 codex-adapter chronicle,
/// *"codex — la soupape, pas le réacteur"*). The pane's cwd (set by
/// `tmux new-session -c <worktree>`) is the working directory, so no
/// `--cd` flag is needed.
///
/// # Interactive by default (task-20260711-246d)
///
/// The default mode is now `codex::CodexMode::Interactive` — the
/// steerable TUI, **parity with the claude adapter**: the pane stays open
/// after the task, the worker is driveable by `cs whisper`, and the prompt
/// is injected into the composer *after* readiness (mirroring
/// [`spawn_claude_and_prompt`]'s paste-then-Enter dance), which also
/// submits it. `[adapters.codex].mode = "exec"` selects the legacy
/// non-interactive `codex exec '<prompt>'` batch mode, where the prompt is
/// baked into the command and no injection happens.
///
/// # Readiness
///
/// Liveness is asserted through the **same** substrate-agnostic
/// `LiveProbe` contract every adapter uses — here
/// [`cosmon_transport::readiness::CodexProbe`], which demands *evidence*
/// codex actually printed its banner (the interactive TUI banner and the
/// `exec` preamble both name codex) before declaring the worker live. This
/// is the surface-lie guard applied to codex: an `[exited]` carcass pane
/// (binary missing on PATH, crash on launch) is caught here instead of the
/// prompt firing into a dead pane.
#[allow(clippy::too_many_arguments)]
fn spawn_codex_and_prompt(
    backend: &TmuxBackend,
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    _mol: &MoleculeData,
    adapter_entry: Option<&AdapterEntry>,
    preferred_model: Option<&str>,
) -> anyhow::Result<()> {
    use cosmon_transport::codex;
    use cosmon_transport::readiness::LiveProbe as _;

    // Resolve the launch mode and interactive flags from `[adapters.codex]`.
    // Absent config → the interactive default (steerable, whisperable).
    let mode = adapter_entry.and_then(|e| e.mode.as_deref()).map_or(
        codex::CodexMode::Interactive,
        codex::CodexMode::from_config_str,
    );
    let extra_args = adapter_entry
        .map(|e| e.extra_args.clone())
        .unwrap_or_default();

    // Resolve the operator git identity to pin on the codex worker
    // (delib-20260717-194b, F3). codex runs its own git process out of cosmon's
    // reach, and env beats per-worktree `git config` (F2), so we thread the
    // operator identity through as `GIT_AUTHOR_*` / `GIT_COMMITTER_*`. The
    // worktree shares the repo config, so resolving from it yields the same
    // effective identity `git commit` would use. `None` on a bare checkout with
    // no configured identity — the command then stays byte-identical.
    let git_identity = match (
        git_config_value(worktree_path, "user.name"),
        git_config_value(worktree_path, "user.email"),
    ) {
        (Some(name), Some(email)) => Some(codex::GitIdentity { name, email }),
        _ => None,
    };

    // codex is resolved by bare name; the tmux pane's shell resolves it on
    // PATH at exec time, the same contract `preflight::adapter_binary`
    // already checks ("codex" present on PATH). An absent binary surfaces
    // here as an `[exited]` pane and is caught by the readiness probe below.
    let config = codex::CodexSessionConfig {
        socket: backend.socket().to_owned(),
        session_name: session_name.to_owned(),
        work_dir: worktree_path.to_string_lossy().into_owned(),
        binary: std::path::PathBuf::from("codex"),
        prompt: Some(prompt.to_owned()),
        mode,
        model: preferred_model.map(str::to_owned),
        extra_args,
        telemetry: None,
        pre_existing_worker: None,
        git_identity,
    };

    codex::spawn_codex_session(&config)
        .map_err(|e| anyhow::anyhow!("cs tackle: codex spawn failed: {e}"))?;

    // Postcondition: demand evidence codex actually came up, via the same
    // `LiveProbe` contract the claude/aider paths use. `CodexProbe::observe`
    // returns `Live` only on codex's own banner / preamble — never from the
    // mere existence of the tmux session — so an `[exited]` carcass pane is
    // caught here. A single one-shot inline observation loop, no background
    // watcher (godel's Q6).
    let probe = cosmon_transport::readiness::CodexProbe;
    match probe.await_live(
        backend,
        wid,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(200),
    ) {
        Ok(cosmon_transport::readiness::Liveness::Live) => {}
        Ok(other) => {
            let _ = backend.terminate(wid);
            return Err(anyhow::anyhow!(
                "cs tackle: codex session {session_name} never produced live \
                 output within 30s (last verdict={other}). Treating as a failed \
                 spawn; tearing down. Inspect with \
                 `tmux -L {} capture-pane -pS - -t {session_name}`",
                backend.socket()
            ));
        }
        Err(e) => {
            let _ = backend.terminate(wid);
            return Err(anyhow::anyhow!(
                "cs tackle: codex readiness wait failed for {session_name}: {e}. \
                 Inspect with `tmux -L {} capture-pane -pS - -t {session_name}`",
                backend.socket()
            ));
        }
    }

    // Interactive mode: inject the prompt into the TUI composer, exactly as
    // the claude branch does. `codex exec` already baked the prompt into the
    // command line, so nothing is injected there.
    if mode == codex::CodexMode::Interactive {
        backend.send_input(wid, prompt)?;
    }

    Ok(())
}

/// opencode branch of [`spawn_and_prompt`].
///
/// opencode (sst/opencode) is the external-CLI sibling of codex — a
/// coding-agent binary on PATH driven in its non-interactive automation
/// mode, `opencode run '<prompt>'` (the counterpart of `codex exec`). Like
/// codex it runs its own agent loop inside a tmux pane and cosmon supervises
/// the pane, not the loop (ADR-125). The pane's cwd
/// (set by `tmux new-session -c <worktree>`) is the working directory, so no
/// `--cwd` flag is needed.
///
/// This helper is a near-clone of [`spawn_codex_and_prompt`] — the
/// template the opencode arm was scoped to copy.
///
/// # Readiness
///
/// Liveness is asserted through the **same** substrate-agnostic
/// `LiveProbe` contract every adapter uses — here
/// [`cosmon_transport::readiness::OpencodeProbe`], which demands *evidence*
/// opencode actually printed its run preamble before declaring the worker
/// live. This is the surface-lie guard applied to opencode: an
/// `[exited]` carcass pane (binary missing on PATH, crash on launch) is
/// caught here instead of the prompt firing into a dead pane.
fn spawn_opencode_and_prompt(
    backend: &TmuxBackend,
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    _mol: &MoleculeData,
) -> anyhow::Result<()> {
    use cosmon_transport::opencode;
    use cosmon_transport::readiness::LiveProbe as _;

    // opencode is resolved by bare name; the tmux pane's shell resolves it on
    // PATH at exec time, the same contract `preflight::adapter_binary`
    // already checks ("opencode" present on PATH). An absent binary surfaces
    // here as an `[exited]` pane and is caught by the readiness probe below.
    let config = opencode::OpencodeSessionConfig {
        socket: backend.socket().to_owned(),
        session_name: session_name.to_owned(),
        work_dir: worktree_path.to_string_lossy().into_owned(),
        binary: std::path::PathBuf::from("opencode"),
        prompt: Some(prompt.to_owned()),
        telemetry: None,
        pre_existing_worker: None,
    };

    opencode::spawn_opencode_session(&config)
        .map_err(|e| anyhow::anyhow!("cs tackle: opencode spawn failed: {e}"))?;

    // Postcondition: demand evidence opencode actually came up, via the same
    // `LiveProbe` contract the claude/aider/codex paths use.
    // `OpencodeProbe::observe` returns `Live` only on opencode's own run
    // preamble — never from the mere existence of the tmux session — so an
    // `[exited]` carcass pane is caught here. A single one-shot inline
    // observation loop, no background watcher (godel's Q6).
    let probe = cosmon_transport::readiness::OpencodeProbe;
    match probe.await_live(
        backend,
        wid,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_millis(200),
    ) {
        Ok(cosmon_transport::readiness::Liveness::Live) => Ok(()),
        Ok(other) => {
            let _ = backend.terminate(wid);
            Err(anyhow::anyhow!(
                "cs tackle: opencode session {session_name} never produced live \
                 output within 30s (last verdict={other}). Treating as a failed \
                 spawn; tearing down. Inspect with \
                 `tmux -L {} capture-pane -pS - -t {session_name}`",
                backend.socket()
            ))
        }
        Err(e) => {
            let _ = backend.terminate(wid);
            Err(anyhow::anyhow!(
                "cs tackle: opencode readiness wait failed for {session_name}: {e}. \
                 Inspect with `tmux -L {} capture-pane -pS - -t {session_name}`",
                backend.socket()
            ))
        }
    }
}

/// Map a molecule's posture to an Aider [`Clearance`].
///
/// The Adapter dispatch path defaults to `Write` (matching the
/// historical `register_tackle_worker` choice). A future ADR will
/// move this into [`MoleculeData`] once kind-driven clearance lands.
fn aider_clearance_for(_mol: &MoleculeData) -> Clearance {
    Clearance::Execute
}

/// Resolve the model identifier passed to `aider --model`, applying the
/// same precedence as the `openai` Direct-API branch
/// (see [`spawn_openai_session`]): **`--model` pin >
/// `.cosmon/config.toml` > env > compile-time default**.
///
/// 0. **Pin.** The chain-resolved `preferred_model` (delib-20260704-b476
///    C1) — a `cs tackle --model` flag or a formula-step `model =` pin —
///    is the top tier so a per-molecule choice wins over the galaxy
///    default (adapter-uniform with the claude carrier). Carried opaquely;
///    an invalid `(aider, model)` pair is aider's to reject at launch.
/// 1. **Config.** `[adapters.aider].default_model` in `.cosmon/config.toml`
///    is authoritative. This is the row that lets an operator point the
///    terminal-REPL aider co-pilot at Mistral (or any model) **without
///    recompiling** — closing the gap chronicled when the hard-coded
///    `kimi-k2.6` silently ignored the config (the C6 TOML loader the old
///    `aider_default_model` comment promised).
/// 2. **Env.** Aider's own native `AIDER_MODEL` env var, kept as the
///    middle tier so a shell-scoped override still works when no config
///    row is present (mirrors `OPENAI_MODEL` in the openai branch).
/// 3. **Compile-time default.** [`aider_default_model`] (`kimi-k2.6`),
///    matching the historical `AiderAdapter::default_for_dispatch` choice.
fn aider_model(adapter_entry: Option<&AdapterEntry>, preferred_model: Option<&str>) -> String {
    preferred_model
        .filter(|s| !s.is_empty())
        .map(std::borrow::ToOwned::to_owned)
        .or_else(|| adapter_entry.and_then(|e| e.default_model.clone()))
        .or_else(|| std::env::var("AIDER_MODEL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| aider_default_model().to_owned())
}

/// Compile-time default model identifier passed to `aider --model` when
/// neither `[adapters.aider].default_model` nor `AIDER_MODEL` is set.
/// Mirrors the `AiderAdapter::default_for_dispatch` choice (`kimi-k2.6`).
fn aider_default_model() -> &'static str {
    "kimi-k2.6"
}

/// `OpenAI` branch of [`spawn_and_prompt`] — the first **Direct-API** path
/// (ADR-100 R2 wave 2). Unlike `claude` / `aider`, no tmux session is
/// created: the in-process agent loop runs to completion inside this call.
///
/// # Adapter-config precedence (ADR-097 / C6, academy GAP #4)
///
/// API key, base URL, and model identifier all follow the same three-tier
/// resolution order: **`.cosmon/config.toml` > env > compile-time defaults**.
///
/// 1. **API key.** If `[adapters.openai].api_key_env = "XAI_API_KEY"` is
///    declared, that single env var is the authoritative source. Otherwise
///    the historical scan applies — first non-empty of `OPENAI_API_KEY`,
///    `XAI_API_KEY`, `MOONSHOT_API_KEY` wins.
/// 2. **Base URL.** `[adapters.openai].base_url` wins; otherwise the
///    `OPENAI_BASE_URL` env var; otherwise the vendor default
///    `https://api.openai.com` (with the `XAI_API_KEY` / `MOONSHOT_API_KEY`
///    fallbacks contributing their hard-coded vendor URLs only in the
///    env-scan path).
/// 3. **Model.** `[adapters.openai].default_model` > `OPENAI_MODEL` env >
///    `gpt-4o-mini`.
///
/// The structural reason the config tier is first: a free-rider build
/// (`openai`-named Adapter routed to xAI/Moonshot/DeepSeek via `base_url`)
/// is otherwise vulnerable to the "first vendor key in the shell wins
/// silently" trap — a request meant for `api.x.ai` could leak to
/// `api.openai.com` with a Grok model identifier, producing a 404. The
/// config row makes the binding authoritative.
#[allow(clippy::too_many_arguments)]
fn spawn_openai_session(
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    mol: &MoleculeData,
    mol_state_dir: &std::path::Path,
    adapter_entry: Option<&AdapterEntry>,
    preferred_model: Option<&str>,
) -> anyhow::Result<()> {
    let (api_key, base_url) = openai_credentials(adapter_entry).ok_or_else(|| {
        anyhow::anyhow!(
            "cs tackle: --adapter openai requires one of OPENAI_API_KEY / \
             XAI_API_KEY / MOONSHOT_API_KEY to be set in the environment \
             (or [adapters.openai].api_key_env in .cosmon/config.toml)"
        )
    })?;
    // `--model` / formula-pin (delib-20260704-b476 C1) is the top tier,
    // above the config `default_model`, so a per-molecule pin wins.
    let model = preferred_model
        .filter(|s| !s.is_empty())
        .map(std::borrow::ToOwned::to_owned)
        .or_else(|| adapter_entry.and_then(|e| e.default_model.clone()))
        .or_else(|| std::env::var("OPENAI_MODEL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "gpt-4o-mini".to_owned());

    let provider = if let Some(url) = base_url {
        cosmon_provider::OpenAIProvider::with_base_url(api_key, model, url)
    } else {
        cosmon_provider::OpenAIProvider::new(api_key, model)
    };

    // Emit WorkerSpawnAttempted before the loop so the cat-test sees the
    // intent even if the HTTP call never lands.
    let invocation_uuid = format!(
        "openai-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let telemetry = cosmon_provider::openai::telemetry_for(
        mol.id.clone(),
        wid.clone(),
        mol_state_dir.to_owned(),
        invocation_uuid,
    );
    let cfg = cosmon_transport::spawn::SpawnConfig {
        socket: "openai-inprocess".into(),
        session_name: session_name.to_owned(),
        work_dir: worktree_path.to_string_lossy().into_owned(),
        clearance: cosmon_core::clearance::Clearance::Execute,
        prompt: Some(prompt.to_owned()),
        telemetry: Some(telemetry.clone()),
        pre_existing_worker: None,
    };
    provider
        .spawn(&cfg)
        .map_err(|e| anyhow::anyhow!("cs tackle: openai spawn-event emission failed: {e}"))?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("cs tackle: tokio runtime build failed: {e}"))?;
    let synthesis = rt
        .block_on(cosmon_provider::openai::run_agent_loop(
            &provider,
            prompt,
            worktree_path,
            Some(&telemetry),
        ))
        .map_err(|e| anyhow::anyhow!("cs tackle: openai agent loop failed: {e}"))?;

    tracing::info!(
        target: "cosmon::tackle::openai",
        molecule = %mol.id.as_str(),
        session = session_name,
        bytes = synthesis.len(),
        "openai in-process agent loop completed"
    );
    Ok(())
}

/// Default Ollama OpenAI-compat host root. The harness loop appends
/// `/v1/chat/completions` (see `OpenAIProvider::one_turn`), so the
/// host root is the bare `http://localhost:11434` — *not* the `…/v1`
/// form. Override via `[adapters.local].base_url`,
/// `COSMON_LOCAL_BASE_URL`, or `OPENAI_BASE_URL`.
const DEFAULT_LOCAL_BASE_URL: &str = "http://localhost:11434";

/// Default local model served by Ollama. Chosen because it returns
/// *structured* `tool_calls` (not text) on the `/v1/chat/completions`
/// envelope — measured against the live Ollama install. By contrast
/// `qwen2.5-coder:7b` emitted the tool call as plain `content` and is
/// therefore NOT a safe default. Override via
/// `[adapters.local].default_model` or `COSMON_LOCAL_MODEL`.
const DEFAULT_LOCAL_MODEL: &str = "qwen3:8b";

/// Resolve the `local` / `ollama` floor's base URL: config `base_url` →
/// `COSMON_LOCAL_BASE_URL` → `OPENAI_BASE_URL` → compile-time Ollama
/// default.
///
/// Extracted from [`run_local_agent_loop`] (task-20260719-f45b) so the
/// **dispatch-side preflight and the worker resolve the same endpoint**.
/// A preflight that probed a different URL than the worker later dialled
/// would be worse than no preflight at all: it would certify a host the
/// work never touches.
fn resolve_local_base_url(adapter_entry: Option<&AdapterEntry>) -> String {
    adapter_entry
        .and_then(|e| e.base_url.clone())
        .or_else(|| {
            std::env::var("COSMON_LOCAL_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_LOCAL_BASE_URL.to_owned())
}

/// Resolve the *effective* model the `local` / `ollama` floor will run:
/// chain-resolved `--model` pin → config `default_model` →
/// `COSMON_LOCAL_MODEL` → [`DEFAULT_LOCAL_MODEL`].
///
/// Note the return type is `String`, not `Option<String>`: by the time
/// the floor spawns, *some* concrete model is always chosen. This is why
/// "no model was selected" (`preferred_model == None`) is **not** the
/// fault condition it looks like — the floor is documented to mean "let
/// the adapter pick its own default", and it does. The real fault is a
/// model the backend cannot serve, which is what
/// [`preflight_local_adapter_model`] checks.
///
/// Extracted from [`run_local_agent_loop`] (task-20260719-f45b) so the
/// preflight and the worker cannot drift apart.
fn resolve_local_model(
    preferred_model: Option<&str>,
    adapter_entry: Option<&AdapterEntry>,
) -> String {
    preferred_model
        .filter(|s| !s.is_empty())
        .map(std::borrow::ToOwned::to_owned)
        .or_else(|| adapter_entry.and_then(|e| e.default_model.clone()))
        .or_else(|| {
            std::env::var("COSMON_LOCAL_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_LOCAL_MODEL.to_owned())
}

/// Escape hatch for [`preflight_local_adapter_model`]. Set to `1` to
/// dispatch without proving the backend can serve the model.
///
/// Deliberately narrow: it skips the *check*, never weakens it. An
/// operator who sets this is choosing to risk the collapse the preflight
/// exists to prevent.
const SKIP_PREFLIGHT_ENV: &str = "COSMON_SKIP_ADAPTER_PREFLIGHT";

/// Wall-clock budget for the preflight probe. Short: this sits on the
/// dispatch path, and an unreachable backend must surface fast rather
/// than stalling the operator's terminal.
const PREFLIGHT_TIMEOUT_SECS: u64 = 3;

/// Why a dispatch to the `local` / `ollama` floor was refused before any
/// molecule state changed (task-20260719-f45b).
///
/// Typed rather than a bare string so the caller can render a diagnostic
/// that names the *repair* — the two failures need different fixes
/// (start the daemon vs. pull the model), and a molecule refused for the
/// wrong stated reason wastes the operator's next move.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalPreflightError {
    /// The backend did not answer. The work never had a chance to run.
    Unreachable { base_url: String, detail: String },
    /// The backend answered, but does not serve the resolved model.
    /// `available` is what it *does* serve — empty means a bare daemon
    /// with nothing pulled, which is its own distinct diagnosis.
    ModelNotServed {
        base_url: String,
        model: String,
        available: Vec<String>,
    },
}

impl std::fmt::Display for LocalPreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable { base_url, detail } => write!(
                f,
                "refusing to dispatch: the local adapter's backend at {base_url} \
                 is not reachable ({detail}). Start it (`ollama serve`) or point \
                 the adapter elsewhere with [adapters.local].base_url / \
                 COSMON_LOCAL_BASE_URL. The molecule is untouched and still \
                 tacklable — nothing was spawned and nothing collapsed."
            ),
            Self::ModelNotServed {
                base_url,
                model,
                available,
            } => {
                let served = if available.is_empty() {
                    "it serves no models at all — none have been pulled".to_owned()
                } else {
                    format!("it serves: {}", available.join(", "))
                };
                write!(
                    f,
                    "refusing to dispatch: the local adapter resolved to model \
                     '{model}', but the backend at {base_url} cannot serve it — \
                     {served}. Pull it (`ollama pull {model}`) or pin one that \
                     exists via --model / [adapters.local].default_model / \
                     COSMON_LOCAL_MODEL. The molecule is untouched and still \
                     tacklable — nothing was spawned and nothing collapsed."
                )
            }
        }
    }
}

/// Ollama's OpenAI-compat `/v1/models` envelope.
///
/// `data` is `Option<Vec<_>>`, not `Vec<_>`: a freshly-installed Ollama
/// with nothing pulled answers `{"object":"list","data":null}`, and a
/// non-optional field makes that *parse error* — which would misreport
/// the exact empty-daemon case this preflight exists to catch as an
/// unreachable backend. Observed live on 2026-07-20.
#[derive(serde::Deserialize)]
struct PreflightModelsResponse {
    #[serde(default)]
    data: Option<Vec<PreflightModelEntry>>,
}

#[derive(serde::Deserialize)]
struct PreflightModelEntry {
    id: String,
}

/// Prove the `local` / `ollama` backend can actually serve `model`
/// before a molecule is committed to it (task-20260719-f45b, ASK 2).
///
/// This is the guard whose absence let two molecules die: cosmon spawned
/// a worker against a reachable-but-empty Ollama, the worker asked for a
/// model that was never pulled, died in ~30 s, and the patrol collapsed
/// the molecule — a *terminal* state, so the work was lost and had to be
/// re-nucleated by hand.
///
/// Fail-closed by design: an unreachable backend refuses just as an
/// unservable model does. Both mean the work cannot run, and refusing a
/// dispatch is strictly recoverable where a collapse is not.
///
/// Probes `/v1/models` (the OpenAI-compat surface) rather than Ollama's
/// native `/api/tags`, because `/v1` is the surface the worker itself
/// dials — proving the endpoint the work will actually use.
fn preflight_local_adapter_model(
    base_url: &str,
    model: &str,
    timeout: std::time::Duration,
) -> Result<(), LocalPreflightError> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Err(LocalPreflightError::Unreachable {
                base_url: base_url.to_owned(),
                detail: format!("http client build failed: {e}"),
            })
        }
    };

    let resp = client
        .get(&url)
        .send()
        .map_err(|e| LocalPreflightError::Unreachable {
            base_url: base_url.to_owned(),
            detail: format!("{e}"),
        })?;

    if !resp.status().is_success() {
        return Err(LocalPreflightError::Unreachable {
            base_url: base_url.to_owned(),
            detail: format!("HTTP {}", resp.status()),
        });
    }

    let parsed: PreflightModelsResponse =
        resp.json().map_err(|e| LocalPreflightError::Unreachable {
            base_url: base_url.to_owned(),
            detail: format!("unreadable /v1/models response: {e}"),
        })?;

    let available: Vec<String> = parsed
        .data
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.id)
        .collect();

    if available.iter().any(|m| m == model) {
        return Ok(());
    }

    Err(LocalPreflightError::ModelNotServed {
        base_url: base_url.to_owned(),
        model: model.to_owned(),
        available,
    })
}

/// Default per-request HTTP timeout, in **seconds**, for the `local` /
/// `ollama` floor (task-20260707-7d27, academy banc Mode C, hole #3).
///
/// [`OpenAIProvider`](cosmon_provider::OpenAIProvider) hard-codes 60 s in
/// its constructors — far too short for a single-GPU oracle where a cold
/// 120 B load costs minutes and a warm reasoning-model turn or a queued
/// request routinely exceeds 60 s. The floor overrides that with this
/// 10-minute default: generous enough to absorb a cold big-model load,
/// still finite so a genuinely wedged daemon eventually surfaces SF-1
/// rather than hanging the worker forever. Override with
/// `[adapters.<name>].timeout_secs` or `COSMON_LOCAL_TIMEOUT`.
const DEFAULT_LOCAL_TIMEOUT_SECS: u64 = 600;

/// Resolve the `local` / `ollama` floor's per-request HTTP timeout, in
/// seconds (task-20260707-7d27, hole #3).
///
/// Precedence: `[adapters.<name>].timeout_secs` (`cfg`) →
/// `COSMON_LOCAL_TIMEOUT` (`env`, parsed as a positive integer number of
/// seconds) → [`DEFAULT_LOCAL_TIMEOUT_SECS`]. A non-numeric, empty, or
/// zero env value is ignored (falls through to the default) rather than
/// silently disabling the timeout — a zero timeout on `reqwest` means
/// *no* timeout, which would re-open the "hang forever" failure mode this
/// fix exists to bound.
///
/// Pure over its two inputs so the precedence is unit-testable without
/// touching the process environment.
fn resolve_local_timeout_secs(cfg: Option<u64>, env: Option<&str>) -> u64 {
    cfg.filter(|&s| s > 0)
        .or_else(|| {
            env.and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|&s| s > 0)
        })
        .unwrap_or(DEFAULT_LOCAL_TIMEOUT_SECS)
}

/// `local` branch of [`spawn_and_prompt`] — the walking-skeleton
/// local-default Adapter.
///
/// # Why this reuses `OpenAIProvider` (the R-openai route)
///
/// The deliberation's synthesis ranked three routes to a multi-turn
/// local loop on a cost/generality curve and chose **R-openai**: point
/// the already-proven `openai` Adapter at Ollama's OpenAI-compat `/v1`
/// endpoint. Ollama serves `/v1/chat/completions` with native
/// structured `tool_calls`, so the existing
/// [`cosmon_agent_harness::run_loop`] spine — multi-turn, tool
/// dispatch, the four loop invariants — drives the local model with
/// **zero new provider code**. carnot's "irreversible loss to refuse"
/// was writing a bespoke `OllamaProvider: Provider`; this branch
/// honours that by reusing the openai envelope verbatim.
///
/// # Credentials
///
/// Ollama ignores the bearer token on its OpenAI-compat endpoint, so a
/// sentinel `"ollama"` key is injected — no `OPENAI_API_KEY` is
/// required (that is the whole point: ZERO Claude Code, ZERO cloud
/// key, in the default path). The base URL and model are resolved
/// config → env → compile-time default.
///
/// # Synthesis artefact
///
/// Unlike [`spawn_openai_session`] (which discards the returned text),
/// this branch persists `synthesis.md` to the molecule's state dir —
/// mirroring [`spawn_llama_session`] — so the molecule's proof-of-work
/// trail carries the local model's output and the smoke test's
/// `test -s <mol_dir>/synthesis.md` passes.
///
/// The loop deliberately runs in a separate session. A local model can take
/// minutes on a real task; tying that lifetime to the caller makes an RPP
/// tackle request wait for inference and eventually time out. A separate
/// process group cuts the tie to the one-shot CLI while the normal parent path
/// immediately records the Running molecule and its worker.
#[allow(clippy::too_many_arguments)]
fn spawn_detached_local_worker(
    adapter_name: &str,
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    mol: &MoleculeData,
    mol_state_dir: &std::path::Path,
    state_dir: &std::path::Path,
    adapter_entry: Option<&AdapterEntry>,
    preferred_model: Option<&str>,
) -> anyhow::Result<()> {
    let job = LocalWorkerJob {
        adapter_name: adapter_name.to_owned(),
        worker_id: wid.as_str().to_owned(),
        session_name: session_name.to_owned(),
        worktree_path: worktree_path.to_owned(),
        prompt: prompt.to_owned(),
        molecule_id: mol.id.as_str().to_owned(),
        molecule_dir: mol_state_dir.to_owned(),
        state_dir: state_dir.to_owned(),
        adapter_entry: adapter_entry.cloned(),
        preferred_model: preferred_model.map(str::to_owned),
    };
    let job_path = mol_state_dir.join("local-worker-job.json");
    let bytes = serde_json::to_vec(&job)
        .map_err(|e| anyhow::anyhow!("cs tackle: could not serialize local worker job: {e}"))?;
    fs::write(&job_path, bytes).map_err(|e| {
        anyhow::anyhow!(
            "cs tackle: could not write detached local worker job {}: {e}",
            job_path.display()
        )
    })?;

    let executable = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("cs tackle: could not resolve current cs executable: {e}"))?;
    let log_path = mol_state_dir.join("local-worker.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| anyhow::anyhow!("cs tackle: could not open {}: {e}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|e| anyhow::anyhow!("cs tackle: could not clone local worker log handle: {e}"))?;

    let mut command = ProcessCommand::new(executable);
    command
        .arg("local-worker")
        .arg("--job")
        .arg(&job_path)
        .current_dir(worktree_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("cs tackle: could not detach local worker: {e}"))?;

    // The detached process is the ground-truth witness for the RPP tackle
    // ceiling. Persist its PID after spawn so a worker that crashes before it
    // can report its own terminal transition cannot occupy a slot forever.
    record_detached_local_worker_pid(
        state_dir,
        &mol.id,
        wid,
        child.id(),
        process_start_time(child.id()),
    )?;

    Ok(())
}

/// Persist the PID of a detached local worker without reviving a record that
/// has already reached a terminal status.
fn record_detached_local_worker_pid(
    state_dir: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
    pid: u32,
    pid_start_time: Option<u64>,
) -> anyhow::Result<()> {
    let store = FileStore::new(state_dir);
    let _guard = store.lock_fleet()?;
    let mut molecule = store.load_molecule(mol_id)?;
    if let Some(process) = molecule
        .process
        .as_mut()
        .filter(|p| p.worker_id == *worker_id)
    {
        if process.is_active() {
            process.pid = Some(pid);
            process.pid_start_time = pid_start_time;
            store.save_molecule(mol_id, &molecule)?;
        }
    }
    Ok(())
}

/// Mark a detached local worker terminal once its process has returned.
///
/// The molecule's completion and the worker's lifecycle are distinct: the
/// former records formula progress, while this status retracts the live-worker
/// claim used by the RPP tackle ceiling. Keeping the terminal record preserves
/// forensic identity without allowing it to consume a concurrent-worker slot.
fn mark_detached_local_worker_stopped(
    store: &FileStore,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
) -> anyhow::Result<()> {
    let _guard = store.lock_fleet()?;
    let mut molecule = store.load_molecule(mol_id)?;
    if let Some(process) = molecule
        .process
        .as_mut()
        .filter(|p| p.worker_id == *worker_id)
    {
        process.status = WorkerStatus::Stopped;
        store.save_molecule(mol_id, &molecule)?;
    }
    Ok(())
}

/// Entry point for the detached local worker. It is intentionally not a
/// second tackle path: all dispatch choices are frozen in [`LocalWorkerJob`],
/// and this process only owns inference plus the terminal lifecycle emit.
pub fn run_local_worker(args: &LocalWorkerArgs) -> anyhow::Result<()> {
    let body = fs::read(&args.job).map_err(|e| {
        anyhow::anyhow!(
            "cs local-worker: could not read {}: {e}",
            args.job.display()
        )
    })?;
    let job: LocalWorkerJob = serde_json::from_slice(&body)
        .map_err(|e| anyhow::anyhow!("cs local-worker: invalid job {}: {e}", args.job.display()))?;
    let store = FileStore::new(&job.state_dir);
    let mol_id: MoleculeId = job
        .molecule_id
        .parse()
        .map_err(|e| anyhow::anyhow!("cs local-worker: invalid molecule id: {e}"))?;
    let wid = WorkerId::new(&job.worker_id)?;
    let mol = store.load_molecule(&mol_id)?;

    let result = run_local_agent_loop(
        &job.adapter_name,
        &wid,
        &job.session_name,
        &job.worktree_path,
        &job.prompt,
        &mol,
        &job.molecule_dir,
        job.adapter_entry.as_ref(),
        job.preferred_model.as_deref(),
    );
    let synthesis = match result {
        Ok(synthesis) => synthesis,
        Err(error) => {
            let _ = append_local_worker_failure(&job.molecule_dir, &error);
            return match mark_detached_local_worker_stopped(&store, &mol_id, &wid) {
                Ok(()) => Err(error),
                Err(mark_error) => Err(error.context(format!(
                    "cs local-worker: additionally failed to mark worker {wid} stopped: {mark_error}"
                ))),
            };
        }
    };

    // Surface the deliverables the worker produced in its worktree as RPP
    // artifacts (under COSMON_ARTIFACT_DIR), deterministically — do NOT rely on
    // a weak local model honoring the formula's RESULT CONTRACT. This closes the
    // gap where `GET /v1/molecules/{id}/artifacts` returned nothing even though
    // the worker committed real code to the worktree (cosmon-ward b127).
    if let Err(error) = sync_worktree_deliverables_to_artifact_dir(&job.worktree_path) {
        let _ = append_local_worker_failure(&job.molecule_dir, &error);
        return match mark_detached_local_worker_stopped(&store, &mol_id, &wid) {
            Ok(()) => Err(error),
            Err(mark_error) => Err(error.context(format!(
                "cs local-worker: additionally failed to mark worker {wid} stopped: {mark_error}"
            ))),
        };
    }

    // BUG #4 real-work guard. The agent loop returning `Ok` proves only that
    // the transport did not error — NOT that the weak local model did any
    // work. A no-op turn (empty synthesis, no worktree edits) must NOT be
    // booked "completed": that is a silent false success, the exact wall
    // tester Matteo Cacciari hit (an unresolvable/weak ollama model no-ops and
    // the molecule "passes"). Refuse to finalize unless there is real work,
    // and fail LOUDLY with a repair-naming message. The molecule is left
    // Running with the worker stopped — recoverable and re-tacklable — rather
    // than collapsed (terminal, work lost) or completed (false success).
    if !local_worker_produced_real_work(&job.worktree_path, &synthesis) {
        let guard_error = anyhow::anyhow!(
            "cs local-worker: the local agent loop returned without producing any real work \
             — synthesis.md is empty and the worktree holds no deliverables. This is almost \
             always a weak or unresolved local model that no-opped instead of honoring the \
             result contract. The molecule is NOT completed; it needs attention. Re-tackle \
             with a model the backend can actually serve and reason with (pin one via \
             --model, [adapters.local].default_model, or COSMON_LOCAL_MODEL)."
        );
        let _ = append_local_worker_failure(&job.molecule_dir, &guard_error);
        return match mark_detached_local_worker_stopped(&store, &mol_id, &wid) {
            Ok(()) => Err(guard_error),
            Err(mark_error) => Err(guard_error.context(format!(
                "cs local-worker: additionally failed to mark worker {wid} stopped: {mark_error}"
            ))),
        };
    }

    mark_detached_local_worker_stopped(&store, &mol_id, &wid)?;

    let adapter =
        validate_adapter_name(&job.adapter_name, std::slice::from_ref(&job.adapter_name))?.0;
    finalize_inprocess_molecule(&store, &job.state_dir, &mol_id, &adapter)
}

/// Mirror the files a local worker produced in its worktree into the RPP
/// artifact directory (`$COSMON_ARTIFACT_DIR`), so a thin `cosmon-remote` client
/// can fetch them via `GET /v1/molecules/{id}/artifacts` without any out-of-band
/// shell access to the worktree.
///
/// "Produced" = tracked files that differ from the worktree's merge-base with
/// `main`, plus new untracked files. Git paths are read as NUL-delimited bytes.
/// Paths internal to cosmon (`.cosmon/`, `target/`, `.git/`) are skipped.
///
/// The artifact listing is flat, so each source path is encoded as
/// `path-<hex-encoded-path-bytes>`. This mapping is reversible and injective:
/// unlike separator replacement, no two distinct paths can overwrite each other.
/// The boundary accepts only regular files no larger than
/// [`MAX_SYNCED_ARTIFACT_BYTES`]; symlinks and every other special file are
/// rejected. Copy and discovery failures are returned so the local worker cannot
/// report success without publishing its promised artifacts.
///
/// A no-op when `$COSMON_ARTIFACT_DIR` is unset (non-RPP tackle).
fn sync_worktree_deliverables_to_artifact_dir(worktree: &Path) -> anyhow::Result<()> {
    let artifact_dir = match std::env::var("COSMON_ARTIFACT_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => return Ok(()),
    };
    sync_worktree_deliverables(worktree, &artifact_dir)
}

/// Whether a finished local worker produced *real work* worth booking as a
/// completed molecule (BUG #4).
///
/// The `local` / `ollama` floor drives a weak, operator-supplied model. Such a
/// model can return `Ok` from the agent loop having done nothing at all — no
/// edits, no synthesis, a single no-op turn — because the loop's success is the
/// *absence of a transport error*, not the *presence of output*. Booking that
/// "completed" is a silent false success: the molecule reports done while the
/// work never happened. This is the exact wall tester Matteo Cacciari hit —
/// unable to name a capable ollama model, the loop no-ops and the molecule
/// "passes".
///
/// Real work is either of:
/// * a non-empty `synthesis.md` body (the model's own final text), or
/// * at least one non-internal deliverable in the worktree (a tracked diff vs
///   `main` or an untracked file that is not under `.cosmon/`, `target/`,
///   `.git/`).
///
/// Fail-closed: if the worktree cannot even be inspected (git failure) *and*
/// the synthesis is empty, we cannot prove work happened, so we report `false`
/// and let the caller surface the molecule for attention rather than complete
/// it on faith.
fn local_worker_produced_real_work(worktree: &Path, synthesis: &str) -> bool {
    if !synthesis.trim().is_empty() {
        return true;
    }
    match discover_worktree_deliverables(worktree) {
        Ok(rels) => rels.iter().any(|rel| !ignored_artifact_path(rel)),
        Err(error) => {
            tracing::warn!(
                worktree = %worktree.display(),
                error = %error,
                "real-work guard could not inspect worktree deliverables; treating as no work"
            );
            false
        }
    }
}

/// Maximum artifact size accepted at the RPP boundary (16 MiB).
const MAX_SYNCED_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

/// Discover the paths a local worker produced in its worktree, as
/// git-native NUL-safe byte paths relative to the worktree root.
///
/// "Produced" = tracked files that differ from the worktree's merge-base with
/// `main`, plus new untracked files. Cosmon-internal paths (`.cosmon/`,
/// `target/`, `.git/`) are *not* filtered here — callers apply
/// [`ignored_artifact_path`] themselves so both the artifact-sync boundary and
/// the real-work guard share one discovery.
///
/// Shared by [`sync_worktree_deliverables`] (which publishes the paths) and
/// [`local_worker_produced_real_work`] (which only counts them), so the two
/// can never disagree about what the worker actually produced.
fn discover_worktree_deliverables(
    worktree: &Path,
) -> anyhow::Result<std::collections::BTreeSet<Vec<u8>>> {
    let mut rels = std::collections::BTreeSet::new();
    rels.extend(git_nul_paths(
        worktree,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?);
    match git_stdout(worktree, &["merge-base", "HEAD", "main"]) {
        Ok(base) => {
            let base = std::str::from_utf8(&base)
                .map_err(|_| anyhow::anyhow!("git merge-base returned non-UTF-8 object id"))?
                .trim();
            if base.is_empty() {
                anyhow::bail!("git merge-base returned an empty object id");
            }
            rels.extend(git_nul_paths(
                worktree,
                &["diff", "-z", "--name-only", "--diff-filter=ACMR", base],
            )?);
        }
        Err(error) if !rels.is_empty() => {
            // Fresh RPP galaxies can have a feature worktree but no `main`
            // ref yet. Untracked worker output is still a real deliverable and
            // must cross the artifact boundary; only tracked-diff discovery is
            // unavailable in this repository shape.
            tracing::warn!(
                worktree = %worktree.display(),
                error = %error,
                "artifact sync could not resolve main merge-base; publishing untracked deliverables"
            );
        }
        Err(error) => return Err(error),
    }
    Ok(rels)
}

fn sync_worktree_deliverables(worktree: &Path, artifact_dir: &Path) -> anyhow::Result<()> {
    let rels = discover_worktree_deliverables(worktree)?;
    if rels.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(artifact_dir).map_err(|error| {
        anyhow::anyhow!(
            "could not create artifact directory {}: {error}",
            artifact_dir.display()
        )
    })?;
    for rel in rels {
        if ignored_artifact_path(&rel) {
            continue;
        }
        let rel_path = git_path_to_path(&rel);
        if !rel_path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            anyhow::bail!("git reported an unsafe artifact path: {rel:?}");
        }
        let src = worktree.join(rel_path);
        let metadata = fs::symlink_metadata(&src)
            .map_err(|error| anyhow::anyhow!("could not lstat {}: {error}", src.display()))?;
        if !metadata.file_type().is_file() {
            anyhow::bail!("refusing non-regular worktree artifact {}", src.display());
        }
        if metadata.len() > MAX_SYNCED_ARTIFACT_BYTES {
            anyhow::bail!(
                "refusing artifact {} ({} bytes exceeds {} byte limit)",
                src.display(),
                metadata.len(),
                MAX_SYNCED_ARTIFACT_BYTES
            );
        }
        let dst = artifact_dir.join(artifact_filename(&rel));
        if let Ok(existing) = fs::symlink_metadata(&dst) {
            if existing.file_type().is_symlink() {
                anyhow::bail!("refusing symlink destination {}", dst.display());
            }
        }
        fs::copy(&src, &dst).map_err(|error| {
            anyhow::anyhow!(
                "could not copy {} to {}: {error}",
                src.display(),
                dst.display()
            )
        })?;
    }
    Ok(())
}

fn git_stdout(worktree: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = ProcessCommand::new("git")
        .current_dir(worktree)
        .args(args)
        .output()
        .map_err(|error| anyhow::anyhow!("could not run git {args:?}: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        anyhow::bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}

fn git_nul_paths(worktree: &Path, args: &[&str]) -> anyhow::Result<Vec<Vec<u8>>> {
    Ok(git_stdout(worktree, args)?
        .split(|byte| *byte == b'\0')
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn ignored_artifact_path(path: &[u8]) -> bool {
    matches!(path, b".cosmon" | b"target" | b".git")
        || path.starts_with(b".cosmon/")
        || path.starts_with(b"target/")
        || path.starts_with(b".git/")
}

fn artifact_filename(path: &[u8]) -> String {
    let mut encoded = String::with_capacity("path-".len() + path.len() * 2);
    encoded.push_str("path-");
    for byte in path {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(unix)]
fn git_path_to_path(path: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    PathBuf::from(std::ffi::OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn git_path_to_path(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).as_ref())
}

fn append_local_worker_failure(molecule_dir: &Path, error: &anyhow::Error) -> std::io::Result<()> {
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(molecule_dir.join("local-worker.log"))?;
    writeln!(log, "local worker failed: {error:#}")
}

#[allow(clippy::too_many_arguments)]
fn run_local_agent_loop(
    // The validated adapter name the operator selected — `local` (the
    // bare-tackle floor) or its `ollama` alias (task-20260707-7d27 hole #1).
    // Threaded through so telemetry stamps the *selected* name, not a
    // hard-coded floor constant, keeping the ADR-099 cat-test honest.
    adapter_name: &str,
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    mol: &MoleculeData,
    mol_state_dir: &std::path::Path,
    adapter_entry: Option<&AdapterEntry>,
    preferred_model: Option<&str>,
) -> anyhow::Result<String> {
    // base_url: config → COSMON_LOCAL_BASE_URL → OPENAI_BASE_URL →
    // compile-time Ollama default. The provider's `normalize_base_url`
    // strips a stray trailing `/v1` so either the host-root or the
    // vendor-doc `…/v1` form is accepted.
    //
    // Shared with the dispatch-side preflight (task-20260719-f45b) — see
    // `resolve_local_base_url` for why they must not drift.
    let base_url = resolve_local_base_url(adapter_entry);

    // model: --model pin → config → COSMON_LOCAL_MODEL → compile-time
    // default. The chain-resolved pin (delib-20260704-b476 C1) is top tier.
    let model = resolve_local_model(preferred_model, adapter_entry);

    // Per-request timeout (hole #3): config `timeout_secs` →
    // `COSMON_LOCAL_TIMEOUT` (seconds) → 10-minute floor default. Without
    // this override the provider's 60 s constructor timeout killed every
    // cold big-model load and most warm reasoning turns at exactly 60 s.
    let timeout_secs = resolve_local_timeout_secs(
        adapter_entry.and_then(|e| e.timeout_secs),
        std::env::var("COSMON_LOCAL_TIMEOUT").ok().as_deref(),
    );

    // Sentinel API key — Ollama's OpenAI-compat endpoint ignores the
    // bearer token. The provider redacts it on every Debug/Display
    // site (it is a `Secret`), so the sentinel never leaks to a log.
    let provider =
        cosmon_provider::OpenAIProvider::with_base_url("ollama", model.clone(), base_url.clone())
            .with_timeout(std::time::Duration::from_secs(timeout_secs));

    let invocation_uuid = format!(
        "local-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    // The `local` floor reuses `OpenAIProvider` against Ollama. Stamp the
    // provider-level IFBDD events (`WorkerSpawnAttempted`,
    // `AdapterLivenessProbed`) with the validated floor name, not the
    // provider's `"openai"` class constant — otherwise events.jsonl claims a
    // remote endpoint for a strictly-local run, breaching the ADR-099 cat-test
    // (`adapter_selected == worker_spawned`) on the DEFAULT dispatch path
    // (task-20260614-a63c, audit GAP #1). The name is the *selected* floor
    // name — `local` or the `ollama` alias (hole #1) — not a hard-coded
    // constant, so the alias path stamps `ollama` and the cat-test holds.
    let telemetry = cosmon_provider::openai::telemetry_for(
        mol.id.clone(),
        wid.clone(),
        mol_state_dir.to_owned(),
        invocation_uuid,
    )
    .with_adapter_name(adapter_name);
    let cfg = cosmon_transport::spawn::SpawnConfig {
        socket: "local-inprocess".into(),
        session_name: session_name.to_owned(),
        work_dir: worktree_path.to_string_lossy().into_owned(),
        clearance: cosmon_core::clearance::Clearance::Execute,
        prompt: Some(prompt.to_owned()),
        telemetry: Some(telemetry.clone()),
        pre_existing_worker: None,
    };
    provider
        .spawn(&cfg)
        .map_err(|e| anyhow::anyhow!("cs tackle: local spawn-event emission failed: {e}"))?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("cs tackle: tokio runtime build failed: {e}"))?;
    let synthesis = run_local_future_with_timeout(
        &rt,
            std::time::Duration::from_secs(timeout_secs),
            cosmon_provider::openai::run_local_sandboxed_agent_loop(
                &provider,
                prompt,
                worktree_path,
                Some(&telemetry),
            ),
        )
        .map_err(|_| anyhow::anyhow!(
            "cs tackle: local worker wall-clock limit ({timeout_secs}s) elapsed; worker was terminated"
        ))?
        .map_err(|e| {
            anyhow::anyhow!(
                "cs tackle: local agent loop failed (is `ollama serve` running at \
                 {base_url}, and does model `{model}` support /v1 tool-calling?): {e}"
            )
        })?;

    // Persist synthesis.md so the molecule's proof-of-work trail carries
    // the local model's output (mirrors spawn_llama_session). Best-effort:
    // a write failure must not undo a completed loop.
    let synthesis_path = mol_state_dir.join("synthesis.md");
    let body = format!(
        "# local synthesis\n\n\
         **Model**: `{model}` via `{base_url}` (Ollama OpenAI-compat)\n\n\
         ---\n\n\
         {synthesis}\n",
    );
    if let Err(e) = std::fs::write(&synthesis_path, &body) {
        tracing::warn!(
            target: "cosmon::tackle::local",
            error = %e,
            path = %synthesis_path.display(),
            "failed to write synthesis.md"
        );
    }

    tracing::info!(
        target: "cosmon::tackle::local",
        molecule = %mol.id.as_str(),
        session = session_name,
        bytes = synthesis.len(),
        "local in-process agent loop completed"
    );
    Ok(synthesis)
}

/// Run a local-worker future under its wall-clock limit inside the runtime that
/// owns the timer driver.
///
/// `tokio::time::timeout` binds to the current Tokio reactor when its future is
/// created. Detached local workers construct their runtime here, so creating
/// the timeout before `Runtime::block_on` panics instead of reaching Ollama.
fn run_local_future_with_timeout<F, T>(
    runtime: &tokio::runtime::Runtime,
    timeout: std::time::Duration,
    future: F,
) -> Result<T, tokio::time::error::Elapsed>
where
    F: Future<Output = T>,
{
    runtime.block_on(async move { tokio::time::timeout(timeout, future).await })
}

/// Resolve `OpenAI` credentials and optional base URL.
///
/// Three-tier precedence: **config > env > defaults** (academy GAP #4 /
/// docs/architectural-invariants.md §8n).
///
/// * When `adapter_entry.api_key_env` is set, that env var is the single
///   authoritative source. The historical multi-vendor scan is skipped
///   entirely — declaring the binding in `.cosmon/config.toml` shuts the
///   silent-leak trap closed.
/// * When `adapter_entry.api_key_env` is absent, the historical scan
///   applies: first non-empty of `OPENAI_API_KEY`, `XAI_API_KEY`,
///   `MOONSHOT_API_KEY` wins, each carrying its hard-coded vendor URL as
///   the env-tier fallback.
/// * `adapter_entry.base_url` (config tier) always wins over both the
///   `OPENAI_BASE_URL` env var (env tier) and the vendor default (env-scan
///   fallback / compile-time default).
fn openai_credentials(adapter_entry: Option<&AdapterEntry>) -> Option<(String, Option<String>)> {
    // Config-declared binding short-circuits the multi-vendor scan. The
    // operator named exactly which env var holds the credential; we honour
    // it and refuse to silently fall through to a sibling.
    if let Some(entry) = adapter_entry {
        if let Some(key_env) = entry.api_key_env.as_deref() {
            let key = std::env::var(key_env).ok().filter(|s| !s.is_empty())?;
            // config.base_url > env OPENAI_BASE_URL > None (provider default)
            let url = entry.base_url.clone().or_else(|| {
                std::env::var("OPENAI_BASE_URL")
                    .ok()
                    .filter(|s| !s.is_empty())
            });
            return Some((key, url));
        }
    }

    // Historical free-rider scan (pre-C6 behaviour, preserved when no
    // [adapters.openai].api_key_env is declared). First non-empty key
    // wins; each contributes its hard-coded vendor URL as a default.
    for (key_env, default_url) in [
        ("OPENAI_API_KEY", None),
        ("XAI_API_KEY", Some("https://api.x.ai")),
        ("MOONSHOT_API_KEY", Some("https://api.moonshot.ai")),
    ] {
        if let Ok(key) = std::env::var(key_env) {
            if !key.is_empty() {
                // config.base_url > env OPENAI_BASE_URL > vendor default
                let url = adapter_entry
                    .and_then(|e| e.base_url.clone())
                    .or_else(|| {
                        std::env::var("OPENAI_BASE_URL")
                            .ok()
                            .filter(|s| !s.is_empty())
                    })
                    .or_else(|| default_url.map(ToOwned::to_owned));
                return Some((key, url));
            }
        }
    }
    None
}

/// `Anthropic` branch of [`spawn_and_prompt`] — the second **Direct-API** path
/// (ADR-100 R2 wave 3). Mirrors
/// [`spawn_openai_session`] verbatim, swapping the `OpenAI` envelope for the
/// `Anthropic` `/v1/messages` envelope (header `x-api-key`, top-level `system`
/// field, `stop_reason` instead of `finish_reason`, `input_schema` instead of
/// `parameters`).
///
/// Why this exists as a distinct branch rather than a generic Direct-API
/// dispatcher: ADR-098 §6 trigger #3 (cat-test cross-Adapter) demands that
/// the existing claude.rs subprocess Adapter and this Anthropic Direct-API
/// Adapter produce convergent `events.jsonl` traces for an identical
/// briefing. Two independent code paths are the structural invariant; a
/// shared helper would erase the very divergence the cat-test exists to
/// detect.
///
/// # Adapter-config precedence (ADR-097 / C6, academy GAP #4)
///
/// Same three-tier order as [`spawn_openai_session`]: **config > env >
/// defaults**.
///
/// 1. `[adapters.anthropic].api_key_env` names the env var to read; absent
///    means `ANTHROPIC_API_KEY`.
/// 2. `[adapters.anthropic].base_url` > `ANTHROPIC_BASE_URL` > provider
///    default (`https://api.anthropic.com`).
/// 3. `[adapters.anthropic].default_model` > `ANTHROPIC_MODEL` >
///    [`crate::cmd::config::ANTHROPIC_DEFAULT_MODEL`].
#[allow(clippy::too_many_arguments)]
fn spawn_anthropic_session(
    wid: &cosmon_core::id::WorkerId,
    session_name: &str,
    worktree_path: &std::path::Path,
    prompt: &str,
    mol: &MoleculeData,
    mol_state_dir: &std::path::Path,
    adapter_entry: Option<&AdapterEntry>,
    preferred_model: Option<&str>,
) -> anyhow::Result<()> {
    let key_env = adapter_entry
        .and_then(|e| e.api_key_env.as_deref())
        .unwrap_or("ANTHROPIC_API_KEY");
    let api_key = std::env::var(key_env)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cs tackle: --adapter anthropic requires {key_env} to be set in the environment \
                 (or [adapters.anthropic].api_key_env in .cosmon/config.toml)"
            )
        })?;
    // `--model` / formula-pin (delib-20260704-b476 C1) tops the chain,
    // above the config `default_model`.
    let model = preferred_model
        .filter(|s| !s.is_empty())
        .map(std::borrow::ToOwned::to_owned)
        .or_else(|| adapter_entry.and_then(|e| e.default_model.clone()))
        .or_else(|| {
            std::env::var("ANTHROPIC_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| crate::cmd::config::ANTHROPIC_DEFAULT_MODEL.to_owned());
    let base_url = adapter_entry.and_then(|e| e.base_url.clone()).or_else(|| {
        std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
    });

    let provider = if let Some(url) = base_url {
        cosmon_provider::AnthropicProvider::with_base_url(api_key, model, url)
    } else {
        cosmon_provider::AnthropicProvider::new(api_key, model)
    };

    // Emit WorkerSpawnAttempted before the loop so the cat-test sees the
    // intent even if the HTTP call never lands.
    let invocation_uuid = format!(
        "anthropic-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    );
    let telemetry = cosmon_provider::anthropic::telemetry_for(
        mol.id.clone(),
        wid.clone(),
        mol_state_dir.to_owned(),
        invocation_uuid,
    );
    let cfg = cosmon_transport::spawn::SpawnConfig {
        socket: "anthropic-inprocess".into(),
        session_name: session_name.to_owned(),
        work_dir: worktree_path.to_string_lossy().into_owned(),
        clearance: cosmon_core::clearance::Clearance::Execute,
        prompt: Some(prompt.to_owned()),
        telemetry: Some(telemetry.clone()),
        pre_existing_worker: None,
    };
    provider
        .spawn(&cfg)
        .map_err(|e| anyhow::anyhow!("cs tackle: anthropic spawn-event emission failed: {e}"))?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("cs tackle: tokio runtime build failed: {e}"))?;
    let synthesis = rt
        .block_on(cosmon_provider::anthropic::run_agent_loop(
            &provider,
            prompt,
            worktree_path,
            Some(&telemetry),
        ))
        .map_err(|e| anyhow::anyhow!("cs tackle: anthropic agent loop failed: {e}"))?;

    tracing::info!(
        target: "cosmon::tackle::anthropic",
        molecule = %mol.id.as_str(),
        session = session_name,
        bytes = synthesis.len(),
        "anthropic in-process agent loop completed"
    );
    Ok(())
}

/// Install the harvest hook on `session_name` so `pane-died` triggers
/// `cs harvest` from a sibling shell rooted at `repo_root`.
///
/// Rooting the exec in the **main repo** (not the worktree) is the
/// structural enforcement of the `cs done = not-the-worker` invariant:
/// the shell the hook spawns has no cwd inheritance from the worker,
/// so the harvest (and the `cs done` it exec's) runs as a sibling of
/// the dying worker, never as the worker itself.
///
/// # Mandatory — ADR-052 child #4
///
/// Pre-ADR-052 this was best-effort: install errors were logged and
/// swallowed, and liveness was backstopped by polling (`cs patrol
/// --harvest`). That let the dfd8 / 192a class of ghosts slip through:
/// the hook silently failed to arm and nobody noticed until the morning
/// after. ADR-052 §D4 #5 retires polling as the primary mechanism and
/// promotes the pane-died hook to a hard precondition — if we cannot
/// install the event-driven witness, we refuse to report the molecule
/// as `Running`. The caller is expected to run [`cleanup_partial_tackle`]
/// and propagate the error to the operator.
///
/// The periodic `cs patrol --harvest` sweep survives as the belt-and-
/// suspenders for the residual class where the hook *did* install but
/// tmux lost it (server restart). It is no longer the primary signal.
///
/// # Errors
///
/// Returns the transport error from `install_pane_died_hook` verbatim
/// so the caller can log a diagnostic and tear down partial state.
pub(super) fn install_harvest_hook(
    backend: &cosmon_transport::TmuxBackend,
    session_name: &str,
    mol_id: &MoleculeId,
    repo_root: &std::path::Path,
) -> Result<(), cosmon_core::transport::TransportError> {
    let cs_bin = std::env::current_exe()
        .map_or_else(|_| "cs".to_owned(), |p| p.to_string_lossy().into_owned());
    // shell_quote is module-private to tmux.rs, so we do the quoting here
    // for values that flow into a single-quoted shell expression. Only
    // single quotes in the inputs matter; paths with spaces survive the
    // outer `run-shell '…'` wrapper.
    let safe_repo = repo_root.to_string_lossy().replace('\'', "'\\''");
    let safe_bin = cs_bin.replace('\'', "'\\''");
    let safe_mol = mol_id.as_str().replace('\'', "'\\''");
    // `#{pane_dead_status}` is a tmux format that expands to the exit
    // status of the dead pane in the hook context. When tmux cannot
    // supply it (older versions, unusual pane types) the literal string
    // appears in argv — `cs harvest` treats any unparseable value as
    // "no information" and emits `exit_code = None`.
    let command = format!(
        "cd '{safe_repo}' && '{safe_bin}' harvest --molecule '{safe_mol}' \
         --from-pane-died --exit-code '#{{pane_dead_status}}' \
         >/dev/null 2>&1 || true"
    );
    backend.install_pane_died_hook(session_name, &command)
}

/// Emit a forensic `EventV2::SF6SupervisionSetupFailed` receipt to
/// the local events.jsonl when a post-spawn supervision step fails
/// after the worker has already produced a real artefact on disk.
///
/// Used exclusively from the `cs tackle` post-spawn pipeline — at the
/// moment, only the `install_pane_died_hook` failure path. The receipt
/// names the adapter and the specific hook that failed, plus a
/// truncated copy of the underlying error, so a later operator audit
/// can attribute drift between expected supervision coverage and
/// observed reality without re-running the call.
///
/// Best-effort: any I/O or serialise failure is silently swallowed
/// (same `trace-not-lock` discipline as the briefing seal — telemetry
/// failure must never block the hot path). The caller logs a user-
/// facing warning regardless of whether this event lands.
///
/// The `error` field is truncated to 500 bytes to keep events.jsonl
/// row sizes bounded; a 500-byte tail captures the actionable
/// classification (tmux error class, errno) without bloating the log.
fn emit_supervision_setup_failed_event(
    mol_id: &MoleculeId,
    wid: &WorkerId,
    adapter_name: &str,
    hook_name: &str,
    error: &str,
) {
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");
    emit_supervision_setup_failed_event_to(
        &events_path,
        mol_id,
        wid,
        adapter_name,
        hook_name,
        error,
    );
}

/// Inner form of [`emit_supervision_setup_failed_event`] that takes the
/// events.jsonl path explicitly. Factored out so unit tests can point
/// at a temp directory without setting `COSMON_STATE_DIR` (env var
/// manipulation is global and racy under `cargo test --jobs N`).
///
/// `error` is truncated to 500 bytes — a 500-byte tail captures the
/// actionable classification (tmux error class, errno) without
/// bloating the events.jsonl row. The truncation is done on a UTF-8
/// boundary so the resulting string remains valid Rust `String`.
fn emit_supervision_setup_failed_event_to(
    events_path: &Path,
    mol_id: &MoleculeId,
    wid: &WorkerId,
    adapter_name: &str,
    hook_name: &str,
    error: &str,
) {
    let truncated_error = truncate_at_utf8_boundary(error, 500);
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        cosmon_core::event_v2::EventV2::SF6SupervisionSetupFailed {
            mol_id: mol_id.clone(),
            worker_id: wid.clone(),
            adapter_name: adapter_name.to_owned(),
            hook_name: hook_name.to_owned(),
            error: truncated_error,
        },
        None,
    );
}

/// Truncate `s` to at most `max_bytes` bytes, falling back to the
/// nearest preceding UTF-8 char boundary. Appends `…` (single
/// codepoint, 3 bytes UTF-8) when truncation occurred so an audit can
/// tell at a glance that the field was clipped.
fn truncate_at_utf8_boundary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = s[..cut].to_owned();
    out.push('…');
    out
}

/// Undo every side-effect `cs tackle` has taken on the filesystem before
/// the fleet write lands. Used on the error paths between
/// `create_worktree` and the `with_fleet_lock` block where the spawn
/// itself failed (`spawn_and_prompt` returned `Err`) or the post-spawn
/// liveness re-check found a dead worker — i.e. the molecule never
/// produced any usable artefact and the on-disk side-effects are
/// strictly orphan state the operator would have to clean up by hand.
/// Symmetry on the failed-spawn path: tackle either commits everything
/// or nothing.
///
/// **Not** invoked when `install_harvest_hook` fails post-spawn — the
/// worker is alive and its work is real; see
/// [`emit_supervision_setup_failed_event`] and the
/// `2026-05-18-cleanup-preserve-on-success.md` chronicle for the
/// preserve-on-success contract.
///
/// All calls are best-effort. A cleanup failure MUST NOT shadow the
/// original error the caller is about to return — the operator needs to
/// see the spawn/readiness diagnostic, not a branch-delete failure.
fn cleanup_partial_tackle(
    backend: &TmuxBackend,
    wid: &cosmon_core::id::WorkerId,
    repo_root: &Path,
    worktree_path: &Path,
    branch_name: &str,
    no_worktree: bool,
) {
    let _ = backend.terminate(wid);
    if no_worktree {
        return;
    }
    let _ = std::process::Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ])
        .output();
    let _ = std::process::Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "branch",
            "-D",
            branch_name,
        ])
        .output();
}

/// Default proof-of-life window for the spawn postcondition.
///
/// Widened from the historical 2 s to 12 s on 2026-06-02 after the
/// hosted tenant 503 was traced to a pure cold-start timing race: on a cold
/// container, claude's first TUI
/// frame — including the trust prompt, which **is** a recognised marker
/// (`readiness::TRUST_PROMPT_ALT`) — renders *after* 2 s, so the
/// postcondition timed out, tore the tmux session down, and surfaced as
/// `503 tackle_unavailable` at the adapter. The detector was never the
/// problem; the window was. The second-stage `wait_ready` budget is
/// already 30 s, so only this first proof-of-life gate was hardcoded
/// too tight. See ADR-093 (Alternative D, reversed by this evidence).
const DEFAULT_SPAWN_POSTCONDITION_SECS: u64 = 12;

/// Resolve the proof-of-life window from `COSMON_SPAWN_POSTCONDITION_SECS`,
/// falling back to [`DEFAULT_SPAWN_POSTCONDITION_SECS`].
///
/// A missing, empty, unparseable, or zero value yields the default —
/// the operator override can only *set* a positive window, never
/// disable the gate (a zero-length window would make every spawn fail).
/// `env_lookup` is injected so the resolution is unit-testable without
/// touching the process environment.
fn spawn_postcondition_window<F>(env_lookup: F) -> std::time::Duration
where
    F: Fn(&str) -> Option<String>,
{
    let secs = env_lookup("COSMON_SPAWN_POSTCONDITION_SECS")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_SPAWN_POSTCONDITION_SECS);
    std::time::Duration::from_secs(secs)
}

/// Whether a failed-spawn carcass should be **kept** for inspection
/// rather than torn down, controlled by `COSMON_SPAWN_NO_TEARDOWN`.
///
/// Debug affordance: when set to `1` / `true`,
/// `cs tackle` leaves the dead tmux pane in place on a spawn-postcondition
/// (or readiness) failure so the operator can run the `capture-pane`
/// hint already printed in the error message and see what the session
/// actually rendered. Off by default — production tear-down is the norm.
/// `env_lookup` is injected for the same testability reason as
/// [`spawn_postcondition_window`].
fn spawn_no_teardown<F>(env_lookup: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    env_lookup("COSMON_SPAWN_NO_TEARDOWN").is_some_and(|v| {
        let t = v.trim();
        t == "1" || t.eq_ignore_ascii_case("true")
    })
}

/// Tear down a failed-spawn carcass **unless** the operator asked to
/// keep it via `COSMON_SPAWN_NO_TEARDOWN` (see [`spawn_no_teardown`]).
///
/// Centralises the `terminate`-on-failure gesture so every claude-startup
/// failure path honours the debug affordance consistently.
fn maybe_terminate(backend: &TmuxBackend, wid: &cosmon_core::id::WorkerId) {
    if spawn_no_teardown(|k| std::env::var(k).ok()) {
        return;
    }
    let _ = backend.terminate(wid);
}

/// Poll the newly-spawned session at 200ms cadence within the proof-of-life
/// window ([`spawn_postcondition_window`], default
/// [`DEFAULT_SPAWN_POSTCONDITION_SECS`]) and require evidence that claude
/// actually printed something a live process would print.
///
/// The contract: we return `Ok(())` the first time we observe any of
/// `{Loading, TrustPrompt, Ready, Working, Blocked}`. We return `Err` if
/// the budget elapses with only `Dead` or `Unknown` observations.
///
/// Rationale: the pre-fix code slept 1s blindly and trusted
/// `spawn_claude`'s success as proof of a live worker. That is how the
/// surface lie happened — tmux spawned, claude exec failed silently
/// under `remain-on-exit`, the session became an `[exited]` carcass, and
/// the rest of tackle barrelled on to write `Running` to the surface.
/// This function is the structural counter-measure: it demands real
/// evidence before letting the caller proceed.
///
/// The window is env-configurable (`COSMON_SPAWN_POSTCONDITION_SECS`) and
/// defaults wide enough to clear a cold-container first-frame render —
/// see [`DEFAULT_SPAWN_POSTCONDITION_SECS`] for the tenant-demo
/// cold-start evidence that motivated widening it from 2 s.
fn observe_spawn_postcondition(
    backend: &TmuxBackend,
    wid: &cosmon_core::id::WorkerId,
) -> anyhow::Result<()> {
    use cosmon_transport::readiness::{poll_until_live, ClaudeTuiProbe, Liveness};
    // The postcondition is the substrate-agnostic "demand evidence of
    // liveness, no perturbation" poll (task-20260426-d781). The Claude-TUI
    // pane parse lives behind `ClaudeTuiProbe::observe`; this function only
    // owns the spawn-specific window and the operator-facing diagnostic.
    let window = spawn_postcondition_window(|k| std::env::var(k).ok());
    let probe = ClaudeTuiProbe;
    match poll_until_live(
        &probe,
        backend,
        wid,
        window,
        std::time::Duration::from_millis(200),
    ) {
        Ok(Liveness::Live) => Ok(()),
        // Dead / Indeterminate / a probe transport error all mean "no
        // evidence the worker came alive within the window" — the same
        // failed-spawn verdict the pre-refactor loop produced.
        Ok(other) => Err(anyhow::anyhow!(
            "cs tackle: spawn postcondition failed — session {} never \
             produced live-worker output within {}s (last verdict={other}). \
             Treating as a failed spawn; tearing down. Inspect with \
             `tmux -L {} capture-pane -pS - -t {}` (set \
             COSMON_SPAWN_NO_TEARDOWN=1 to keep the carcass; raise \
             COSMON_SPAWN_POSTCONDITION_SECS for slower cold starts)",
            wid.name(),
            window.as_secs(),
            backend.socket(),
            wid.name(),
        )),
        Err(e) => Err(anyhow::anyhow!(
            "cs tackle: spawn postcondition probe failed for session {}: {e}. \
             Inspect with `tmux -L {} capture-pane -pS - -t {}`",
            wid.name(),
            backend.socket(),
            wid.name(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the absolute path to the `claude` binary.
fn which_claude() -> Option<String> {
    std::process::Command::new("which")
        .arg("claude")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Default permission mode based on molecule kind.
fn default_permission_mode(_mol: &MoleculeData) -> &'static str {
    // All workers run in bypass mode for full autonomy.
    // The molecule kind and formula steps provide guardrails —
    // permission mode is not the right place to add friction.
    "bypassPermissions"
}

/// Resolve the absolute path to the global cosmon config,
/// `~/.config/cosmon/config.toml` (Q5a extension).
///
/// Honours `$COSMON_CONFIG_HOME` for test isolation, falling back to
/// `$HOME/.config` — the **same** convention as `security.toml` /
/// `galaxies.toml` / `daemons.toml` (`cs security`), deliberately
/// **not** `dirs::config_dir()` (which lands in
/// `~/Library/Application Support` on macOS) and **not**
/// [`cosmon_filestore::resolve_config_path`]'s `global_config_fallback`
/// (which points at `~/.cosmon/config.toml`, the walk-up terminus
/// reached *outside* a galaxy). This is the operator's machine-wide
/// adapter preference, a separate surface from the per-galaxy
/// `.cosmon/config.toml`.
fn global_adapter_config_path() -> PathBuf {
    let config_home = std::env::var_os("COSMON_CONFIG_HOME").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into())).join(".config"),
        PathBuf::from,
    );
    config_home.join("cosmon").join("config.toml")
}

/// Parse **only** the `[adapters]` section of the global config at `path`,
/// if the file exists and is well-formed (Q5a extension).
///
/// Best-effort by construction: a missing file, an I/O error, or a TOML
/// parse failure all yield `None` so the resolver falls through to the
/// built-in `"local"` floor — a malformed global config must never abort
/// dispatch. A bespoke `#[derive(Deserialize)]` struct (rather than the
/// full [`cosmon_core::config::ProjectConfig`]) is used so unrelated
/// global-config sections are ignored and the `[project]` table is not
/// required.
fn load_global_adapters(path: &Path) -> Option<AdaptersConfig> {
    #[derive(serde::Deserialize)]
    struct GlobalAdaptersOnly {
        #[serde(default)]
        adapters: Option<AdaptersConfig>,
    }
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: GlobalAdaptersOnly = toml::from_str(&text).ok()?;
    parsed.adapters
}

/// The **strong cost-class** set for `adapter_name` (delib-20260704-b476 C4),
/// unioned across the per-galaxy and global `[adapters.<name>].strong` rows.
///
/// Union (not per-galaxy-wins) is the fail-open-*and*-conservative choice: a
/// larger strong set classifies *more* models as expensive, which only ever
/// tightens the ceiling — the direction that protects the operator's credits.
/// An id declared strong in either scope is treated as strong.
fn adapter_strong_set(
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

/// Fold the fleet `events.jsonl` into the strong-dispatch records the ceiling
/// counts (delib-20260704-b476 C4) — the `cs reconcile` idiom, never a mutable
/// counter file.
///
/// Reads every persisted [`EventV2::ModelSelected`](cosmon_core::event_v2::EventV2::ModelSelected)
/// that pinned a concrete model (`model: Some`) and projects it to a
/// [`DispatchRecord`](cosmon_core::model_budget::DispatchRecord). The floor
/// (`model: None`) is never strong, so it is skipped. Best-effort by
/// construction: a missing or unreadable log yields an empty vec (the ceiling
/// then sees zero prior strong dispatches and honours the pin) — the count is
/// telemetry-derived and must never abort a spawn because the log is unhappy.
/// Fold the local `events.jsonl` for the strong-dispatch records the ceiling
/// counts, distinguishing a **trustworthy absence** from an **unreadable** log.
///
/// A genuinely absent log (`NotFound` — a fresh galaxy that has never
/// dispatched) is `Ok(empty)`: a real zero the ceiling may trust. A log that
/// *exists but cannot be read/parsed* is `Err(())`: the count is unknown, and
/// the caller must map it to [`cosmon_core::model_budget::LocalHistory::Unavailable`]
/// so the budget gate **fails closed** rather than treating unknown history as
/// zero (the `unreadable → empty` bug, C3 of `delib-20260711-c6c8`).
fn load_strong_dispatch_records(
    state_dir: &Path,
) -> Result<Vec<cosmon_core::model_budget::DispatchRecord>, ()> {
    let path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    let envelopes = match cosmon_state::event_log::read_all(&path) {
        Ok(envelopes) => envelopes,
        // A never-created log is a genuine zero, not an unreadable one.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        // The log exists but could not be read — unknown count, fail closed.
        Err(_) => return Err(()),
    };
    Ok(envelopes
        .into_iter()
        .filter_map(|env| match env.event {
            cosmon_core::event_v2::EventV2::ModelSelected {
                adapter_name,
                model: Some(model),
                selected_at,
                ..
            } => Some(cosmon_core::model_budget::DispatchRecord {
                adapter_name,
                model,
                selected_at,
            }),
            _ => None,
        })
        .collect())
}

/// Resolve the Worker-Spawn Port Adapter name for a `cs tackle`
/// invocation (ADR-097 / C6; ADR-108 Q5a chain).
///
/// Walks the six-level resolution chain documented in `Args::adapter`
/// (Q5a), highest priority first:
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
///   one constant (spelled exactly once, in `cosmon_core::config`), never
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
fn resolve_adapter_selection(
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

/// The model env tier of the resolution chain (delib-20260704-b476 C1):
/// `$COSMON_DEFAULT_MODEL` (the canonical name), else the legacy
/// `$ANTHROPIC_MODEL` (the carrier the rpp-adapter already exports from
/// `rpp.toml`'s `claude_model` pin, honoured for backward compatibility).
///
/// Returns `(value, var_name)` for the first set-and-non-empty var, or
/// `None`. Shared by `cs tackle`'s chain (which threads the var name into
/// the recorded source) and `cs resurrect` (which needs only the value, and
/// must keep honouring `$ANTHROPIC_MODEL` exactly as it did before C1 lifted
/// the inline env read out of `resolve_worker_model`).
pub(super) fn env_default_model() -> Option<(String, &'static str)> {
    std::env::var("COSMON_DEFAULT_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|v| (v, "COSMON_DEFAULT_MODEL"))
        .or_else(|| {
            std::env::var("ANTHROPIC_MODEL")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|v| (v, "ANTHROPIC_MODEL"))
        })
}

/// Resolve the per-molecule **model** pin for a `cs tackle` invocation
/// (delib-20260704-b476 C1) — the model sibling of
/// [`resolve_adapter_selection`], a verbatim shape-clone of its chain.
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
/// guard — that lands in C4 (the strong-cost-class set + the reconcile-check
/// that rejects a strong config-default). C1 must not itself wire a config
/// path that *silently defaults* to strong; here it does not — a config
/// `default_model` is only consulted when no positive per-molecule act
/// (flag / pin) fired, and the guard that rejects a strong value in that
/// slot is C4's job. The `ModelSelectionSource` is carried out verbatim so
/// C2's `ModelSelected` event and C4's guards can read the origin.
#[allow(clippy::too_many_arguments)]
fn resolve_model_selection(
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

/// Resolve the per-Adapter [`LoopOwnership`] axis (ADR-103).
///
/// Built-in names (`claude`, `aider`, `codex`, `openai`, `anthropic`)
/// take the validator's verdict verbatim — the
/// [`BUILT_IN_AXES`](cosmon_core::spawn_seam) table is the
/// authoritative source. TOML-only adapters (a `[adapters.<name>]`
/// row whose `<name>` is not built-in) may override the legacy
/// default by declaring `ownership = "cosmon"`; the absence-default
/// preserves the pre-ADR-103 `External` contract.
///
/// Unknown `ownership` strings fall back to the validator's verdict
/// with a stderr warning rather than aborting — `cs tackle` must
/// remain dispatch-tolerant of stale operator config.
fn resolve_loop_ownership(
    adapter_name: &str,
    from_validator: LoopOwnership,
    entry: Option<&AdapterEntry>,
) -> LoopOwnership {
    // Built-in adapters: the validator's axis table wins.
    if cosmon_core::spawn_seam::axes_for_built_in(adapter_name).is_some() {
        return from_validator;
    }
    // TOML-only adapter: read the row, fall back to the validator's
    // verdict (which is `External` for any caller-supplied name).
    match entry.and_then(|e| e.ownership.as_deref()) {
        Some("cosmon") => LoopOwnership::Cosmon,
        Some("external") | None => from_validator,
        Some(other) => {
            eprintln!(
                "cs tackle: warning — [adapters.{adapter_name}].ownership = {other:?} \
                 is not recognised ('external' or 'cosmon'); falling back to '{from_validator:?}'"
            );
            from_validator
        }
    }
}

// ---------------------------------------------------------------------------
// Stress-test seal gate (ADR-085 §M4 — Layer 1 + bypass receipt)
// ---------------------------------------------------------------------------

/// Refuse `cs tackle` of a stress-test molecule that lacks the
/// pre-commitment seal (Layer 1 of ADR-085 §Decision §2). When
/// `bypass_seal` is true, mint a typed
/// [`BypassReceipt`](cosmon_core::molecule_class::BypassReceipt) +
/// [`EventV2::SealBypassed`](cosmon_core::event_v2::EventV2::SealBypassed)
/// instead of refusing.
///
/// # Errors
///
/// - Refuses with [`anyhow::Error`] wrapping
///   [`cosmon_runtime::guard::SealGuardError::SealMissing`] when the
///   stress-test seal is incomplete and `--bypass-seal` was not passed.
/// - Refuses if `--bypass-seal` was passed without a non-empty
///   `--bypass-reason` (the receipt must record *why*).
fn enforce_stress_test_seal(
    store: &FileStore,
    state_dir: &Path,
    mol: &MoleculeData,
    bypass_seal: bool,
    bypass_reason: Option<&str>,
) -> anyhow::Result<()> {
    use cosmon_runtime::guard::{check_prior_seal, emit_seal_bypassed, SealGuardError};

    if !mol.class.requires_seal() {
        return Ok(());
    }

    let mol_dir = store.molecule_dir(&mol.id);
    let events_path = cosmon_state::event_log::resolve_events_log_path(state_dir);
    // `force = bypass_seal` makes the guard return the report instead of
    // a refusal error; we still need the report's `missing_condition` to
    // stamp into the BypassReceipt for forensic accountability.
    let report = match check_prior_seal(mol, &mol_dir, &events_path, bypass_seal) {
        Ok(r) => r,
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    if report.is_sealed() {
        return Ok(());
    }

    // Reaching here ⇒ bypass_seal was true and the seal is incomplete.
    let reason = bypass_reason
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--bypass-seal requires a non-empty --bypass-reason \"<…>\" \
                 (ADR-085 §3.5: silent overrides are forbidden)"
            )
        })?;

    let receipt = cosmon_core::molecule_class::BypassReceipt {
        molecule_id: mol.id.clone(),
        actor: "operator".to_owned(),
        reason: reason.to_owned(),
        bypassed_at: Utc::now(),
        frame_hash: hash_artifact(&mol_dir),
        bypassed_condition: report.missing_condition().to_owned(),
    };
    let receipt_json = serde_json::to_vec_pretty(&receipt)
        .map_err(|e| anyhow::anyhow!("failed to serialize BypassReceipt: {e}"))?;
    let receipt_path = mol_dir.join("bypass-receipt.json");
    std::fs::write(&receipt_path, &receipt_json)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", receipt_path.display()))?;
    let receipt_b3 = cosmon_hash::Hash::of_bytes(&receipt_json).to_hex();

    // Defensive: a write failure here would mean the operator sees
    // refusal even though they passed --bypass-seal. emit_seal_bypassed
    // refuses a blank reason; we already validated, so the unwrap-on-Ok
    // is sound.
    let _ = emit_seal_bypassed(&events_path, &mol.id, receipt_b3, reason)
        .map_err(|e| anyhow::anyhow!("failed to emit SealBypassed: {e}"))?;

    eprintln!(
        "warning: cs tackle bypassed stress-test seal on {} \
         (condition={}); BypassReceipt written to {}",
        mol.id,
        report.missing_condition(),
        receipt_path.display()
    );
    // Suppress unused-import lint when SealGuardError is matched only
    // for its From impl path. Reference it explicitly.
    let _: Option<SealGuardError> = None;
    Ok(())
}

/// BLAKE3 hash of `frame.md` if present, else `briefing.md`, else the
/// 64-zero sentinel. The frame artefact distinguishes a bypass that
/// knew the framing from one that pre-dated framing entirely (ADR-085
/// §3.5 — `frame_hash` is forensic, not load-bearing).
fn hash_artifact(mol_dir: &Path) -> String {
    for name in ["frame.md", "briefing.md"] {
        if let Ok(bytes) = std::fs::read(mol_dir.join(name)) {
            return cosmon_hash::Hash::of_bytes(&bytes).to_hex();
        }
    }
    "0".repeat(64)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use cosmon_core::id::{FleetId, FormulaId, MoleculeId};
    use cosmon_core::kind::MoleculeKind;
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{MoleculeData, StateStore};
    use tempfile::TempDir;

    use super::*;

    // ── RÉSIDUEL SÉCU B: exposed launch → fail-closed egress ────────────
    // (task-20260713-d436). `cs tackle` must project the exposed /
    // multi-tenant posture onto the egress enforcement requirement, so a
    // strict-local policy that cannot be kernel-enforced on this host is
    // *refused*, never degraded to an unconfined passthrough shell.

    /// `COSMON_API_REQUEST=1` is the exposed / multi-tenant marker.
    #[test]
    fn egress_launch_is_exposed_reads_api_request_marker() {
        // The RPP adapter sets exactly "1"; a local operator tackle has it
        // unset. Nothing else counts as exposed.
        assert!(egress_launch_is_exposed(
            |k| (k == "COSMON_API_REQUEST").then(|| "1".to_owned())
        ));
        assert!(!egress_launch_is_exposed(|_| None));
        // A stray non-"1" value (e.g. "0", "true", empty) is NOT exposed —
        // the marker is the exact envelope token, not any truthy string.
        assert!(!egress_launch_is_exposed(
            |k| (k == "COSMON_API_REQUEST").then(|| "0".to_owned())
        ));
        assert!(!egress_launch_is_exposed(
            |k| (k == "COSMON_API_REQUEST").then(|| "true".to_owned())
        ));
        assert!(!egress_launch_is_exposed(
            |k| (k == "COSMON_API_REQUEST").then(String::new)
        ));
    }

    /// The load-bearing security assertion: an exposed launch forces the
    /// enforcement requirement true, which — composed with the pre-spawn
    /// preflight — turns a strict-local dispatch on a non-netns host into a
    /// hard `Refused`. Without the projection the same host would take
    /// `DegradedAdvisory` (unjailed passthrough) — the fail-open the
    /// re-review flagged (task-20260713-c5ad §FIX B).
    #[test]
    fn exposed_strict_launch_without_netns_is_refused_not_degraded() {
        use cosmon_core::egress::{EgressJail, EgressPolicy, EgressPreflight};

        let exposed =
            egress_launch_is_exposed(|k| (k == "COSMON_API_REQUEST").then(|| "1".to_owned()));
        // Operator did NOT set COSMON_EGRESS_REQUIRE_NETNS; the exposure
        // alone must supply the requirement.
        let require_netns = false || exposed;
        assert!(
            require_netns,
            "exposed launch must require hard enforcement"
        );

        // Strict-local policy, host cannot build a netns jail (macOS / any
        // non-Linux, or a hardened Linux kernel).
        let decision = EgressJail::preflight(
            EgressPolicy::DenyExternal,
            /* netns_available */ false,
            require_netns,
            /* exposed_multi_tenant */ true,
        );
        assert!(
            matches!(decision, EgressPreflight::Refused { .. }),
            "exposed strict launch on a non-netns host must fail closed, got {decision:?}"
        );
    }

    /// A *remote* exposed launch (`AllowAll`) is unaffected: egress is a
    /// conscious remote opt-in, so forcing the requirement still preflights
    /// `Ready` — the projection only bites strict-local workers.
    #[test]
    fn exposed_remote_launch_still_ready() {
        use cosmon_core::egress::{EgressJail, EgressPolicy, EgressPreflight};

        let require_netns =
            egress_launch_is_exposed(|k| (k == "COSMON_API_REQUEST").then(|| "1".to_owned()));
        let decision = EgressJail::preflight(
            EgressPolicy::AllowAll,
            /* netns_available */ false,
            require_netns,
            /* exposed_multi_tenant */ true,
        );
        assert!(
            matches!(decision, EgressPreflight::Ready),
            "remote (AllowAll) exposed launch must not be refused, got {decision:?}"
        );
    }

    /// A *local operator* strict launch (marker absent) keeps the trusted
    /// single-operator default: degrade to advisory, not refuse. The
    /// projection must not tighten the non-exposed path.
    #[test]
    fn local_operator_strict_launch_degrades_not_refused() {
        use cosmon_core::egress::{EgressJail, EgressPolicy, EgressPreflight};

        let require_netns = egress_launch_is_exposed(|_| None); // no marker
        assert!(!require_netns);
        let decision = EgressJail::preflight(
            EgressPolicy::DenyExternal,
            /* netns_available */ false,
            require_netns,
            /* exposed_multi_tenant */ false,
        );
        assert!(
            matches!(decision, EgressPreflight::DegradedAdvisory { .. }),
            "local operator strict launch should degrade, not refuse, got {decision:?}"
        );
    }

    // ── Model-fallback observability (task-20260614-3116) ──────────────

    /// The selection trail is persisted to `<mol_dir>/model-selection.json`
    /// so the operator can see which model actually backed a tackle (and,
    /// on a fallback, why the preferred one was skipped).
    #[test]
    fn record_model_selection_writes_observable_json() {
        let dir = TempDir::new().unwrap();
        let value = serde_json::json!({
            "outcome": "selected",
            "chosen": "claude-opus-4-8",
            "probes": [
                {"model": "claude-fable-5", "outcome": "unavailable", "detail": "model_not_found"},
                {"model": "claude-opus-4-8", "outcome": "available", "detail": ""},
            ],
        });
        record_model_selection(dir.path(), &value);

        let written = std::fs::read_to_string(dir.path().join("model-selection.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["chosen"], "claude-opus-4-8");
        assert_eq!(parsed["outcome"], "selected");
        assert_eq!(parsed["probes"][0]["model"], "claude-fable-5");
        assert_eq!(parsed["probes"][0]["outcome"], "unavailable");
    }

    /// A probe against a non-existent binary must resolve to
    /// `Unavailable` (carrying the spawn error) rather than panicking —
    /// the whole point is that an unreachable model never hangs the spawn.
    #[test]
    fn probe_missing_binary_is_unavailable_not_panic() {
        use cosmon_core::model_chain::ProbeOutcome;
        let outcome = probe_claude_model("/nonexistent/claude-binary-xyz", "claude-fable-5", None);
        match outcome {
            ProbeOutcome::Unavailable(reason) => assert!(reason.contains("probe spawn failed")),
            ProbeOutcome::Available => panic!("a missing binary cannot be available"),
        }
    }

    /// Test helper: thread a name through the TS-0 validator so tests
    /// can call functions that take `&ValidatedAdapterName`. Production
    /// code goes through the same `validate_adapter_name` call from
    /// `cmd::tackle::tackle`; tests reuse it rather than minting a
    /// backdoor constructor (which would undermine the typestate).
    fn validated(name: &str) -> ValidatedAdapterName {
        let (v, _supervision, _ownership) = validate_adapter_name(
            name,
            &["claude".to_owned(), "aider".to_owned(), "codex".to_owned()],
        )
        .expect("test name must be a built-in adapter");
        v
    }

    fn make_store() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        let store = FileStore::new(&path);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        // Write config.toml with project_id so the project identity guard passes.
        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nproject_id = \"test-0000\"\n",
        )
        .unwrap();
        (tmp, path)
    }

    /// The detached worker must create its timeout while a Tokio reactor is
    /// entered; creating it before `block_on` panics before the first model
    /// request and leaves no artifact to synchronize.
    #[test]
    fn local_worker_timeout_enters_runtime_before_construction() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result =
            run_local_future_with_timeout(&runtime, std::time::Duration::from_secs(1), async {
                "completed"
            });

        assert_eq!(result.ok(), Some("completed"));
    }

    fn sample_molecule(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("idea-to-plan").unwrap(),
            status,
            variables: HashMap::from([("title".to_owned(), "Test molecule".to_owned())]),
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
            kind: Some(MoleculeKind::Idea),
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

    /// A detached local worker retracts its live-process claim before exit.
    #[test]
    fn detached_local_worker_marks_its_process_stopped() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mut molecule = sample_molecule("task-20260715-stop", MoleculeStatus::Running);
        let worker_id = WorkerId::new("local-worker-stop").unwrap();
        molecule.bind_process(cosmon_core::process::MoleculeProcess::new(
            worker_id.clone(),
            "local-stop",
        ));
        store.save_molecule(&molecule.id, &molecule).unwrap();

        mark_detached_local_worker_stopped(&store, &molecule.id, &worker_id).unwrap();

        let reloaded = store.load_molecule(&molecule.id).unwrap();
        assert_eq!(
            reloaded.process.as_ref().map(|process| &process.status),
            Some(&WorkerStatus::Stopped),
            "the terminal local worker must no longer advertise an active process"
        );
    }

    // ── Multi-blocker branch selection (decision task-20260712-2686, C6-2) ──

    /// Run a git command in `dir`, panicking on failure. Commit-time env
    /// vars (`GIT_COMMITTER_DATE`/`GIT_AUTHOR_DATE`) are threaded through so
    /// tests can pin a branch tip's `%ct` timestamp deterministically.
    fn git_in(dir: &Path, env: &[(&str, &str)], args: &[&str]) {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(dir).args(args).env("LC_ALL", "C");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("git spawn");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Initialise a throwaway git repo with a `main` root commit, and return
    /// its path (owned by the returned `TempDir`).
    fn init_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        git_in(&root, &[], &["init", "-q", "-b", "main"]);
        git_in(&root, &[], &["config", "user.email", "t@test"]);
        git_in(&root, &[], &["config", "user.name", "test"]);
        std::fs::write(root.join("seed"), "seed").unwrap();
        git_in(&root, &[], &["add", "-A"]);
        let date = "2026-01-01T00:00:00 +0000";
        git_in(
            &root,
            &[("GIT_COMMITTER_DATE", date), ("GIT_AUTHOR_DATE", date)],
            &["commit", "-q", "-m", "seed"],
        );
        (tmp, root)
    }

    /// Nested paths and names containing the old `__` separator publish as
    /// distinct artifacts. A newline in the third name proves git's NUL path
    /// protocol is preserved instead of being split as text lines.
    #[test]
    fn artifact_sync_uses_injective_nul_safe_path_names() {
        let (_tmp, root) = init_repo();
        let artifacts = root.join("artifacts");
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/result.md"), "nested").unwrap();
        std::fs::write(root.join("src__result.md"), "flat").unwrap();
        std::fs::write(root.join("line\nbreak.md"), "newline").unwrap();

        sync_worktree_deliverables(&root, &artifacts).unwrap();

        let nested = artifact_filename(b"src/result.md");
        let flat = artifact_filename(b"src__result.md");
        let newline = artifact_filename(b"line\nbreak.md");
        assert_ne!(nested, flat, "the mapping must not flatten collisions");
        assert_eq!(
            std::fs::read_to_string(artifacts.join(nested)).unwrap(),
            "nested"
        );
        assert_eq!(
            std::fs::read_to_string(artifacts.join(flat)).unwrap(),
            "flat"
        );
        assert_eq!(
            std::fs::read_to_string(artifacts.join(newline)).unwrap(),
            "newline"
        );
    }

    /// A freshly initialised RPP galaxy may not name its seed branch `main`.
    /// Its untracked worker output must still reach the artifact directory;
    /// otherwise a valid source file is stranded before the fente.
    #[test]
    fn artifact_sync_publishes_untracked_output_without_main_ref() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        git_in(root, &[], &["init", "-q", "-b", "trunk"]);
        git_in(root, &[], &["config", "user.email", "t@test"]);
        git_in(root, &[], &["config", "user.name", "test"]);
        std::fs::write(root.join("seed"), "seed").unwrap();
        git_in(root, &[], &["add", "seed"]);
        git_in(root, &[], &["commit", "-q", "-m", "seed"]);
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn is_prime(_: u64) -> bool { true }",
        )
        .unwrap();

        let artifacts = root.join("artifacts");
        sync_worktree_deliverables(root, &artifacts).unwrap();

        assert_eq!(
            std::fs::read_to_string(artifacts.join(artifact_filename(b"src/lib.rs"))).unwrap(),
            "pub fn is_prime(_: u64) -> bool { true }"
        );
    }

    /// A worktree symlink must never be dereferenced across the RPP boundary.
    #[cfg(unix)]
    #[test]
    fn artifact_sync_rejects_symlink_without_copying_target() {
        let (_tmp, root) = init_repo();
        let artifacts = root.join("artifacts");
        std::os::unix::fs::symlink("/etc/passwd", root.join("result.md")).unwrap();

        let error = sync_worktree_deliverables(&root, &artifacts).unwrap_err();

        assert!(error.to_string().contains("non-regular"));
        assert!(
            !artifacts.join(artifact_filename(b"result.md")).exists(),
            "the symlink target must not be copied"
        );
    }

    /// An unwritable artifact destination is a terminal publication error, not
    /// a best-effort omission that lets the local worker report success.
    #[test]
    fn artifact_sync_surfaces_destination_failures() {
        let (_tmp, root) = init_repo();
        let artifact_file = root.join("not-a-directory");
        std::fs::write(root.join("result.md"), "deliverable").unwrap();
        std::fs::write(&artifact_file, "blocker").unwrap();

        let error = sync_worktree_deliverables(&root, &artifact_file).unwrap_err();

        assert!(error
            .to_string()
            .contains("could not create artifact directory"));
    }

    /// BUG #4: a non-empty synthesis is real work even when the worktree is
    /// otherwise untouched — the model produced output worth booking.
    #[test]
    fn real_work_guard_accepts_non_empty_synthesis() {
        let (_tmp, root) = init_repo();
        assert!(local_worker_produced_real_work(
            &root,
            "  I implemented the fix.  "
        ));
    }

    /// BUG #4: an empty synthesis with no worktree deliverable is a no-op — the
    /// guard must refuse it so the caller does not book a false "completed".
    #[test]
    fn real_work_guard_rejects_empty_synthesis_and_clean_worktree() {
        let (_tmp, root) = init_repo();
        // Only whitespace in the synthesis, and nothing changed since the seed
        // commit: a weak model's no-op turn.
        assert!(!local_worker_produced_real_work(&root, "   \n\t  "));
    }

    /// BUG #4: an empty synthesis is still real work when the worker actually
    /// edited the worktree — a weak model that wrote code but no prose.
    #[test]
    fn real_work_guard_accepts_worktree_deliverable_without_synthesis() {
        let (_tmp, root) = init_repo();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn f() {}").unwrap();
        assert!(local_worker_produced_real_work(&root, ""));
    }

    /// BUG #4: a file produced only under a cosmon-internal path (`.cosmon/`,
    /// `target/`, `.git/`) is NOT a deliverable — it must not rescue a no-op
    /// from the guard.
    #[test]
    fn real_work_guard_ignores_internal_only_output() {
        let (_tmp, root) = init_repo();
        std::fs::create_dir(root.join("target")).unwrap();
        std::fs::write(root.join("target/build-artifact"), "noise").unwrap();
        std::fs::create_dir(root.join(".cosmon")).unwrap();
        std::fs::write(root.join(".cosmon/state"), "internal").unwrap();
        assert!(!local_worker_produced_real_work(&root, ""));
    }

    /// Create branch `feat/{id}` off `main`, add a commit dated `unix_ts`,
    /// and return to `main`.
    fn make_blocker_branch(root: &Path, id: &str, unix_ts: i64) {
        // Git accepts a raw unix timestamp only with the `@` prefix.
        let date = format!("@{unix_ts} +0000");
        git_in(root, &[], &["checkout", "-q", "-b", &format!("feat/{id}")]);
        std::fs::write(root.join(format!("{id}.out")), id).unwrap();
        git_in(root, &[], &["add", "-A"]);
        git_in(
            root,
            &[
                ("GIT_COMMITTER_DATE", date.as_str()),
                ("GIT_AUTHOR_DATE", date.as_str()),
            ],
            &["commit", "-q", "-m", id],
        );
        git_in(root, &[], &["checkout", "-q", "main"]);
    }

    /// A molecule blocked by the given ids, in the given `blocked_by()` order.
    fn blocked_by_mol(id: &str, blockers: &[&str]) -> MoleculeData {
        let mut mol = sample_molecule(id, MoleculeStatus::Pending);
        mol.typed_links = blockers
            .iter()
            .map(|b| cosmon_core::interaction::MoleculeLink::BlockedBy {
                source: MoleculeId::new(*b).unwrap(),
            })
            .collect();
        mol
    }

    // Valid `PREFIX-YYYYMMDD-XXXX` molecule IDs (the `MoleculeId` constructor
    // rejects free-form strings). B1/B2 are the two blockers, D the dependent.
    const B1: &str = "task-20260712-b001";
    const B2: &str = "task-20260712-b002";
    const D: &str = "task-20260712-d001";

    /// No blockers → branch from HEAD/main (`None`).
    #[test]
    fn branch_start_point_no_blockers_is_none() {
        let (_tmp, root) = init_repo();
        let mol = sample_molecule("task-20260712-5010", MoleculeStatus::Pending);
        assert_eq!(resolve_branch_start_point(&root, &mol), None);
    }

    /// A single existing blocker branch is selected.
    #[test]
    fn branch_start_point_single_blocker_selected() {
        let (_tmp, root) = init_repo();
        make_blocker_branch(&root, B1, 1_000_000);
        let mol = blocked_by_mol(D, &[B1]);
        assert_eq!(
            resolve_branch_start_point(&root, &mol),
            Some(format!("feat/{B1}"))
        );
    }

    /// A blocker whose branch does not exist locally is skipped, falling
    /// back to `None` (branch from main — its output is already merged).
    #[test]
    fn branch_start_point_missing_branch_is_none() {
        let (_tmp, root) = init_repo();
        let mol = blocked_by_mol(D, &["task-20260712-9999"]);
        assert_eq!(resolve_branch_start_point(&root, &mol), None);
    }

    /// The core C6-2 pin: with **two live blocker branches**, selection is by
    /// most-recent tip commit — NOT `blocked_by()` iteration order. B1 is
    /// listed first but B2 has the newer commit, so `feat/{B2}` must win.
    /// Reverting to first-existing selection reddens this test.
    #[test]
    fn branch_start_point_multi_blocker_picks_most_recent_not_first() {
        let (_tmp, root) = init_repo();
        // B1 is older; B2 is newer. B1 is declared first in blocked_by order.
        make_blocker_branch(&root, B1, 1_000_000);
        make_blocker_branch(&root, B2, 2_000_000);
        let mol = blocked_by_mol(D, &[B1, B2]);
        assert_eq!(
            resolve_branch_start_point(&root, &mol),
            Some(format!("feat/{B2}")),
            "most-recent tip (B2) must win over first-in-blocked_by (B1)"
        );
    }

    /// The order of `blocked_by()` must not change the result — same two
    /// branches, reversed declaration order, still selects the newer tip.
    #[test]
    fn branch_start_point_multi_blocker_order_independent() {
        let (_tmp, root) = init_repo();
        make_blocker_branch(&root, B1, 1_000_000);
        make_blocker_branch(&root, B2, 2_000_000);
        // Reversed: newer branch declared first this time.
        let mol = blocked_by_mol(D, &[B2, B1]);
        assert_eq!(
            resolve_branch_start_point(&root, &mol),
            Some(format!("feat/{B2}")),
        );
    }

    /// Exact-timestamp tie → deterministic: the first-declared blocker wins
    /// (we only replace on a strictly-greater tip). Pins the tie-break so a
    /// future refactor can't silently make it order-dependent-but-unspecified.
    #[test]
    fn branch_start_point_tie_breaks_to_first_declared() {
        let (_tmp, root) = init_repo();
        make_blocker_branch(&root, B1, 1_500_000);
        make_blocker_branch(&root, B2, 1_500_000);
        let mol = blocked_by_mol(D, &[B1, B2]);
        assert_eq!(
            resolve_branch_start_point(&root, &mol),
            Some(format!("feat/{B1}")),
            "equal timestamps break to the first-declared blocker"
        );
    }

    // -- Q5a: six-level adapter resolution chain (task-20260530-c089,
    //    extended by task-20260531-c99e: env + global tiers) ----------------

    /// Build an [`AdaptersConfig`] whose `[adapters.default]` is `name`.
    fn cfg_with_default(name: &str) -> cosmon_core::config::AdaptersConfig {
        cosmon_core::config::AdaptersConfig {
            default: Some(name.to_owned()),
            entries: std::collections::BTreeMap::new(),
        }
    }

    /// Per-galaxy config path used by the resolution-chain tests.
    fn galaxy_cfg_path() -> &'static Path {
        Path::new("/tmp/.cosmon/config.toml")
    }

    /// Global config path used by the resolution-chain tests.
    fn global_cfg_path() -> &'static Path {
        Path::new("/tmp/.config/cosmon/config.toml")
    }

    #[test]
    fn adapter_chain_flag_beats_everything() {
        // --adapter is rank 1: it wins over a step pin, env, and both configs.
        let cfg = cfg_with_default("openai");
        let global = cfg_with_default("anthropic");
        let (name, source) = resolve_adapter_selection(
            Some("aider"),
            Some(("claude", "deep-think", "panel")),
            Some("openai"),
            Some(&cfg),
            galaxy_cfg_path(),
            Some(&global),
            global_cfg_path(),
        );
        assert_eq!(name, "aider");
        assert!(matches!(source, AdapterSelectionSource::Cli { flag } if flag == "aider"));
    }

    #[test]
    fn adapter_chain_formula_step_beats_env_config_and_floor() {
        // No flag → the formula-step pin (rank 2) wins over env AND both
        // configs: a correctness need outranks a blanket preference.
        let cfg = cfg_with_default("local");
        let (name, source) = resolve_adapter_selection(
            None,
            Some(("claude", "deep-think", "panel")),
            Some("openai"),
            Some(&cfg),
            galaxy_cfg_path(),
            None,
            global_cfg_path(),
        );
        assert_eq!(name, "claude");
        assert!(matches!(
            source,
            AdapterSelectionSource::FormulaStep { formula, step_id }
                if formula == "deep-think" && step_id == "panel"
        ));
    }

    #[test]
    fn adapter_chain_env_beats_both_configs() {
        // No flag, no step pin → $COSMON_DEFAULT_ADAPTER (rank 3) wins over
        // both the per-galaxy and the global config.
        let cfg = cfg_with_default("claude");
        let global = cfg_with_default("anthropic");
        let (name, source) = resolve_adapter_selection(
            None,
            None,
            Some("openai"),
            Some(&cfg),
            galaxy_cfg_path(),
            Some(&global),
            global_cfg_path(),
        );
        assert_eq!(name, "openai");
        assert!(matches!(
            source,
            AdapterSelectionSource::EnvVar { var } if var == "COSMON_DEFAULT_ADAPTER"
        ));
    }

    #[test]
    fn adapter_chain_env_empty_string_is_unset() {
        // An empty env value is treated as unset: it falls through to the
        // per-galaxy config rather than pinning an empty adapter name.
        let cfg = cfg_with_default("claude");
        let (name, source) = resolve_adapter_selection(
            None,
            None,
            Some(""),
            Some(&cfg),
            galaxy_cfg_path(),
            None,
            global_cfg_path(),
        );
        assert_eq!(name, "claude");
        assert!(matches!(
            source,
            AdapterSelectionSource::Config { key, .. } if key == "adapters.default"
        ));
    }

    #[test]
    fn adapter_chain_per_galaxy_beats_global() {
        // No flag, no step pin, no env, per-galaxy default present → it wins
        // (rank 4) and the global config is ignored: a committed per-galaxy
        // choice outranks the uncommitted machine preference.
        let cfg = cfg_with_default("claude");
        let global = cfg_with_default("anthropic");
        let (name, source) = resolve_adapter_selection(
            None,
            None,
            None,
            Some(&cfg),
            galaxy_cfg_path(),
            Some(&global),
            global_cfg_path(),
        );
        assert_eq!(name, "claude");
        assert!(matches!(
            source,
            AdapterSelectionSource::Config { key, .. } if key == "adapters.default"
        ));
    }

    #[test]
    fn adapter_chain_global_config_when_per_galaxy_silent() {
        // No flag, no step pin, no env, per-galaxy silent, global present →
        // the global config (rank 5) resolves, with honest provenance.
        let global = cfg_with_default("anthropic");
        for galaxy in [None, Some(cosmon_core::config::AdaptersConfig::default())] {
            let (name, source) = resolve_adapter_selection(
                None,
                None,
                None,
                galaxy.as_ref(),
                galaxy_cfg_path(),
                Some(&global),
                global_cfg_path(),
            );
            assert_eq!(name, "anthropic");
            assert!(matches!(
                source,
                AdapterSelectionSource::GlobalConfig { ref path }
                    if path == "/tmp/.config/cosmon/config.toml"
            ));
        }
    }

    #[test]
    fn adapter_chain_floor_is_local_and_config_undeletable() {
        // Nothing set anywhere — no flag, no step pin, no env, no per-galaxy
        // default, no global default → the built-in "local" floor (rank 6).
        // This is the load-bearing invariant: deleting every config row and
        // unsetting the env can never reach Claude.
        for galaxy in [None, Some(cosmon_core::config::AdaptersConfig::default())] {
            for global in [None, Some(cosmon_core::config::AdaptersConfig::default())] {
                let (name, source) = resolve_adapter_selection(
                    None,
                    None,
                    None,
                    galaxy.as_ref(),
                    galaxy_cfg_path(),
                    global.as_ref(),
                    global_cfg_path(),
                );
                assert_eq!(name, "local", "the code floor is local, never claude");
                assert!(matches!(source, AdapterSelectionSource::Default { .. }));
            }
        }
    }

    #[test]
    fn adapter_chain_flag_beats_config_when_no_step_pin() {
        let cfg = cfg_with_default("local");
        let (name, source) = resolve_adapter_selection(
            Some("claude"),
            None,
            None,
            Some(&cfg),
            galaxy_cfg_path(),
            None,
            global_cfg_path(),
        );
        assert_eq!(name, "claude");
        assert!(matches!(source, AdapterSelectionSource::Cli { .. }));
    }

    // -- C1: six-level MODEL resolution chain (delib-20260704-b476) --------
    //    The model sibling of the adapter chain, floor `None` not a
    //    constant, config tiers scoped per adapter.

    /// Build an [`AdaptersConfig`] with `[adapters.<adapter>].default_model`
    /// set to `model`, so the config-tier model lookups have something to
    /// resolve. `default` (the adapter-name tier) is left `None`.
    fn cfg_with_model(adapter: &str, model: &str) -> cosmon_core::config::AdaptersConfig {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            adapter.to_owned(),
            cosmon_core::config::AdapterEntry {
                default_model: Some(model.to_owned()),
                ..cosmon_core::config::AdapterEntry::default()
            },
        );
        cosmon_core::config::AdaptersConfig {
            default: None,
            entries,
        }
    }

    #[test]
    fn model_chain_flag_beats_everything() {
        // --model is rank 1: it wins over a step pin, env, and both configs.
        let cfg = cfg_with_model("claude", "cfg-model");
        let global = cfg_with_model("claude", "global-model");
        let (model, source) = resolve_model_selection(
            Some("flag-model"),
            Some(("pin-model", "deep-think", "panel")),
            Some(("env-model", "COSMON_DEFAULT_MODEL")),
            "claude",
            Some(&cfg),
            galaxy_cfg_path(),
            Some(&global),
            global_cfg_path(),
        );
        assert_eq!(model.as_deref(), Some("flag-model"));
        assert!(matches!(source, ModelSelectionSource::Flag { flag } if flag == "flag-model"));
    }

    #[test]
    fn model_chain_formula_pin_beats_env_and_config() {
        // No flag → the formula-step model pin (rank 2) wins over env AND
        // both configs: a correctness need outranks a blanket preference.
        let cfg = cfg_with_model("claude", "cfg-model");
        let (model, source) = resolve_model_selection(
            None,
            Some(("pin-model", "deep-think", "panel")),
            Some(("env-model", "COSMON_DEFAULT_MODEL")),
            "claude",
            Some(&cfg),
            galaxy_cfg_path(),
            None,
            global_cfg_path(),
        );
        assert_eq!(model.as_deref(), Some("pin-model"));
        assert!(matches!(
            source,
            ModelSelectionSource::FormulaPin { formula, step_id }
                if formula == "deep-think" && step_id == "panel"
        ));
    }

    #[test]
    fn model_chain_env_beats_both_configs_and_records_var_name() {
        // No flag, no pin → the env tier (rank 3) wins over both configs,
        // and the recorded var name is the one that actually fired (the
        // legacy $ANTHROPIC_MODEL carrier here, not the canonical name).
        let cfg = cfg_with_model("claude", "cfg-model");
        let global = cfg_with_model("claude", "global-model");
        let (model, source) = resolve_model_selection(
            None,
            None,
            Some(("env-model", "ANTHROPIC_MODEL")),
            "claude",
            Some(&cfg),
            galaxy_cfg_path(),
            Some(&global),
            global_cfg_path(),
        );
        assert_eq!(model.as_deref(), Some("env-model"));
        assert!(matches!(
            source,
            ModelSelectionSource::EnvVar { var } if var == "ANTHROPIC_MODEL"
        ));
    }

    #[test]
    fn model_chain_config_is_scoped_to_the_resolved_adapter() {
        // No flag, no pin, no env → the per-galaxy config (rank 4) resolves,
        // scoped to the adapter: the same config carries no default_model for
        // a *different* adapter, so a mismatched adapter falls through.
        let cfg = cfg_with_model("claude", "cfg-model");
        let (model, source) = resolve_model_selection(
            None,
            None,
            None,
            "claude",
            Some(&cfg),
            galaxy_cfg_path(),
            None,
            global_cfg_path(),
        );
        assert_eq!(model.as_deref(), Some("cfg-model"));
        assert!(matches!(
            source,
            ModelSelectionSource::Config { key, .. }
                if key == "adapters.claude.default_model"
        ));
        // Same config, different adapter → the claude row is invisible, so
        // the chain hits the floor (`None`), never leaking claude's model.
        let (other, other_source) = resolve_model_selection(
            None,
            None,
            None,
            "openai",
            Some(&cfg),
            galaxy_cfg_path(),
            None,
            global_cfg_path(),
        );
        assert_eq!(other, None);
        assert!(matches!(other_source, ModelSelectionSource::Default { .. }));
    }

    #[test]
    fn model_chain_per_galaxy_beats_global() {
        // Both configs carry a claude default_model → the per-galaxy one wins.
        let cfg = cfg_with_model("claude", "cfg-model");
        let global = cfg_with_model("claude", "global-model");
        let (model, source) = resolve_model_selection(
            None,
            None,
            None,
            "claude",
            Some(&cfg),
            galaxy_cfg_path(),
            Some(&global),
            global_cfg_path(),
        );
        assert_eq!(model.as_deref(), Some("cfg-model"));
        assert!(matches!(source, ModelSelectionSource::Config { .. }));
    }

    #[test]
    fn model_chain_global_config_when_per_galaxy_silent() {
        // Per-galaxy silent, global present → the global config (rank 5) wins.
        let global = cfg_with_model("claude", "global-model");
        for galaxy in [None, Some(cosmon_core::config::AdaptersConfig::default())] {
            let (model, source) = resolve_model_selection(
                None,
                None,
                None,
                "claude",
                galaxy.as_ref(),
                galaxy_cfg_path(),
                Some(&global),
                global_cfg_path(),
            );
            assert_eq!(model.as_deref(), Some("global-model"));
            assert!(matches!(
                source,
                ModelSelectionSource::GlobalConfig { ref path }
                    if path == "/tmp/.config/cosmon/config.toml"
            ));
        }
    }

    #[test]
    fn model_chain_floor_is_none_not_a_strong_constant() {
        // Nothing set anywhere → the floor is `None`: cosmon pins no model
        // and the adapter's own default applies. This is von-neumann's
        // minimax floor — silence NEVER resolves to a named (strong) model.
        for galaxy in [None, Some(cosmon_core::config::AdaptersConfig::default())] {
            for global in [None, Some(cosmon_core::config::AdaptersConfig::default())] {
                let (model, source) = resolve_model_selection(
                    None,
                    None,
                    None,
                    "claude",
                    galaxy.as_ref(),
                    galaxy_cfg_path(),
                    global.as_ref(),
                    global_cfg_path(),
                );
                assert_eq!(model, None, "the model floor is None, never a strong id");
                assert!(matches!(source, ModelSelectionSource::Default { .. }));
            }
        }
    }

    /// Serve exactly one canned HTTP response on an ephemeral port, then
    /// exit. Returns the base URL. Enough to drive the preflight without
    /// pulling a test-server dependency into the CLI.
    fn one_shot_http(body: &'static str, status_line: &'static str) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn preflight_timeout() -> std::time::Duration {
        std::time::Duration::from_secs(PREFLIGHT_TIMEOUT_SECS)
    }

    #[test]
    fn preflight_refuses_the_empty_ollama_that_collapsed_two_molecules() {
        // THE regression test (task-20260719-f45b). On 2026-07-19 an Ollama
        // was running and answering, but had nothing pulled — it replies
        // `{"object":"list","data":null}`. Two molecules were dispatched to
        // it, spawned workers that died in ~30 s, and were auto-collapsed by
        // the patrol into a TERMINAL state, losing the work.
        //
        // Reachable-but-empty must refuse, and must say so as
        // `ModelNotServed` (pull a model) — not `Unreachable` (start the
        // daemon), which would send the operator to the wrong repair.
        let base = one_shot_http(r#"{"object":"list","data":null}"#, "200 OK");
        let err = preflight_local_adapter_model(&base, "qwen3:8b", preflight_timeout())
            .expect_err("an Ollama serving no models must refuse the dispatch");
        match err {
            LocalPreflightError::ModelNotServed {
                model, available, ..
            } => {
                assert_eq!(model, "qwen3:8b");
                assert!(
                    available.is_empty(),
                    "a null `data` means no models served, not a parse failure"
                );
            }
            unreachable @ LocalPreflightError::Unreachable { .. } => {
                panic!("expected ModelNotServed, got {unreachable:?}")
            }
        }
    }

    #[test]
    fn preflight_admits_a_model_the_backend_actually_serves() {
        // The healthy path must NOT refuse — a preflight that blocks good
        // dispatches is worse than none.
        let base = one_shot_http(
            r#"{"object":"list","data":[{"id":"qwen3:8b"},{"id":"llama3:8b"}]}"#,
            "200 OK",
        );
        assert!(preflight_local_adapter_model(&base, "qwen3:8b", preflight_timeout()).is_ok());
    }

    #[test]
    fn preflight_refuses_an_explicitly_pinned_but_unpulled_model() {
        // Why the guard is on "can the backend serve it", not on "was a
        // model selected": here a model IS pinned, so a None-check would
        // wave this through — and the worker would die exactly as the two
        // collapsed molecules did. The served-model check catches it.
        let base = one_shot_http(r#"{"object":"list","data":[{"id":"llama3:8b"}]}"#, "200 OK");
        let err = preflight_local_adapter_model(&base, "qwen3:8b", preflight_timeout())
            .expect_err("a pinned-but-unpulled model must refuse");
        match err {
            LocalPreflightError::ModelNotServed { available, .. } => {
                assert_eq!(available, vec!["llama3:8b".to_owned()]);
            }
            unreachable @ LocalPreflightError::Unreachable { .. } => {
                panic!("expected ModelNotServed, got {unreachable:?}")
            }
        }
    }

    #[test]
    fn preflight_refuses_when_the_backend_is_unreachable() {
        // Nothing listening: bind a port, drop the listener, reuse the addr.
        let addr = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr")
        };
        let err = preflight_local_adapter_model(
            &format!("http://{addr}"),
            "qwen3:8b",
            std::time::Duration::from_millis(500),
        )
        .expect_err("a dead backend must refuse the dispatch");
        assert!(
            matches!(err, LocalPreflightError::Unreachable { .. }),
            "a dead backend is Unreachable (start it), not ModelNotServed (pull a model); got {err:?}"
        );
    }

    #[test]
    fn preflight_diagnostics_name_the_repair_and_promise_recoverability() {
        // The refusal text is the whole operator-facing payload: it must
        // name the fix and state that the molecule survived, because the
        // failure mode being replaced is a SILENT terminal collapse.
        let empty = LocalPreflightError::ModelNotServed {
            base_url: "http://localhost:11434".to_owned(),
            model: "qwen3:8b".to_owned(),
            available: Vec::new(),
        }
        .to_string();
        assert!(empty.contains("ollama pull qwen3:8b"), "{empty}");
        assert!(empty.contains("no models at all"), "{empty}");
        assert!(empty.contains("still tacklable"), "{empty}");

        let dead = LocalPreflightError::Unreachable {
            base_url: "http://localhost:11434".to_owned(),
            detail: "connection refused".to_owned(),
        }
        .to_string();
        assert!(dead.contains("ollama serve"), "{dead}");
        assert!(dead.contains("still tacklable"), "{dead}");
    }

    #[test]
    fn preflight_and_worker_resolve_the_same_model() {
        // The preflight is only meaningful if it checks the model the
        // worker will actually run. Both call `resolve_local_model`; this
        // pins the two top tiers, which short-circuit above the env tier
        // and so are safe under test parallelism.
        let cfg = cosmon_core::config::AdapterEntry {
            default_model: Some("llama3:8b".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            resolve_local_model(Some("qwen3:8b"), Some(&cfg)),
            "qwen3:8b",
            "an explicit pin outranks config"
        );
        assert_eq!(
            resolve_local_model(None, Some(&cfg)),
            "llama3:8b",
            "config default applies when nothing is pinned"
        );
        assert_eq!(
            resolve_local_model(Some(""), Some(&cfg)),
            "llama3:8b",
            "an empty pin is unset, not an empty model id"
        );
    }

    #[test]
    fn model_chain_empty_flag_and_env_are_unset() {
        // An empty `--model ""` and an empty env value are both treated as
        // unset: they fall through rather than pinning an empty model id.
        let cfg = cfg_with_model("claude", "cfg-model");
        let (model, source) = resolve_model_selection(
            Some(""),
            None,
            Some(("", "COSMON_DEFAULT_MODEL")),
            "claude",
            Some(&cfg),
            galaxy_cfg_path(),
            None,
            global_cfg_path(),
        );
        assert_eq!(model.as_deref(), Some("cfg-model"));
        assert!(matches!(source, ModelSelectionSource::Config { .. }));
    }

    /// The C1 output criterion: two sibling dispatches pinning *different*
    /// models resolve to independent pins with no cross-contamination, and
    /// each flows through the claude `ANTHROPIC_MODEL` closure-shadow as its
    /// own byte string — the isolation the whole feature exists to provide
    /// (strong is never inherited; resolution is a pure per-dispatch fn).
    #[test]
    fn two_sibling_model_pins_produce_independent_command_env() {
        let build_env = |pin: Option<&str>| {
            let (preferred, _src) = resolve_model_selection(
                pin,
                None,
                None,
                "claude",
                None,
                galaxy_cfg_path(),
                None,
                global_cfg_path(),
            );
            // Mirror the spawn-time closure-shadow at tackle.rs: the resolved
            // pin is what `build_claude_command` reads for `ANTHROPIC_MODEL`.
            preferred
        };
        let worker_a = build_env(Some("claude-fable-5"));
        let worker_b = build_env(Some("claude-opus-4-8"));
        assert_eq!(worker_a.as_deref(), Some("claude-fable-5"));
        assert_eq!(worker_b.as_deref(), Some("claude-opus-4-8"));
        // No shared mutable state: resolving A did not alter B's resolution.
        assert_ne!(worker_a, worker_b);
        // A third sibling with no pin resolves to the floor `None` — it does
        // NOT inherit either sibling's strong model.
        let worker_c = build_env(None);
        assert_eq!(worker_c, None);
    }

    // -- C4: strong cost-class + fail-closed ceiling (delib-20260704-b476) --
    //    The pure decision logic lives in `cosmon_core::model_budget` (tested
    //    there); these exercise the `cs tackle`-side helpers that feed it —
    //    the per-adapter strong-set union and the `events.jsonl` fold — plus
    //    their composition through the gate at the cap.

    #[test]
    fn strong_set_unions_per_galaxy_and_global() {
        let mut galaxy_entries = std::collections::BTreeMap::new();
        galaxy_entries.insert(
            "claude".to_owned(),
            cosmon_core::config::AdapterEntry {
                strong: vec!["claude-fable-5".to_owned()],
                ..Default::default()
            },
        );
        let galaxy = cosmon_core::config::AdaptersConfig {
            default: None,
            entries: galaxy_entries,
        };
        let mut global_entries = std::collections::BTreeMap::new();
        global_entries.insert(
            "claude".to_owned(),
            cosmon_core::config::AdapterEntry {
                // Overlap on fable-5 (must dedup) + a new id (must union in).
                strong: vec!["claude-fable-5".to_owned(), "claude-opus-4-8".to_owned()],
                ..Default::default()
            },
        );
        let global = cosmon_core::config::AdaptersConfig {
            default: None,
            entries: global_entries,
        };
        let set = adapter_strong_set(Some(&galaxy), Some(&global), "claude");
        assert_eq!(set.len(), 2, "fable-5 deduped, opus unioned in");
        assert!(set.iter().any(|s| s == "claude-fable-5"));
        assert!(set.iter().any(|s| s == "claude-opus-4-8"));
        // An adapter with no rows anywhere → empty (fail-open: nothing strong).
        assert!(adapter_strong_set(Some(&galaxy), Some(&global), "aider").is_empty());
    }

    #[test]
    fn dispatch_fold_reads_model_selected_and_skips_the_floor() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path();
        let mol = MoleculeId::new("task-20260705-aaaa").unwrap();
        // Two concrete pins + one floor (`None`, must be skipped by the fold).
        emit_model_selected(
            state_dir,
            &mol,
            "claude",
            Some("claude-fable-5"),
            ModelSelectionSource::Flag {
                flag: "claude-fable-5".to_owned(),
            },
        );
        emit_model_selected(
            state_dir,
            &mol,
            "openai",
            Some("gpt-strong"),
            ModelSelectionSource::Flag {
                flag: "gpt-strong".to_owned(),
            },
        );
        emit_model_selected(
            state_dir,
            &mol,
            "local",
            None,
            ModelSelectionSource::Default {
                fallback_reason: "floor".to_owned(),
            },
        );
        let records = load_strong_dispatch_records(state_dir).expect("log is readable");
        assert_eq!(records.len(), 2, "the None floor is not a record");
        assert!(records.iter().any(|r| r.model == "claude-fable-5"));
        assert!(records.iter().any(|r| r.model == "gpt-strong"));
    }

    /// A never-created log is a *trustworthy zero* (`Ok(empty)`), NOT an
    /// unreadable one (`Err`). Only a genuinely unreadable log fails closed;
    /// a fresh galaxy with no dispatch history must route normally.
    #[test]
    fn absent_event_log_is_trustworthy_zero_not_unavailable() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path();
        let records = load_strong_dispatch_records(state_dir)
            .expect("an absent log is a genuine empty, not an unreadable error");
        assert!(records.is_empty());
    }

    /// End-to-end of the C4 output criterion: with `K` strong dispatches
    /// already on the log, the (K+1)th strong pin fails closed. The fold
    /// (`load_strong_dispatch_records` + `count_strong_in_window`) feeds the
    /// gate; the gate refuses / downgrades per policy.
    #[test]
    fn kth_plus_one_strong_dispatch_fails_closed() {
        use cosmon_core::model_budget::{
            count_strong_in_window, is_strong_model, strong_gate, OverflowPolicy, StrongGate,
        };
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path();
        let mol = MoleculeId::new("task-20260705-bbbb").unwrap();
        let strong = vec!["claude-fable-5".to_owned()];
        // Cap K = 2. Land exactly 2 strong dispatches on the log.
        for _ in 0..2 {
            emit_model_selected(
                state_dir,
                &mol,
                "claude",
                Some("claude-fable-5"),
                ModelSelectionSource::Flag {
                    flag: "claude-fable-5".to_owned(),
                },
            );
        }
        let records = load_strong_dispatch_records(state_dir).expect("log is readable");
        let now = chrono::Utc::now();
        let count = count_strong_in_window(&records, now, chrono::Duration::hours(24), |_a, m| {
            is_strong_model(&strong, m)
        });
        assert_eq!(count, 2, "two strong dispatches counted in the window");
        // The 3rd (positive-act) strong pin is at the cap → fails closed.
        let flag = ModelSelectionSource::Flag {
            flag: "claude-fable-5".to_owned(),
        };
        assert_eq!(
            strong_gate(true, &flag, count, Some(2), OverflowPolicy::Abort),
            StrongGate::Abort {
                strong_count: 2,
                cap: 2
            }
        );
        assert!(matches!(
            strong_gate(true, &flag, count, Some(2), OverflowPolicy::Downgrade),
            StrongGate::Downgrade { .. }
        ));
        // A widened cap (K=3) still admits the 3rd dispatch.
        assert_eq!(
            strong_gate(true, &flag, count, Some(3), OverflowPolicy::Abort),
            StrongGate::AllowStrong
        );
    }

    #[test]
    fn test_resolve_exact_match() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule(&store, "idea-20260407-abcd").unwrap();
        assert_eq!(resolved.id.as_str(), "idea-20260407-abcd");
    }

    #[test]
    fn test_resolve_prefix_match() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule(&store, "idea-20260407-ab").unwrap();
        assert_eq!(resolved.id.as_str(), "idea-20260407-abcd");
    }

    #[test]
    fn test_resolve_substring_match() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule(&store, "abcd").unwrap();
        assert_eq!(resolved.id.as_str(), "idea-20260407-abcd");
    }

    #[test]
    fn test_resolve_topic_match() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mut mol = sample_molecule("task-20260407-42a8", MoleculeStatus::Pending);
        mol.variables
            .insert("topic".to_owned(), "fix send-keys escaping".to_owned());
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule(&store, "fix send-keys").unwrap();
        assert_eq!(resolved.id.as_str(), "task-20260407-42a8");
    }

    #[test]
    fn test_resolve_topic_case_insensitive() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mut mol = sample_molecule("task-20260407-42a8", MoleculeStatus::Pending);
        mol.variables
            .insert("topic".to_owned(), "Fix Send-Keys".to_owned());
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule(&store, "fix send-keys").unwrap();
        assert_eq!(resolved.id.as_str(), "task-20260407-42a8");
    }

    #[test]
    fn test_resolve_formula_id_match() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("task-20260407-abcd", MoleculeStatus::Pending);
        // formula_id is "idea-to-plan" from sample_molecule
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule(&store, "idea-to-plan").unwrap();
        assert_eq!(resolved.id.as_str(), "task-20260407-abcd");
    }

    #[test]
    fn test_resolve_ambiguous_shows_topic() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let mut m1 = sample_molecule("task-20260407-aaaa", MoleculeStatus::Pending);
        m1.variables
            .insert("topic".to_owned(), "fix send-keys escaping".to_owned());
        let mut m2 = sample_molecule("task-20260407-bbbb", MoleculeStatus::Pending);
        m2.variables
            .insert("topic".to_owned(), "fix send-keys quoting".to_owned());
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();

        let err = resolve_molecule(&store, "fix send-keys").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"));
        assert!(msg.contains("fix send-keys escaping"));
        assert!(msg.contains("fix send-keys quoting"));
    }

    #[test]
    fn test_resolve_no_match() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let err = resolve_molecule(&store, "nonexistent").unwrap_err();
        assert!(err.to_string().contains("no molecule"));
    }

    #[test]
    fn test_resolve_ambiguous() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let m1 = sample_molecule("idea-20260407-aaaa", MoleculeStatus::Pending);
        let m2 = sample_molecule("idea-20260407-aabb", MoleculeStatus::Pending);
        store.save_molecule(&m1.id, &m1).unwrap();
        store.save_molecule(&m2.id, &m2).unwrap();

        let err = resolve_molecule(&store, "idea-20260407-aa").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn test_build_prompt_basic() {
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &ProjectConfig::default(),
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("idea-20260407-abcd"));
        assert!(prompt.contains("idea-to-plan"));
        assert!(prompt.contains("Step 1/3"));
        // Must have autonomous work mode header.
        assert!(prompt.contains("AUTONOMOUS WORK MODE"));
        // Must have DO NOT list.
        assert!(prompt.contains("DO NOT"));
        assert!(prompt.contains("Do NOT pause between steps"));
        // Must have terminal action.
        assert!(prompt.contains("cs complete"));
        // Must end with execute now.
        assert!(prompt.contains("Execute step 1 NOW"));
        // Must carry the canonical-text guideline (task-20260623-80f9):
        // workers FETCH standard licence/legal texts, never LLM-generate
        // them (long canonical text trips the OUTPUT content-filter).
        assert!(prompt.contains("Canonical texts — fetch, never generate"));
        assert!(prompt.contains("trips the model's OUTPUT content-filter"));
        // Must carry the diagnosis-discipline THIN POINTER (delib-20260711-f62a
        // Q8 / C7 = task-20260711-7173): the root-cause/perf class points at the
        // guide, the six clauses stay OUT of the brief (Transport ≠ Cognition).
        assert!(prompt.contains("Diagnosis discipline (root-cause & perf molecules)"));
        assert!(prompt.contains("docs/guides/diagnosis-discipline.md"));
        // The pointer must NOT inline the clause bodies — cognition rots the DNA.
        assert!(!prompt.contains("Instrument the seam before you trust"));
    }

    #[test]
    fn test_build_prompt_no_attribution_block_is_byte_identical() {
        // Passive-helper discipline (ADR-128, mirrors CLAUDE_CONFIG_DIR):
        // with no `[attribution]` block the prompt must NOT gain an
        // attribution section — byte-identical to a pre-attribution cosmon.
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        let mol_dir = Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd");
        let baseline = build_prompt(&mol, None, None, &ProjectConfig::default(), mol_dir);
        assert!(!baseline.contains("## External attribution"));
        assert!(!baseline.contains("External attribution for this fleet"));
    }

    #[test]
    fn test_build_prompt_injects_attribution_directive_when_configured() {
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        let mol_dir = Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd");
        let mut config = ProjectConfig::default();
        config.attribution.public_name = "Noogram".to_owned();
        config.attribution.public_url = "noogram.org".to_owned();
        let prompt = build_prompt(&mol, None, None, &config, mol_dir);

        assert!(prompt.contains("## External attribution"));
        assert!(prompt.contains("External attribution for this fleet is `Noogram` (noogram.org)."));
        assert!(prompt.contains("The operator's fund affiliation is PRIVATE"));

        // The directive must sit ABOVE the mission so the worker reads the
        // right name before reaching any artifact slot.
        let attr_pos = prompt.find("## External attribution").unwrap();
        let mission_pos = prompt.find("## Mission").unwrap();
        assert!(
            attr_pos < mission_pos,
            "attribution must precede the mission"
        );
    }

    #[test]
    fn test_build_prompt_with_formula() {
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        let formula_toml = r#"
            formula = "idea-to-plan"
            version = 1
            description = "Test"
            id_prefix = "idea"

            [[steps]]
            id = "capture"
            title = "Capture the idea"
            description = "Document the idea."
            acceptance = "Idea documented"
        "#;
        let formula = cosmon_core::formula::Formula::parse(formula_toml).unwrap();
        let prompt = build_prompt(
            &mol,
            Some(&formula),
            None,
            &ProjectConfig::default(),
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        // Step checklist rendered inline.
        assert!(prompt.contains("Step Checklist"));
        assert!(prompt.contains("Capture the idea"));
        assert!(prompt.contains("Document the idea."));
        assert!(prompt.contains("CURRENT"));
    }

    #[test]
    fn test_build_prompt_with_briefing() {
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        let prompt = build_prompt(
            &mol,
            None,
            Some("# Mission\nDo something great."),
            &ProjectConfig::default(),
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("## Briefing"));
        assert!(prompt.contains("Do something great."));
    }

    #[test]
    fn test_build_prompt_injects_canonical_molecule_dir() {
        // Lever A (idea-20260531-107d): the bootstrap prompt must hand the
        // worker the EXACT absolute canonical molecule_dir so durable
        // artifacts never land in the throwaway worktree. Guards against the
        // delib-20260410-b79f / delib-20260531-bcc7 data-loss class.
        let mol = sample_molecule("idea-20260407-abcd", MoleculeStatus::Pending);
        let mol_dir = Path::new(
            "/srv/cosmon/example/.cosmon/state/fleets/default/molecules/idea-example-abcd",
        );
        let prompt = build_prompt(&mol, None, None, &ProjectConfig::default(), mol_dir);

        // The block header and its imperative are present.
        assert!(prompt.contains("## Artifact paths — write durable output HERE"));
        assert!(prompt.contains("Canonical molecule directory (resolved):"));
        // The exact absolute path appears verbatim.
        assert!(prompt.contains(
            "/srv/cosmon/example/.cosmon/state/fleets/default/molecules/idea-example-abcd"
        ));
        // The worktree-is-destroyed warning is present and names the molecule.
        assert!(prompt.contains("NEVER write them to"));
        assert!(prompt.contains(".worktrees/idea-20260407-abcd/"));
        assert!(prompt.contains("DESTROYED when `cs done`"));
    }

    #[test]
    fn test_tackle_rejects_completed_molecule() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("idea-20260407-done", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            molecule: "idea-20260407-done".to_owned(),
            fleet: None,
            workdir: None,
            no_worktree: true,
            dry_run: true,
            permission_mode: None,
            force: false,
            name: None,
            leaf: false,
            force_runtime: false,
            bypass_seal: false,
            bypass_reason: None,
            adapter: None,
            model: None,
            role_hint: None,
            fallback_from_local: None,
            by: "human".to_owned(),
        };
        let err = run(&ctx, &args).unwrap_err();
        assert!(err
            .to_string()
            .contains("cannot tackle a terminal molecule"));
    }

    #[test]
    fn test_dry_run_outputs_prompt() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("idea-20260407-test", MoleculeStatus::Pending);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            molecule: "idea-20260407-test".to_owned(),
            fleet: None,
            workdir: None,
            no_worktree: true,
            dry_run: true,
            permission_mode: None,
            force: false,
            name: None,
            leaf: false,
            force_runtime: false,
            bypass_seal: false,
            bypass_reason: None,
            adapter: None,
            model: None,
            role_hint: None,
            fallback_from_local: None,
            by: "human".to_owned(),
        };
        // dry_run should succeed without tmux.
        let result = run(&ctx, &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_register_tackle_worker_stores_relative_path() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let wid = WorkerId::new("cosmon-test-mol").unwrap();
        let mol = sample_molecule("task-20260409-wwww", MoleculeStatus::Running);
        let repo_root = std::path::PathBuf::from("/projects/cosmon");
        let worktree = repo_root.join(".worktrees/task-20260409-wwww");

        register_tackle_worker(
            &store,
            &wid,
            &worktree,
            &repo_root,
            &mol,
            &validated("claude"),
            LoopOwnership::External,
        )
        .unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.contains_key(&wid));
        let worker = &fleet.workers[&wid];
        assert_eq!(worker.desired, DesiredState::Running);
        // Path is stored relative to the project root.
        assert_eq!(
            worker.repo.as_deref(),
            Some(".worktrees/task-20260409-wwww")
        );
        assert_eq!(worker.current_molecule, Some(mol.id));
    }

    #[test]
    fn test_register_tackle_worker_external_workdir_stays_absolute() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let wid = WorkerId::new("cosmon-test-ext").unwrap();
        let mol = sample_molecule("task-20260409-ext1", MoleculeStatus::Running);
        let repo_root = std::path::PathBuf::from("/projects/cosmon");
        // workdir outside the repo root — cannot be made relative
        let worktree = std::path::PathBuf::from("/other/place");

        register_tackle_worker(
            &store,
            &wid,
            &worktree,
            &repo_root,
            &mol,
            &validated("claude"),
            LoopOwnership::External,
        )
        .unwrap();

        let fleet = store.load_fleet().unwrap();
        let worker = &fleet.workers[&wid];
        assert_eq!(worker.repo.as_deref(), Some("/other/place"));
    }

    #[test]
    fn test_register_tackle_worker_is_idempotent() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let wid = WorkerId::new("cosmon-test-mol").unwrap();
        let mol = sample_molecule("task-20260409-iiii", MoleculeStatus::Running);
        let repo_root = std::path::PathBuf::from("/projects/cosmon");
        let worktree = repo_root.join(".worktrees/task-20260409-iiii");

        register_tackle_worker(
            &store,
            &wid,
            &worktree,
            &repo_root,
            &mol,
            &validated("claude"),
            LoopOwnership::External,
        )
        .unwrap();
        register_tackle_worker(
            &store,
            &wid,
            &worktree,
            &repo_root,
            &mol,
            &validated("claude"),
            LoopOwnership::External,
        )
        .unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
    }

    #[test]
    fn test_register_tackle_worker_uses_assigned_role() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let wid = WorkerId::new("cosmon-test-role").unwrap();
        let mut mol = sample_molecule("task-20260409-role", MoleculeStatus::Running);
        mol.assigned_role = Some(AgentRole::Research);
        let repo_root = std::path::PathBuf::from("/projects/cosmon");
        let worktree = repo_root.join(".worktrees/task-20260409-role");

        register_tackle_worker(
            &store,
            &wid,
            &worktree,
            &repo_root,
            &mol,
            &validated("claude"),
            LoopOwnership::External,
        )
        .unwrap();

        let fleet = store.load_fleet().unwrap();
        let worker = &fleet.workers[&wid];
        assert_eq!(worker.role, AgentRole::Research);
    }

    #[test]
    fn test_register_tackle_worker_defaults_to_implementation() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let wid = WorkerId::new("cosmon-test-def").unwrap();
        let mol = sample_molecule("task-20260409-deft", MoleculeStatus::Running);
        assert!(mol.assigned_role.is_none());
        let repo_root = std::path::PathBuf::from("/projects/cosmon");
        let worktree = repo_root.join(".worktrees/task-20260409-deft");

        register_tackle_worker(
            &store,
            &wid,
            &worktree,
            &repo_root,
            &mol,
            &validated("claude"),
            LoopOwnership::External,
        )
        .unwrap();

        let fleet = store.load_fleet().unwrap();
        let worker = &fleet.workers[&wid];
        assert_eq!(worker.role, AgentRole::Implementation);
    }

    /// ADR-097 / C8 + ADR-099 / TS-0 — `spawn_and_prompt` cannot be
    /// reached with an unknown adapter from in-tree code because its
    /// `adapter: &ValidatedAdapterName` parameter has no `&str`
    /// constructor. The validation gate is the only producer of that
    /// type; this test pins the gate's rejection contract. The C8
    /// runtime catch-all inside `spawn_and_prompt` survives as a
    /// registry-completeness guard (a new adapter added to the
    /// registry but not wired in the match), which is a distinct
    /// invariant — covered structurally rather than at runtime by
    /// TS-0 (`spawn_and_prompt` would need an additional `&str` path
    /// to regress, and that path no longer exists).
    /// Tactical GAP #1 from an internal chronicle. The
    /// post-spawn pipeline calls `install_harvest_hook` and the tmux
    /// liveness re-check only when this predicate returns true; the
    /// inverse — both calls fire for Direct-API adapters — was the
    /// failure mode academy smoke
    /// `2026-05-18-grok-direct-api-smoke-result.md` surfaced. The dette
    /// restante (GAP #3) is to promote this decision to a typed
    /// `SupervisionMode` field on `ValidatedAdapterName`; until then,
    /// this string `match` IS the structural divergence and a test
    /// pins each adapter's verdict.
    /// GAP #6 — `finalize_inprocess_molecule` is the call site
    /// `cs tackle` invokes for in-process Direct-API adapters
    /// immediately after `spawn_and_prompt` returns Ok. It must drive
    /// the molecule from `Running` to `Completed` and produce the
    /// canonical event sequence; the structural pin lives here so a
    /// future refactor of the helper (e.g. when ADR-101's
    /// `SupervisionMode::InProcess` lands and the dispatch becomes
    /// typed instead of predicate-driven) cannot accidentally drop
    /// the completion-emit responsibility.
    ///
    /// The integration-test counterpart in
    /// `tests/tackle_inprocess_completion.rs` exercises the same
    /// contract through the `cs complete` CLI surface — together they
    /// pin the helper's contract from both sides of the function
    /// boundary.
    #[test]
    fn finalize_inprocess_molecule_drives_completion() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let mut mol = sample_molecule("task-20260518-gap6", MoleculeStatus::Running);
        // Match the state cs tackle step 9 leaves us in: Running, with
        // a bound process. The completion-emit must survive that
        // shape on disk.
        mol.bind_process(cosmon_core::process::MoleculeProcess::new(
            WorkerId::new("cosmon-test-gap6").unwrap(),
            "openai-inprocess-gap6".to_owned(),
        ));
        store.save_molecule(&mol.id, &mol).unwrap();

        let registry = ["claude".to_owned(), "openai".to_owned()];
        let (adapter, _supervision, _ownership) =
            validate_adapter_name("openai", &registry).unwrap();

        finalize_inprocess_molecule(&store, &state_dir, &mol.id, &adapter)
            .expect("finalize_inprocess_molecule must succeed on a Running molecule");

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(
            reloaded.status,
            MoleculeStatus::Completed,
            "GAP #6 — Running molecule must flip to Completed"
        );

        let events_raw =
            std::fs::read_to_string(state_dir.join("events.jsonl")).unwrap_or_default();
        let has_completed = events_raw
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|row| {
                row.get("type").and_then(|t| t.as_str()) == Some("molecule_completed")
                    && row.get("molecule_id").and_then(|id| id.as_str()) == Some(mol.id.as_str())
            });
        assert!(
            has_completed,
            "GAP #6 — events.jsonl must contain V2 molecule_completed after \
             finalize_inprocess_molecule fires. Events:\n{events_raw}"
        );

        // Idempotency: a second call (e.g. retry, double-tap) must
        // not error — `complete_one` short-circuits on already-completed.
        finalize_inprocess_molecule(&store, &state_dir, &mol.id, &adapter)
            .expect("finalize_inprocess_molecule must be idempotent");
    }

    /// ADR-103: the validator-and-toml resolver assigns the per-Adapter
    /// `LoopOwnership` axis. Built-in names take the validator's
    /// verdict; TOML-only adapters consult the row's `ownership`
    /// field; unknown strings degrade to the validator's verdict.
    #[test]
    fn resolve_loop_ownership_built_in_names_take_validator_verdict() {
        // Built-in `openai` is `Cosmon` regardless of TOML override —
        // the validator's table is authoritative for in-tree names so
        // an out-of-date TOML row cannot mis-route an in-tree adapter.
        let entry = AdapterEntry {
            ownership: Some("external".to_owned()),
            ..AdapterEntry::default()
        };
        let resolved = resolve_loop_ownership("openai", LoopOwnership::Cosmon, Some(&entry));
        assert_eq!(resolved, LoopOwnership::Cosmon);

        // Built-in `claude` is `External`.
        let resolved = resolve_loop_ownership("claude", LoopOwnership::External, None);
        assert_eq!(resolved, LoopOwnership::External);
    }

    #[test]
    fn resolve_loop_ownership_toml_only_adapter_reads_ownership_field() {
        let cosmon_entry = AdapterEntry {
            ownership: Some("cosmon".to_owned()),
            ..AdapterEntry::default()
        };
        let resolved = resolve_loop_ownership(
            "custom-toml-only",
            LoopOwnership::External,
            Some(&cosmon_entry),
        );
        assert_eq!(resolved, LoopOwnership::Cosmon);

        let external_entry = AdapterEntry {
            ownership: Some("external".to_owned()),
            ..AdapterEntry::default()
        };
        let resolved = resolve_loop_ownership(
            "custom-toml-only",
            LoopOwnership::External,
            Some(&external_entry),
        );
        assert_eq!(resolved, LoopOwnership::External);
    }

    #[test]
    fn resolve_loop_ownership_toml_only_adapter_absent_row_defaults_external() {
        // No row at all → validator default (External for caller-supplied names).
        let resolved = resolve_loop_ownership("custom-toml-only", LoopOwnership::External, None);
        assert_eq!(resolved, LoopOwnership::External);

        // Row present but no `ownership` field → same default.
        let entry = AdapterEntry::default();
        let resolved =
            resolve_loop_ownership("custom-toml-only", LoopOwnership::External, Some(&entry));
        assert_eq!(resolved, LoopOwnership::External);
    }

    #[test]
    fn resolve_loop_ownership_unknown_string_falls_back_to_validator() {
        let entry = AdapterEntry {
            ownership: Some("nonsense".to_owned()),
            ..AdapterEntry::default()
        };
        let resolved =
            resolve_loop_ownership("custom-toml-only", LoopOwnership::External, Some(&entry));
        assert_eq!(resolved, LoopOwnership::External);
    }

    #[test]
    fn adapter_uses_tmux_is_true_only_for_tmux_adapters() {
        let registry: Vec<String> = vec![
            "claude".into(),
            "aider".into(),
            "codex".into(),
            "openai".into(),
            "anthropic".into(),
        ];
        let v = |name: &str| {
            let (n, _supervision, _ownership) =
                validate_adapter_name(name, &registry).expect("name must be in test registry");
            n
        };

        assert!(adapter_uses_tmux(&v("claude")), "claude is tmux-backed");
        assert!(adapter_uses_tmux(&v("aider")), "aider is tmux-backed");
        assert!(
            adapter_uses_tmux(&v("codex")),
            "codex is tmux-backed per delib-20260518-5178 §D4"
        );
        assert!(
            !adapter_uses_tmux(&v("openai")),
            "openai is Direct-API (ADR-100 R2 wave 2) — no tmux session"
        );
        assert!(
            !adapter_uses_tmux(&v("anthropic")),
            "anthropic is Direct-API (ADR-100 R2 wave 3) — no tmux session"
        );
    }

    /// task-20260707-7d27 (hole #1): `ollama` is a built-in floor alias —
    /// it validates without a TOML row, resolves to the in-process floor
    /// axes (NOT the tmux/external legacy fallback), and is not tmux-backed.
    /// Before the fix `--adapter ollama` only validated via an
    /// `[adapters.ollama]` row and then died on the `spawn_and_prompt`
    /// catch-all.
    #[test]
    fn ollama_is_a_builtin_floor_alias() {
        // The built-in registry the real `tackle` composes must carry it.
        let registry: Vec<String> = cosmon_core::spawn_seam::built_in_adapter_names()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert!(
            registry.iter().any(|n| n == "ollama"),
            "ollama must be a built-in adapter name"
        );
        let (name, supervision, ownership) = validate_adapter_name("ollama", &registry)
            .expect("ollama validates against the built-in registry with no TOML row");
        assert!(
            !adapter_uses_tmux(&name),
            "ollama is an in-process floor alias, not tmux-backed"
        );
        assert!(
            !adapter_completes_inline(&name),
            "ollama's loop is detached; the parent tackle must return Running"
        );
        assert_eq!(
            supervision,
            cosmon_core::spawn_seam::SupervisionMode::InProcess,
            "ollama shares the local floor's in-process supervision"
        );
        assert_eq!(
            ownership,
            LoopOwnership::Cosmon,
            "ollama's loop is cosmon-owned like the local floor"
        );
    }

    /// task-20260707-7d27 (hole #3): the floor timeout precedence is
    /// config → env → 10-minute default, with zero/garbage inputs ignored
    /// so the timeout is never silently disabled (0 = no timeout on reqwest).
    #[test]
    fn resolve_local_timeout_secs_precedence() {
        // Config wins outright.
        assert_eq!(resolve_local_timeout_secs(Some(900), Some("120")), 900);
        // No config → env is honoured.
        assert_eq!(resolve_local_timeout_secs(None, Some("120")), 120);
        assert_eq!(resolve_local_timeout_secs(None, Some("  45  ")), 45);
        // Neither → the generous default (not the provider's 60 s).
        assert_eq!(
            resolve_local_timeout_secs(None, None),
            DEFAULT_LOCAL_TIMEOUT_SECS
        );
        assert!(
            DEFAULT_LOCAL_TIMEOUT_SECS > 60,
            "the whole point of hole #3 is a floor above the 60 s provider default"
        );
        // A zero or non-numeric value must NOT disable the timeout.
        assert_eq!(
            resolve_local_timeout_secs(Some(0), None),
            DEFAULT_LOCAL_TIMEOUT_SECS
        );
        assert_eq!(
            resolve_local_timeout_secs(None, Some("0")),
            DEFAULT_LOCAL_TIMEOUT_SECS
        );
        assert_eq!(
            resolve_local_timeout_secs(None, Some("nonsense")),
            DEFAULT_LOCAL_TIMEOUT_SECS
        );
        // Config zero falls through to env, then default.
        assert_eq!(resolve_local_timeout_secs(Some(0), Some("77")), 77);
    }

    #[test]
    fn test_validate_adapter_name_rejects_ghost() {
        let registry = vec!["claude".to_owned(), "aider".to_owned()];
        let err =
            validate_adapter_name("ghost-adapter", &registry).expect_err("ghost must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("ghost-adapter") && msg.contains("not declared"),
            "error must name the bad adapter and the verdict: {msg}",
        );
        assert!(err.available.iter().any(|n| n == "claude"));
        assert!(err.available.iter().any(|n| n == "aider"));
    }

    /// ADR-097 / C8 / WS-1 — `register_tackle_worker` threads the
    /// `adapter_name` argument into the `WorkerSpawned` event row it
    /// writes to fleet.json's sibling worker entry. The field appears
    /// on the inserted `WorkerData` (we cannot easily read
    /// `events.jsonl` here because the emit path walks up from
    /// `cwd`); the assertion below documents the contract that
    /// `register_tackle_worker` is the single writer that names the
    /// adapter on the fleet surface.
    #[test]
    fn test_register_tackle_worker_accepts_aider_adapter_name() {
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        let wid = WorkerId::new("cosmon-aider-c8").unwrap();
        let mol = sample_molecule("task-20260518-c8c8", MoleculeStatus::Running);
        let repo_root = std::path::PathBuf::from("/projects/cosmon");
        let worktree = repo_root.join(".worktrees/task-20260518-c8c8");

        // Both adapter names round-trip without panicking; the
        // `WorkerSpawned` emission is best-effort and silent on
        // missing events.jsonl, so the function must succeed.
        register_tackle_worker(
            &store,
            &wid,
            &worktree,
            &repo_root,
            &mol,
            &validated("aider"),
            LoopOwnership::External,
        )
        .unwrap();
        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.contains_key(&wid));
    }

    // ── Aider model resolution (task-20260615-f169) ───────────────────

    /// `[adapters.aider].default_model` is authoritative: when the config
    /// row carries a model, `aider_model` returns it verbatim — the whole
    /// point of the chip, so the terminal-REPL aider co-pilot can be aimed
    /// at Mistral (or any model) without recompiling. Config wins over both
    /// the env tier and the compile-time `kimi-k2.6`.
    #[test]
    fn aider_model_prefers_config_default_model() {
        let entry = AdapterEntry {
            default_model: Some("mistral/mistral-large-latest".to_owned()),
            ..AdapterEntry::default()
        };
        assert_eq!(
            aider_model(Some(&entry), None),
            "mistral/mistral-large-latest".to_owned()
        );
    }

    /// A `--model` / formula pin tops the aider chain, above the config
    /// `default_model` (adapter-uniform with the claude carrier;
    /// delib-20260704-b476 C1).
    #[test]
    fn aider_model_prefers_explicit_pin_over_config() {
        let entry = AdapterEntry {
            default_model: Some("mistral/mistral-large-latest".to_owned()),
            ..AdapterEntry::default()
        };
        assert_eq!(
            aider_model(Some(&entry), Some("anthropic/claude-fable-5")),
            "anthropic/claude-fable-5".to_owned()
        );
        // An empty pin is treated as unset and falls through to config.
        assert_eq!(
            aider_model(Some(&entry), Some("")),
            "mistral/mistral-large-latest".to_owned()
        );
    }

    /// With no config row carrying a model and no `AIDER_MODEL` in scope,
    /// `aider_model` falls back to the compile-time default. We construct
    /// the absent-config case (`None`) and a present-but-empty case to pin
    /// the `.clone()`-then-`or_else` chain; the env tier is process-global
    /// and left untested here to keep the suite parallel-safe.
    #[test]
    fn aider_model_falls_back_to_compile_time_default() {
        // Absent row → default (unless AIDER_MODEL is set in the ambient
        // env; guard the assertion on that so the test is deterministic in
        // a shell that exports AIDER_MODEL).
        if std::env::var("AIDER_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .is_none()
        {
            assert_eq!(aider_model(None, None), aider_default_model().to_owned());
        }
        // Row present but no model field → same fallback, independent of env
        // only when AIDER_MODEL is unset (same guard).
        let entry = AdapterEntry {
            default_model: None,
            ..AdapterEntry::default()
        };
        if std::env::var("AIDER_MODEL")
            .ok()
            .filter(|s| !s.is_empty())
            .is_none()
        {
            assert_eq!(
                aider_model(Some(&entry), None),
                aider_default_model().to_owned()
            );
        }
    }

    /// The compile-time default stays pinned to the historical
    /// `AiderAdapter::default_for_dispatch` choice.
    #[test]
    fn aider_default_model_is_kimi() {
        assert_eq!(aider_default_model(), "kimi-k2.6");
    }

    #[test]
    fn test_build_prompt_commit_only_forbids_push() {
        let mol = sample_molecule("task-20260407-cfg1", MoleculeStatus::Pending);
        let config = ProjectConfig::default(); // on_complete = Commit
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("Do NOT push to remote"));
        assert!(prompt.contains("Do NOT create GitHub PRs"));
        assert!(!prompt.contains("git push"));
    }

    #[test]
    fn test_build_prompt_commit_push_includes_push() {
        let mol = sample_molecule("task-20260407-cfg2", MoleculeStatus::Pending);
        let mut config = ProjectConfig::default();
        config.worker.on_complete = OnComplete::CommitPush;
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("git push -u origin HEAD"));
        assert!(prompt.contains("Do NOT create GitHub PRs"));
        assert!(!prompt.contains("Do NOT push to remote"));
    }

    #[test]
    fn test_build_prompt_empty_gates_renders_neutral_instruction() {
        let mol = sample_molecule("task-20260411-gates1", MoleculeStatus::Pending);
        let config = ProjectConfig::default();
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(!prompt.contains("cargo check"));
        assert!(!prompt.contains("cargo test"));
        assert!(prompt.contains("[gates]"));
        assert!(prompt.contains("verification gates"));
    }

    #[test]
    fn test_build_prompt_configured_gates_rendered_as_list() {
        let mol = sample_molecule("task-20260411-gates2", MoleculeStatus::Pending);
        let mut config = ProjectConfig::default();
        config.gates.build_command = Some("cargo check --workspace".to_owned());
        config.gates.test_command = Some("cargo test --workspace".to_owned());
        config.gates.lint_command = Some("cargo clippy --workspace -- -D warnings".to_owned());
        config.gates.format_command = Some("cargo fmt --all -- --check".to_owned());
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("cargo check --workspace"));
        assert!(prompt.contains("cargo test --workspace"));
        assert!(prompt.contains("cargo clippy --workspace -- -D warnings"));
        assert!(prompt.contains("cargo fmt --all -- --check"));
        // Ordering: build before test before lint before format.
        let idx_build = prompt.find("cargo check --workspace").unwrap();
        let idx_test = prompt.find("cargo test --workspace").unwrap();
        let idx_lint = prompt.find("cargo clippy").unwrap();
        let idx_fmt = prompt.find("cargo fmt").unwrap();
        assert!(idx_build < idx_test);
        assert!(idx_test < idx_lint);
        assert!(idx_lint < idx_fmt);
    }

    /// A declared `doc_command` reaches the worker's gate list. This is the
    /// whole point of the slot: a broken intra-doc link compiles, lints and
    /// tests green, so unless the doc build is named in the prompt the worker
    /// has no way to know it is owed one, and CI on main finds it instead.
    #[test]
    fn test_build_prompt_renders_doc_gate_after_format() {
        let mol = sample_molecule("task-20260719-gates4", MoleculeStatus::Pending);
        let mut config = ProjectConfig::default();
        config.gates.format_command = Some("cargo fmt --all -- --check".to_owned());
        config.gates.doc_command =
            Some("RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps".to_owned());
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("cargo doc --workspace --no-deps"));
        assert!(
            prompt.contains("RUSTDOCFLAGS='-D warnings'"),
            "the warnings-as-errors prefix is what makes the gate bite"
        );
        assert!(prompt.find("cargo fmt").unwrap() < prompt.find("cargo doc").unwrap());
    }

    /// `doc_command` alone is enough to render a concrete gate list — the slot
    /// counts toward `GatesConfig::is_empty`, so a galaxy that declares only a
    /// doc build does not fall through to the neutral "see CLAUDE.md" prompt.
    #[test]
    fn test_build_prompt_doc_gate_alone_is_not_empty_gates() {
        let mol = sample_molecule("task-20260719-gates5", MoleculeStatus::Pending);
        let mut config = ProjectConfig::default();
        config.gates.doc_command = Some("sphinx-build -W docs docs/_build".to_owned());
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("sphinx-build -W docs docs/_build"));
        assert!(!prompt.contains("or the project's CLAUDE.md"));
    }

    #[test]
    fn test_build_prompt_python_gates() {
        let mol = sample_molecule("task-20260411-gates3", MoleculeStatus::Pending);
        let mut config = ProjectConfig::default();
        config.gates.setup_command = Some("uv sync".to_owned());
        config.gates.test_command = Some("pytest".to_owned());
        config.gates.typecheck_command = Some("mypy .".to_owned());
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("uv sync"));
        assert!(prompt.contains("pytest"));
        assert!(prompt.contains("mypy ."));
        assert!(!prompt.contains("cargo"));
    }

    #[test]
    fn test_cargo_test_gate_carries_anti_stall_guidance() {
        let mut config = ProjectConfig::default();
        config.gates.test_command = Some("cargo test --workspace".to_owned());
        let out = render_gates_instruction(&config.gates);

        // The anti-stall note travels with the test gate.
        assert!(out.contains("anti-stall"));
        assert!(out.contains("timeout 600 cargo test --workspace"));
        // Cargo command → cargo-specific scoping hint.
        assert!(out.contains("cargo test -p <crate>"));
        // The merge contract is preserved, not weakened.
        assert!(out.contains("merge contract"));
    }

    #[test]
    fn test_non_cargo_test_gate_uses_generic_scoping_hint() {
        let mut config = ProjectConfig::default();
        config.gates.test_command = Some("pytest".to_owned());
        let out = render_gates_instruction(&config.gates);

        assert!(out.contains("anti-stall"));
        assert!(out.contains("timeout 600 pytest"));
        // No cargo-specific hint leaks into a non-cargo toolchain.
        assert!(!out.contains("cargo test -p"));
        assert!(out.contains("package / module you touched"));
    }

    #[test]
    fn test_no_test_gate_emits_no_stall_guidance() {
        let mut config = ProjectConfig::default();
        config.gates.build_command = Some("cargo check --workspace".to_owned());
        // No test_command configured.
        let out = render_gates_instruction(&config.gates);

        assert!(out.contains("cargo check --workspace"));
        assert!(!out.contains("anti-stall"));
    }

    #[test]
    fn test_build_prompt_commit_push_pr_includes_pr() {
        let mol = sample_molecule("task-20260407-cfg3", MoleculeStatus::Pending);
        let mut config = ProjectConfig::default();
        config.worker.on_complete = OnComplete::CommitPushPr;
        let prompt = build_prompt(
            &mol,
            None,
            None,
            &config,
            Path::new("/abs/state/fleets/default/molecules/idea-20260407-abcd"),
        );

        assert!(prompt.contains("git push -u origin HEAD"));
        assert!(prompt.contains("gh pr create"));
        assert!(!prompt.contains("Do NOT push to remote"));
        assert!(!prompt.contains("Do NOT create GitHub PRs"));
    }

    #[test]
    fn test_auto_inject_fleet_briefing_writes_briefing_md() {
        let tmp = TempDir::new().unwrap();
        // state_dir is .cosmon/state/ — create that structure.
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        fs::create_dir_all(&state_dir).unwrap();

        // Write a fleet.toml with an agent whose name matches formula_id.
        let fleet_toml = r#"
fleet = "test"
version = 1

[[agents]]
name = "idea-to-plan"
role = "implementation"
clearance = "write"
prompt = "You are a planner. Turn ideas into plans."
"#;
        fs::write(cosmon_dir.join("fleet.toml"), fleet_toml).unwrap();

        // Molecule with formula_id = "idea-to-plan" (matches the agent name).
        let mol = sample_molecule("idea-20260407-fleet", MoleculeStatus::Pending);

        // Molecule dir where briefing.md would be written.
        let mol_dir = state_dir
            .join("ops")
            .join("molecules")
            .join(mol.id.as_str());
        fs::create_dir_all(&mol_dir).unwrap();
        let briefing_path = mol_dir.join("briefing.md");

        let result = try_inject_fleet_briefing(&state_dir, &mol, &briefing_path);

        assert!(result.is_some(), "should auto-inject briefing");
        let briefing = result.unwrap();
        assert!(briefing.contains("# Molecule: idea-20260407-fleet"));
        assert!(briefing.contains("## Role"));
        assert!(briefing.contains("You are a planner. Turn ideas into plans."));

        // File should exist on disk.
        let on_disk = fs::read_to_string(&briefing_path).unwrap();
        assert_eq!(on_disk, briefing);
    }

    #[test]
    fn test_auto_inject_fleet_briefing_no_match_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        fs::create_dir_all(&state_dir).unwrap();

        // Fleet with an agent name that does NOT match formula_id.
        let fleet_toml = r#"
fleet = "test"
version = 1

[[agents]]
name = "reviewer"
role = "advisory"
clearance = "read"
prompt = "You review code."
"#;
        fs::write(cosmon_dir.join("fleet.toml"), fleet_toml).unwrap();

        let mol = sample_molecule("idea-20260407-nomatch", MoleculeStatus::Pending);
        let mol_dir = state_dir
            .join("ops")
            .join("molecules")
            .join(mol.id.as_str());
        fs::create_dir_all(&mol_dir).unwrap();
        let briefing_path = mol_dir.join("briefing.md");

        let result = try_inject_fleet_briefing(&state_dir, &mol, &briefing_path);
        assert!(result.is_none(), "no matching agent — should return None");
        assert!(!briefing_path.exists(), "briefing.md should not be created");
    }

    #[test]
    fn test_auto_inject_fleet_briefing_no_fleet_toml_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        // No fleet.toml written.

        let mol = sample_molecule("idea-20260407-nofile", MoleculeStatus::Pending);
        let briefing_path = state_dir.join("ops").join("molecules").join("briefing.md");

        let result = try_inject_fleet_briefing(&state_dir, &mol, &briefing_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_auto_inject_fleet_briefing_variable_override_path() {
        let tmp = TempDir::new().unwrap();
        let cosmon_dir = tmp.path().join(".cosmon");
        let state_dir = cosmon_dir.join("state");
        fs::create_dir_all(&state_dir).unwrap();

        // Write fleet.toml at a custom path.
        let custom_fleet = tmp.path().join("custom.fleet.toml");
        let fleet_toml = r#"
fleet = "custom"
version = 1

[[agents]]
name = "idea-to-plan"
role = "implementation"
clearance = "write"
prompt = "Custom fleet prompt."
"#;
        fs::write(&custom_fleet, fleet_toml).unwrap();

        let mut mol = sample_molecule("idea-20260407-cust", MoleculeStatus::Pending);
        mol.variables.insert(
            "fleet_template".to_owned(),
            custom_fleet.to_string_lossy().into_owned(),
        );

        let mol_dir = state_dir
            .join("ops")
            .join("molecules")
            .join(mol.id.as_str());
        fs::create_dir_all(&mol_dir).unwrap();
        let briefing_path = mol_dir.join("briefing.md");

        let result = try_inject_fleet_briefing(&state_dir, &mol, &briefing_path);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Custom fleet prompt."));
    }

    #[test]
    fn test_force_runtime_flag_emits_deprecation_no_op() {
        // `--force-runtime` on `cs tackle` is now meaningless: tackle
        // never routes to runtime mode. The flag is accepted (one-month
        // grace window) but emits a deprecation warning. Verify the
        // dispatch still completes the leaf path normally.
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("task-20260426-fdep", MoleculeStatus::Pending);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            molecule: "task-20260426-fdep".to_owned(),
            fleet: None,
            workdir: None,
            no_worktree: true,
            dry_run: true,
            permission_mode: None,
            force: false,
            name: None,
            leaf: false,
            force_runtime: true,
            bypass_seal: false,
            bypass_reason: None,
            adapter: None,
            model: None,
            role_hint: None,
            fallback_from_local: None,
            by: "human".to_owned(),
        };
        // Dry-run completes successfully even with the deprecated flag.
        let result = run(&ctx, &args);
        assert!(
            result.is_ok(),
            "tackle must accept --force-runtime as no-op"
        );
    }

    #[test]
    fn test_leaf_flag_silent_no_op() {
        // `--leaf` is the silent grace-window alias: `cs tackle` is now
        // always leaf, so the flag is a no-op kept for muscle memory.
        // Unlike --force-runtime, no warning is emitted (it would fire
        // on every runtime tick that still passes --leaf via the
        // SubprocessExecutor — see crates/cosmon-runtime/src/lib.rs).
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);
        let mol = sample_molecule("task-20260426-leaf", MoleculeStatus::Pending);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            molecule: "task-20260426-leaf".to_owned(),
            fleet: None,
            workdir: None,
            no_worktree: true,
            dry_run: true,
            permission_mode: None,
            force: false,
            name: None,
            leaf: true,
            force_runtime: false,
            bypass_seal: false,
            bypass_reason: None,
            adapter: None,
            model: None,
            role_hint: None,
            fallback_from_local: None,
            by: "human".to_owned(),
        };
        let result = run(&ctx, &args);
        assert!(result.is_ok(), "tackle must accept --leaf silently");
    }

    #[test]
    fn test_tackle_with_active_blocks_edges_stays_leaf() {
        // Critical regression check for delib-20260426-1bcd #2:
        // a molecule with outgoing Blocks edges to live targets must
        // NOT auto-route to runtime mode. `cs tackle` is always leaf.
        // Walking a DAG of N≥1 nodes is exclusively `cs run`'s job.
        let (_tmp, state_dir) = make_store();
        let store = FileStore::new(&state_dir);

        // Build a DAG: parent --Blocks--> child (both pending).
        let child = sample_molecule("task-20260426-d2ch", MoleculeStatus::Pending);
        store.save_molecule(&child.id, &child).unwrap();

        let mut parent = sample_molecule("task-20260426-d2pa", MoleculeStatus::Pending);
        parent
            .typed_links
            .push(cosmon_core::interaction::MoleculeLink::Blocks {
                target: child.id.clone(),
            });
        store.save_molecule(&parent.id, &parent).unwrap();

        // Tackling the parent must NOT spawn a runtime — dry-run goes
        // straight to the leaf prompt printer.
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(state_dir),
        };
        let args = Args {
            molecule: "task-20260426-d2pa".to_owned(),
            fleet: None,
            workdir: None,
            no_worktree: true,
            dry_run: true,
            permission_mode: None,
            force: false,
            name: None,
            leaf: false,
            force_runtime: false,
            bypass_seal: false,
            bypass_reason: None,
            adapter: None,
            model: None,
            role_hint: None,
            fallback_from_local: None,
            by: "human".to_owned(),
        };
        let result = run(&ctx, &args);
        assert!(
            result.is_ok(),
            "tackle on DAG-root molecule must succeed as leaf, not runtime"
        );
    }

    /// GAP #2 — `SF6SupervisionSetupFailed` is emitted to events.jsonl
    /// when the post-spawn supervision step fails, preserving the
    /// worktree and branch on disk. Pins three properties of the new
    /// preserve-on-success contract chronicled in
    /// `2026-05-18-cleanup-preserve-on-success.md`:
    ///
    /// 1. The event lands in events.jsonl with the `snake_case`
    ///    discriminator the audit query depends on.
    /// 2. The forensic fields (`adapter_name`, `hook_name`, `error`)
    ///    survive the round-trip and are recoverable by an operator
    ///    audit.
    /// 3. A simulated worktree path passed in as a sibling check
    ///    *still exists* after the helper returns — i.e. the new code
    ///    path does **not** invoke `cleanup_partial_tackle`. This is
    ///    the structural counter-measure for the academy-smoke regression
    ///    where a successful Direct-API
    ///    spawn had its haiku wiped by the old `?`-propagating
    ///    supervision-mandatory contract.
    #[test]
    fn sf6_supervision_failure_preserves_worktree_and_emits_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events_path = tmp.path().join("events.jsonl");
        // Simulate a worktree the spawn already created. The new
        // helper must NOT delete it; the only legitimate eraser is
        // `cleanup_partial_tackle`, which we explicitly do not call.
        let worktree = tmp.path().join("worktrees/task-test");
        std::fs::create_dir_all(&worktree).unwrap();
        let haiku = worktree.join("haiku.md");
        std::fs::write(&haiku, "agent's first artefact").unwrap();

        let mol_id = MoleculeId::new("task-20260518-aec3").unwrap();
        let wid = WorkerId::new("cosmon-anthropic-test").unwrap();

        emit_supervision_setup_failed_event_to(
            &events_path,
            &mol_id,
            &wid,
            "anthropic",
            "pane_died",
            "tmux: no server running on socket",
        );

        // (a) worktree + the agent's artefact still exist on disk.
        assert!(worktree.exists(), "worktree must be preserved");
        assert!(haiku.exists(), "agent's artefact must be preserved");
        assert_eq!(
            std::fs::read_to_string(&haiku).unwrap(),
            "agent's first artefact"
        );

        // (b) the SF-6 event landed in events.jsonl with all forensic
        //     fields recoverable by an audit query.
        let log = std::fs::read_to_string(&events_path).expect("events.jsonl must exist");
        assert!(
            log.contains("\"type\":\"sf6_supervision_setup_failed\""),
            "expected snake_case discriminator on the wire: {log}"
        );
        for field in ["anthropic", "pane_died", "no server running"] {
            assert!(
                log.contains(field),
                "forensic field `{field}` must survive serialisation: {log}"
            );
        }
        assert!(
            log.contains(mol_id.as_str()),
            "event must carry mol_id for audit attribution"
        );
    }

    /// GAP #2 sibling — over-long error strings are truncated at a
    /// UTF-8 boundary so the events.jsonl row size stays bounded and
    /// the JSON remains valid even when the underlying tmux error
    /// embeds large multibyte payloads (Unicode quote marks, error
    /// classes with non-ASCII separators). The truncation marker `…`
    /// is preserved so an audit can tell at a glance the field was
    /// clipped.
    #[test]
    fn truncate_at_utf8_boundary_preserves_validity_and_marks_clip() {
        // ASCII under the cap — no truncation, no marker.
        let small = "short error";
        assert_eq!(truncate_at_utf8_boundary(small, 500), "short error");

        // Over the cap — truncated, marker appended.
        let big: String = "x".repeat(600);
        let out = truncate_at_utf8_boundary(&big, 500);
        assert!(out.len() <= 500 + '…'.len_utf8());
        assert!(out.ends_with('…'));
        assert!(out.starts_with("xxxx"));

        // A multibyte codepoint straddling the cut would otherwise
        // panic the slice — verify the boundary walk-back.
        let mut mixed = "a".repeat(499);
        mixed.push('é'); // 2 bytes — straddles 499..501
        mixed.push_str("trailing");
        let out = truncate_at_utf8_boundary(&mixed, 500);
        assert!(
            out.is_char_boundary(out.len()),
            "result must end on a valid UTF-8 boundary"
        );
        assert!(out.ends_with('…'));
    }

    // -- Spawn proof-of-life window + no-teardown affordance --
    // (task-20260602-ef26: cold-container 503 root cause)

    #[test]
    fn spawn_window_defaults_to_twelve_seconds() {
        // No override → the widened default (was 2 s, see
        // DEFAULT_SPAWN_POSTCONDITION_SECS).
        let w = spawn_postcondition_window(|_| None);
        assert_eq!(w, std::time::Duration::from_secs(12));
        assert_eq!(w.as_secs(), DEFAULT_SPAWN_POSTCONDITION_SECS);
    }

    #[test]
    fn spawn_window_honours_env_override() {
        let w = spawn_postcondition_window(|k| {
            (k == "COSMON_SPAWN_POSTCONDITION_SECS").then(|| "30".to_owned())
        });
        assert_eq!(w, std::time::Duration::from_secs(30));
    }

    #[test]
    fn spawn_window_ignores_zero_and_garbage() {
        // Zero would disable the gate — refuse it and keep the default.
        let zero = spawn_postcondition_window(|k| {
            (k == "COSMON_SPAWN_POSTCONDITION_SECS").then(|| "0".to_owned())
        });
        assert_eq!(zero.as_secs(), DEFAULT_SPAWN_POSTCONDITION_SECS);
        // Unparseable → default.
        let garbage = spawn_postcondition_window(|k| {
            (k == "COSMON_SPAWN_POSTCONDITION_SECS").then(|| "soon".to_owned())
        });
        assert_eq!(garbage.as_secs(), DEFAULT_SPAWN_POSTCONDITION_SECS);
        // Whitespace is trimmed.
        let padded = spawn_postcondition_window(|k| {
            (k == "COSMON_SPAWN_POSTCONDITION_SECS").then(|| "  15 ".to_owned())
        });
        assert_eq!(padded.as_secs(), 15);
    }

    #[test]
    fn no_teardown_off_by_default() {
        assert!(!spawn_no_teardown(|_| None));
    }

    #[test]
    fn no_teardown_accepts_one_and_true() {
        assert!(spawn_no_teardown(
            |k| (k == "COSMON_SPAWN_NO_TEARDOWN").then(|| "1".to_owned())
        ));
        assert!(spawn_no_teardown(
            |k| (k == "COSMON_SPAWN_NO_TEARDOWN").then(|| "true".to_owned())
        ));
        assert!(spawn_no_teardown(
            |k| (k == "COSMON_SPAWN_NO_TEARDOWN").then(|| "TRUE".to_owned())
        ));
        assert!(spawn_no_teardown(
            |k| (k == "COSMON_SPAWN_NO_TEARDOWN").then(|| "  1 ".to_owned())
        ));
    }

    #[test]
    fn no_teardown_rejects_other_values() {
        assert!(!spawn_no_teardown(
            |k| (k == "COSMON_SPAWN_NO_TEARDOWN").then(|| "0".to_owned())
        ));
        assert!(!spawn_no_teardown(
            |k| (k == "COSMON_SPAWN_NO_TEARDOWN").then(|| "yes".to_owned())
        ));
        assert!(!spawn_no_teardown(
            |k| (k == "COSMON_SPAWN_NO_TEARDOWN").then(String::new)
        ));
    }
}
