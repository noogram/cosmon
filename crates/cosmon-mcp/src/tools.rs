// SPDX-License-Identifier: AGPL-3.0-only

//! MCP tool implementations wrapping Cosmon domain logic.
//!
//! Each tool corresponds to a CLI command, calling the same domain functions
//! from `cosmon-core` and persisting via `cosmon-filestore`.
//!
//! Tools are organized in two groups:
//!
//! **Lifecycle tools** (mutations): `cosmon_nucleate`, `cosmon_evolve`,
//! `cosmon_observe`, `cosmon_ensemble`, `cosmon_freeze`, `cosmon_thaw`,
//! `cosmon_collapse`, `cosmon_complete`.
//!
//! **Query tools** (read-only, archive-service pattern): `cosmon_search`,
//! `cosmon_get`, `cosmon_list`, `cosmon_count`, `cosmon_export`,
//! `cosmon_stats`, `cosmon_aggregate`, `cosmon_energy`,
//! `cosmon_fleet_templates`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use cosmon_core::energy::BudgetPeriod;
use cosmon_core::evolve::{self, EvolveRequest, NewState};
use cosmon_core::formula::Formula;
use cosmon_core::id::{FormulaId, MoleculeId, WorkerId};
use cosmon_core::interaction::MoleculeLink;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::nucleate;
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::FileStore;
use cosmon_state::file_energy_tracker::FileEnergyTracker;
use cosmon_state::wait::{coupling_report_snapshot, wait_for_status_with_metrics, WaitError};
use cosmon_state::{EnergyTracker, MoleculeData, MoleculeFilter, StateStore};
use rmcp::{
    handler::server::{tool::ToolCallContext, tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerInfo, Tool,
    },
    schemars,
    service::RequestContext,
    tool, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};
use tokio::task_local;

// ---------------------------------------------------------------------------
// Parameter types (schemars derives generate JSON Schema for MCP)
// ---------------------------------------------------------------------------

/// Parameters for nucleating a new molecule.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NucleateParams {
    /// Formula name — looks for {name}.formula.toml in the formulas directory.
    pub formula: String,
    /// Fleet to nucleate the molecule into (default: "default").
    pub fleet: Option<String>,
    /// Worker ID to assign the new molecule to (optional).
    pub assign: Option<String>,
    /// Variables to bind, as a JSON object {"key": "value"}.
    pub vars: Option<HashMap<String, String>>,
    /// Target molecule IDs that this new molecule blocks — each target
    /// cannot progress until this one completes. Symmetric: every target
    /// also gains a `BlockedBy` link pointing back at the new molecule.
    /// Targets must already exist. (ADR-016 Phase 1.)
    pub blocks: Option<Vec<String>>,
    /// Source molecule IDs that block this new molecule — this molecule
    /// cannot progress until each source completes. Symmetric counterpart
    /// of `blocks`: each source gains a Blocks link pointing at the new
    /// molecule. Sources must already exist.
    pub blocked_by: Option<Vec<String>>,
    /// Agent role for the worker that will tackle this molecule. Valid
    /// roles: orchestration, research, implementation, infrastructure,
    /// advisory, validation. When set, `cs tackle` uses this role instead
    /// of the default `implementation`.
    pub role: Option<String>,
    /// Caller's working directory. When set, the MCP server resolves the
    /// formulas directory by walking up from this path instead of from the
    /// long-lived server's own CWD. This is how clients in different
    /// projects reach their own `.cosmon/formulas/`. Absent = fall through
    /// to the server's startup-time `formulas_dir` (backward compatible).
    pub cwd: Option<String>,
}

/// Parameters for evolving a molecule.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EvolveParams {
    /// Molecule ID to advance to the next step.
    pub molecule: String,
    /// Evidence documenting why the current step is complete.
    pub evidence: String,
    /// Path to the formula TOML file.
    pub formula_path: String,
    /// Caller's working directory. Resolves state (and any walk-up lookup)
    /// against this path instead of the long-lived server's own CWD, so a
    /// single MCP server can serve requests from multiple project cwds.
    /// Absent = fall through to the server's startup-time state dir.
    pub cwd: Option<String>,
}

/// Parameters for observing a molecule.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ObserveParams {
    /// Molecule ID to inspect.
    pub molecule: String,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for collapsing a molecule.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CollapseParams {
    /// Molecule ID to mark as failed.
    pub molecule: String,
    /// Reason for the molecule failure.
    pub reason: String,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for completing a molecule (shortcut — no evolve ceremony).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CompleteParams {
    /// Molecule ID to mark as completed. For batch mode, pass a comma-separated list.
    pub molecule: String,
    /// Reason for completion (recorded in the log). Defaults to "completed via MCP".
    pub reason: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for freezing a molecule.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FreezeParams {
    /// Molecule ID to pause.
    pub molecule: String,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for thawing a molecule.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ThawParams {
    /// Molecule ID to resume.
    pub molecule: String,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for decaying a molecule (1 → N).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DecayParams {
    /// Source molecule ID to decay.
    pub source: String,
    /// Formula for the product molecules.
    pub formula: String,
    /// Number of products to create (default: 1).
    pub count: Option<usize>,
    /// Kind for the product molecules (default: "task").
    pub product_kind: Option<String>,
    /// Reason for the decay.
    pub reason: String,
    /// Caller's working directory — resolves state and formulas per-call.
    /// See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for merging molecules (N → 1).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MergeParams {
    /// Source molecule IDs to merge.
    pub sources: Vec<String>,
    /// Formula for the product molecule.
    pub formula: String,
    /// Kind for the product (default: "decision").
    pub product_kind: Option<String>,
    /// Reason for the merge.
    pub reason: String,
    /// Caller's working directory — resolves state and formulas per-call.
    /// See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for transforming a molecule's kind.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TransformParams {
    /// Molecule ID to transform.
    pub molecule: String,
    /// Target kind (idea, task, decision, issue).
    pub to: String,
    /// Reason for the transform.
    pub reason: String,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for filtering the ensemble view.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EnsembleParams {
    /// Filter molecules by status: active, frozen, completed, collapsed.
    pub status: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for nudging a worker (sending a message to its tmux session).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NudgeParams {
    /// Worker ID to nudge.
    pub worker: String,
    /// Message to send to the worker's session.
    pub message: String,
}

/// Parameters for declaring an agent's cognitive state.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeclareParams {
    /// The agent's current cognitive status: working, waiting, done, idle, error.
    pub status: String,
    /// Optional detail about what the agent is doing or waiting for.
    pub detail: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for logging energy (token) consumption.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EnergyLogParams {
    /// Approximate input tokens consumed in this action.
    pub input_tokens: u64,
    /// Approximate output tokens consumed in this action.
    pub output_tokens: u64,
    /// What the tokens were spent on (e.g. "wrote article on LOB").
    pub description: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

// ---------------------------------------------------------------------------
// Query tool parameter types (archive-service pattern)
// ---------------------------------------------------------------------------

/// Parameters for searching molecules by text.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Text to search for across molecule IDs, formula names, variables, and worker names.
    pub query: String,
    /// Maximum number of results to return (default: 50).
    pub limit: Option<usize>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for getting a single molecule by ID.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetParams {
    /// Molecule ID to retrieve.
    pub id: String,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for listing molecules with filters.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ListParams {
    /// Filter by status: active, frozen, completed, collapsed.
    pub status: Option<String>,
    /// Filter by assigned worker ID.
    pub worker: Option<String>,
    /// Filter by formula ID.
    pub formula: Option<String>,
    /// Sort field: `created_at`, `updated_at`, status (default: `updated_at`).
    pub sort_by: Option<String>,
    /// Sort order: asc or desc (default: desc).
    pub order: Option<String>,
    /// Maximum number of results (default: 50).
    pub limit: Option<usize>,
    /// Number of results to skip for pagination (default: 0).
    pub offset: Option<usize>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for counting molecules.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CountParams {
    /// Filter by status: active, frozen, completed, collapsed.
    pub status: Option<String>,
    /// Filter by assigned worker ID.
    pub worker: Option<String>,
    /// Filter by formula ID.
    pub formula: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for exporting molecules.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportParams {
    /// Export format: json (array), ndjson (newline-delimited), or csv.
    pub format: String,
    /// Filter by status: active, frozen, completed, collapsed.
    pub status: Option<String>,
    /// Filter by assigned worker ID.
    pub worker: Option<String>,
    /// Filter by formula ID.
    pub formula: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for system statistics.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatsParams {
    /// Optional: restrict stats to a specific formula.
    pub formula: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for aggregating molecules.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AggregateParams {
    /// Field to group by: status, formula, worker.
    pub group_by: String,
    /// Filter by status before aggregating.
    pub status: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for waiting on a molecule to reach a target status set.
///
/// Defaults mirror the `cs wait` CLI: wait for a terminal status (completed
/// or collapsed), ten-minute budget, five-second poll interval.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitParams {
    /// Molecule ID to wait on — must be an exact ID.
    pub molecule: String,
    /// Statuses that satisfy the wait. Defaults to `["completed","collapsed"]`.
    #[serde(default)]
    pub r#for: Option<Vec<String>>,
    /// Maximum seconds to wait before returning a timeout error (default 600).
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Seconds between polls (default 5, clamped to the remaining budget).
    #[serde(default)]
    pub poll_interval_seconds: Option<u64>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Parameters for energy consumption report.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EnergyParams {
    /// Report period: weekly, monthly, or a molecule ID for per-molecule report.
    pub period: Option<String>,
    /// Caller's working directory — resolves state per-call. See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

/// Parameters for listing available formula templates.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FleetTemplatesParams {
    /// Caller's working directory — resolves the formulas directory per-call.
    /// See `EvolveParams::cwd`.
    pub cwd: Option<String>,
}

// ---------------------------------------------------------------------------
// Remote tool-exposure partition (deny-remote set)
// ---------------------------------------------------------------------------

/// Tools that are **never registered on a public (remote) `/mcp` connector**
/// — the deny-remote set of turing's partition (delib-20260709-943e M3).
///
/// The `#[tool_router]` macro registers *every* `#[tool]` method on
/// [`CosmonService`]; that full set is the complete macro-generated surface
/// ([`CosmonService::new`]) — historically the trusted local stdio path
/// (`cs mcp`, same host, same operator), removed 2026-07-12 (C14), now only
/// the base the remote connector filters down from. A remote connector reached
/// over the network (Claude Desktop / claude.ai over Tailscale) must expose
/// a strictly smaller surface: the worker-internal and teardown verbs are
/// removed from the tool list entirely by [`CosmonService::new_remote`].
///
/// The doctrine is turing exploit #1's defense — *"absent tools cannot be
/// injection targets."* A verb that is not in `tools/list` cannot be called,
/// so a prompt-injected client cannot forge a proof-of-work chain, merge to a
/// trunk, or perturb a live worker. Denial-by-absence is stronger than
/// denial-by-scope for this class because it removes the surface, not just
/// the grant.
///
/// # Membership rationale (each entry, one class)
///
/// - `cosmon_evolve`, `cosmon_complete` — **worker-internal**: they forge the
///   proof-of-work artifact chain a molecule accrues *as its own worker runs*.
///   A remote caller is the REQUESTER of work, never its executor; letting a
///   remote client evolve/complete would let it fabricate a completed
///   molecule that no worker actually produced.
/// - `cosmon_nudge` — **worker-supervision**: injects text into a worker's
///   live tmux session. A remote caller has no worker to supervise, and the
///   verb is a direct text-into-a-running-agent channel (injection amplifier).
/// - `cosmon_declare`, `cosmon_energy_log` — **worker self-report**: a worker
///   declaring its own cognitive state / logging its own token spend, keyed
///   off `COSMON_WORKER_ID`. A remote connector has no worker identity; these
///   would scribble bogus self-reports into the server's own state dir.
///
/// The panel's wider deny-remote list also names `done`, `whisper`, and
/// (as a *gated*, not denied, verb) `tackle`. Those three have **no `#[tool]`
/// counterpart in this crate** — they are worker-teardown / human-pilot /
/// worker-spawn verbs that were never exposed over MCP. They are therefore
/// deny-by-absence today; if one is ever added it MUST be registered only
/// after passing the host adapter's scope + ceiling gate (see
/// `crates/cosmon-rpp-adapter/src/routes/mcp.rs`), never by falling into the
/// default `#[tool_router]` set.
pub const DENY_REMOTE_TOOLS: &[&str] = &[
    "cosmon_evolve",
    "cosmon_complete",
    "cosmon_nudge",
    "cosmon_declare",
    "cosmon_energy_log",
];

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// A per-request pin that forces state / formulas / config resolution to a
/// fixed tenant root, rendering every tool's `cwd` parameter **inert**.
///
/// # Why this exists (tenant-isolation seam, delib-20260709-943e conv. #6)
///
/// On the stdio path the `cwd` parameter is a legitimate walk-up hook: a
/// single long-lived MCP server serves several project directories, and each
/// call names its own project via `cwd`. On the **multi-tenant HTTP path**
/// that same parameter is a tenant-spoofing vector — a client could pass
/// another noyau's filesystem path, walk-up would load *their* state, and the
/// BLAKE3 `(iss,sub)→noyau` seal in
/// `crates/cosmon-rpp-adapter/src/nucleon_map.rs` would be bypassed.
///
/// The host (`cosmon-rpp-adapter`) closes the door structurally: it resolves
/// the tenant state directory **only** from the validated JWT's noyau
/// (`authorise_scope → resolve_for_audience → spark.noyau → tenant_root`),
/// builds an `HttpStatePin`, and inserts it into the request extensions. Every
/// tool then resolves against the pin and **ignores** its `cwd` argument. The
/// pin is re-derived per request by the gate (never cached in the MCP
/// session — panel D1), so the tenant boundary is the only door.
#[derive(Debug, Clone)]
pub struct HttpStatePin {
    /// Tenant `.cosmon/state` directory
    /// (`<galaxies_root>/<noyau>/.cosmon/state`).
    state_dir: PathBuf,
    /// Tenant `.cosmon/formulas` directory.
    formulas_dir: PathBuf,
}

impl HttpStatePin {
    /// Construct a pin from the host-resolved tenant `state_dir` and
    /// `formulas_dir`. Both paths must derive from `spark.noyau`, never from
    /// any client-supplied value — that is the whole point of the seam.
    #[must_use]
    pub fn new(state_dir: PathBuf, formulas_dir: PathBuf) -> Self {
        Self {
            state_dir,
            formulas_dir,
        }
    }

    /// The tenant config path (`<tenant>/.cosmon/config.toml`), derived from
    /// the pinned state dir (`<tenant>/.cosmon/state`).
    fn config_path(&self) -> PathBuf {
        self.state_dir
            .parent()
            .map_or_else(|| PathBuf::from("config.toml"), Path::to_path_buf)
            .join("config.toml")
    }
}

task_local! {
    /// The tenant pin active for the current MCP tool dispatch, if the host
    /// installed one. `None`/unset on the stdio path, where `cwd` walk-up
    /// resolution is preserved byte-for-byte.
    static HTTP_STATE_PIN: Option<HttpStatePin>;
}

/// The Cosmon MCP service — holds configuration and provides tool handlers.
#[derive(Clone)]
pub struct CosmonService {
    /// Root of the state store (default: .cosmon).
    store_dir: Arc<PathBuf>,
    /// Directory containing formula TOML files (default: formulas).
    formulas_dir: Arc<PathBuf>,
    /// Tool router generated by the `#[tool_router]` macro.
    tool_router: ToolRouter<Self>,
}

impl Default for CosmonService {
    fn default() -> Self {
        Self::new()
    }
}

impl CosmonService {
    /// Create a new service with default directories.
    ///
    /// Uses [`cosmon_filestore::resolve_state_dir`] and
    /// [`cosmon_filestore::resolve_formulas_dir`] for unified walk-up
    /// discovery (same logic as the CLI).
    #[must_use]
    pub fn new() -> Self {
        let store_dir = cosmon_filestore::resolve_state_dir(None);
        let formulas_dir = cosmon_filestore::resolve_formulas_dir(None);

        Self {
            store_dir: Arc::new(store_dir),
            formulas_dir: Arc::new(formulas_dir),
            tool_router: Self::tool_router(),
        }
    }

    /// Create a service for a **remote (public) connector**, exposing only
    /// the remote-safe tool partition.
    ///
    /// Identical to [`Self::new`] except the [`DENY_REMOTE_TOOLS`] set is
    /// stripped from the generated [`ToolRouter`] — those verbs disappear
    /// from `tools/list` and become uncallable over this transport. Use this
    /// for any connector reachable over the network; [`Self::new`] retains the
    /// full macro-generated surface as the base this filters from (the legacy
    /// local stdio server that served it directly, `cs mcp`, was removed
    /// 2026-07-12 — decision C14).
    ///
    /// The removal is done on the fully-built router rather than by
    /// conditionally emitting `#[tool]` methods, so the two surfaces share
    /// one source of truth (the macro-generated set) and can never drift:
    /// adding a new `#[tool]` method automatically appears on both, and the
    /// partition is the single explicit subtraction here.
    #[must_use]
    pub fn new_remote() -> Self {
        let mut svc = Self::new();
        let mut router = svc.tool_router;
        for name in DENY_REMOTE_TOOLS {
            router.remove_route(name);
        }
        svc.tool_router = router;
        svc
    }

    fn store(&self) -> FileStore {
        FileStore::new(self.store_dir.as_path())
    }

    /// The tenant pin active for the current dispatch, if the HTTP host
    /// installed one. Returns `None` on the stdio path (the task-local is
    /// unset, so `try_with` errors and we fall through to `cwd` walk-up).
    fn active_pin() -> Option<HttpStatePin> {
        HTTP_STATE_PIN.try_with(Clone::clone).ok().flatten()
    }

    /// Resolve the caller's state directory for a single request.
    ///
    /// **Tenant pin takes absolute precedence.** When the HTTP host installed
    /// an [`HttpStatePin`] the `cwd` argument is ignored entirely — the state
    /// dir comes from the JWT's noyau and nothing else (the seam that makes
    /// `cwd` inert). Otherwise (stdio path): when `cwd` is supplied, walk up
    /// from that path to find the enclosing `.cosmon/state/`; absent = the
    /// server's startup-time state dir (backward compatible).
    fn state_dir_for(&self, cwd: Option<&str>) -> PathBuf {
        if let Some(pin) = Self::active_pin() {
            return pin.state_dir;
        }
        cwd.map_or_else(
            || self.store_dir.as_path().to_path_buf(),
            |c| cosmon_filestore::resolve_state_dir_from(Path::new(c)),
        )
    }

    /// Build a `FileStore` rooted at the caller's resolved state dir.
    /// Companion to [`Self::state_dir_for`].
    fn store_for(&self, cwd: Option<&str>) -> FileStore {
        FileStore::new(self.state_dir_for(cwd))
    }

    /// Resolve the caller's formulas directory for a single request.
    /// Mirrors [`Self::state_dir_for`] for formula TOML lookup — the tenant
    /// pin, when present, overrides `cwd`.
    fn formulas_dir_for(&self, cwd: Option<&str>) -> PathBuf {
        if let Some(pin) = Self::active_pin() {
            return pin.formulas_dir;
        }
        cwd.map_or_else(
            || self.formulas_dir.as_path().to_path_buf(),
            |c| cosmon_filestore::resolve_formulas_dir_from(Path::new(c)),
        )
    }

    /// Resolve the caller's `config.toml` path for a single request.
    /// Mirrors [`Self::state_dir_for`] — the tenant pin, when present,
    /// pins the config read to the tenant root so `project_id` scoping
    /// cannot read a foreign tenant's config either.
    ///
    /// Takes `&self` for call-site symmetry with the other `*_for`
    /// resolvers even though neither branch reads instance state (the
    /// fallback resolves from the ambient env / walk-up, not from a
    /// startup-time field).
    #[allow(clippy::unused_self)]
    fn config_path_for(&self, cwd: Option<&str>) -> PathBuf {
        if let Some(pin) = Self::active_pin() {
            return pin.config_path();
        }
        cwd.map_or_else(
            || cosmon_filestore::resolve_config_path(None),
            |c| cosmon_filestore::resolve_config_path_from(Path::new(c)),
        )
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl CosmonService {
    /// Create a molecule (work unit) from a formula template.
    #[tool(
        description = "Create a molecule (work unit) from a formula template. IMPORTANT: A molecule does nothing on its own — it must be assigned to a running worker via the 'assign' parameter. If assigned_worker is null in the response, the molecule is PENDING and no agent will process it. PREREQUISITES: Check cosmon_ensemble first to verify workers exist. WORKFLOW: cosmon_ensemble (check fleet) → cosmon_nucleate (create + assign) → cosmon_evolve (advance steps). Returns molecule ID, status, and next steps. MOLECULE KINDS (passed via 'kind' param): idea, task, decision, issue, signal, deliberation — the cognitive nature of the molecule, orthogonal to the formula. Use 'deliberation' with the 'deep-think' formula to run a structured multi-perspective panel. SURFACE SYNC: Surfaces (STATUS.md, ISSUES.md, IDEAS.md, DELIBERATIONS.md, GitHub Issues) are auto-generated from state — after this call (or a batch of mutations), run `cs reconcile` to refresh them. DO NOT edit those files directly; edits are overwritten. BLOCKING DEPENDENCIES (ADR-016): Use 'blocks' to declare molecules this one blocks (they cannot progress until this completes), or 'blocked_by' for the reverse direction. Symmetry is maintained automatically — every referenced target gains the reciprocal link. Targets must already exist. Use cosmon_observe to inspect the full DAG after creation. CHILDREN OF A DELIBERATION (deep-think path 1): when nucleating children from a deliberation outcomes step, you MUST pass 'blocked_by=[delib_id]' to establish the typed MoleculeLink::BlockedBy edge — textual references inside the child topic are NOT sufficient. `cs deps --transitive` and the DagPolicy walker follow only typed edges; a parent id mentioned in free text is invisible to the graph. Regression: delib-20260409-b22c produced three orphaned children that did not appear in `cs deps --transitive` because this flag was missed."
    )]
    #[allow(clippy::too_many_lines)]
    fn cosmon_nucleate(
        &self,
        Parameters(params): Parameters<NucleateParams>,
    ) -> Result<CallToolResult, McpError> {
        // Per-call formulas dir: if the caller supplied their cwd, walk up
        // from there to find the right project's `.cosmon/formulas/`.
        // Otherwise fall through to the server's startup-time default.
        let formulas_dir = self.formulas_dir_for(params.cwd.as_deref());
        let formula_path = formulas_dir.join(format!("{}.formula.toml", params.formula));

        let toml_text = std::fs::read_to_string(&formula_path).map_err(|e| {
            McpError::invalid_params(
                format!("formula not found: {} ({})", params.formula, e),
                None,
            )
        })?;

        let formula = Formula::parse(&toml_text)
            .map_err(|e| McpError::invalid_params(format!("invalid formula: {e}"), None))?;

        let variables = params.vars.unwrap_or_default();

        let assign = params
            .assign
            .as_deref()
            .map(WorkerId::new)
            .transpose()
            .map_err(|e| McpError::invalid_params(format!("invalid worker id: {e}"), None))?;

        // Parse and validate blocking link parameters BEFORE nucleation, so
        // invalid IDs or unknown targets fail fast without leaving half-formed
        // state. Symmetry is maintained at the end — same discipline as the
        // CLI path in cs nucleate --blocks.
        let blocks_ids = parse_mol_ids(params.blocks.as_deref().unwrap_or(&[]), "blocks")?;
        let blocked_by_ids =
            parse_mol_ids(params.blocked_by.as_deref().unwrap_or(&[]), "blocked_by")?;

        // State store is also cwd-scoped: the caller's project decides which
        // `.cosmon/state/` the new molecule is written into. Without this,
        // a long-lived MCP server would persist every molecule into its own
        // startup-time state dir regardless of which client called.
        let store = self.store_for(params.cwd.as_deref());
        validate_targets_exist_mcp(&store, &blocks_ids, "blocks")?;
        validate_targets_exist_mcp(&store, &blocked_by_ids, "blocked_by")?;

        let result = nucleate::nucleate(
            nucleate::NucleateRequest {
                formula: &formula,
                variables,
                assign,
            },
            &mut rand::thread_rng(),
        )
        .map_err(|e| McpError::internal_error(format!("nucleation failed: {e}"), None))?;

        let fleet_id = cosmon_core::id::FleetId::new(params.fleet.as_deref().unwrap_or("default"))
            .map_err(|e| McpError::invalid_params(format!("invalid fleet id: {e}"), None))?;

        // Build the new molecule's typed_links from the blocking params.
        let mut typed_links: Vec<MoleculeLink> =
            Vec::with_capacity(blocks_ids.len() + blocked_by_ids.len());
        for target in &blocks_ids {
            typed_links.push(MoleculeLink::Blocks {
                target: target.clone(),
            });
        }
        for source in &blocked_by_ids {
            typed_links.push(MoleculeLink::BlockedBy {
                source: source.clone(),
            });
        }

        // Resolve project_id from config — graceful fallback for legacy projects.
        let project_id = {
            let config_path = self.config_path_for(params.cwd.as_deref());
            cosmon_filestore::load_project_config(&config_path)
                .ok()
                .and_then(|c| c.project.project_id)
        };

        let assigned_role = params
            .role
            .as_deref()
            .map(str::parse::<cosmon_core::agent::AgentRole>)
            .transpose()
            .map_err(|e| McpError::invalid_params(format!("invalid role: {e}"), None))?;

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
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links,
            project_id,
            assigned_role,
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
            adapter: None,
        };

        store
            .save_molecule(&result.id, &mol_data)
            .map_err(|e| McpError::internal_error(format!("failed to persist: {e}"), None))?;

        // Symmetry maintenance: every target gains the reverse link.
        // Idempotent — skip if already present.
        for target in &blocks_ids {
            add_symmetric_link_mcp(
                &store,
                target,
                MoleculeLink::BlockedBy {
                    source: result.id.clone(),
                },
            )?;
        }
        for source in &blocked_by_ids {
            add_symmetric_link_mcp(
                &store,
                source,
                MoleculeLink::Blocks {
                    target: result.id.clone(),
                },
            )?;
        }

        // Build warnings for agent guidance. We deliberately do NOT expose a
        // `steps` or `next_steps` array here: the caller is the REQUESTER of
        // the work, not the executor. Surfacing formula step titles / bodies
        // has been observed to nudge the caller agent into acting out the
        // steps inline ("agent-attacks-molecule-itself" anti-pattern, see
        // delib-20260409-915a P0-2). The only legitimate next move for the
        // caller is `cs tackle <id>`, which spawns a worker to do the work.
        let mut warnings: Vec<String> = Vec::new();

        // Check attention budget. Reuse the cwd-scoped store so the
        // attention warning reflects the caller's project fleet, not the
        // server's process CWD.
        let fleet = store.load_fleet().unwrap_or_default();
        let molecules = store
            .list_molecules(&MoleculeFilter::default())
            .unwrap_or_default();
        let statuses: Vec<_> = molecules.iter().map(|m| m.status).collect();
        let attention =
            cosmon_core::attention::check_attention_budget(fleet.attention_budget, &statuses);
        if let Some(warning) = attention.warning() {
            warnings.push(warning);
        }

        // Soft-prevent: warn if fleet has no workers.
        if fleet.workers.is_empty() {
            warnings.push(
                "No workers exist in the fleet. This molecule will remain pending indefinitely. \
                 Deploy a fleet first (cs deploy) or spawn workers (cs spawn)."
                    .to_string(),
            );
        }

        if result.assigned_worker.is_none() {
            warnings.push(
                "No worker assigned — molecule is PENDING and inert. \
                 Assign a worker (or run `cs tackle <id>`) to make it actionable."
                    .to_string(),
            );
        }

        // Plan summary: one-sentence description of what the molecule will
        // do, taken from the formula's human-readable description so the
        // caller has enough context to decide, but nothing step-specific.
        let plan_summary = {
            let first_line = formula
                .description
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default()
                .trim();
            if first_line.is_empty() {
                format!(
                    "Execute formula '{}' ({} steps).",
                    result.formula_id.as_str(),
                    result.total_steps
                )
            } else {
                first_line.to_owned()
            }
        };

        let output = serde_json::json!({
            "id": result.id.as_str(),
            "formula": result.formula_id.as_str(),
            "status": result.status.to_string(),
            "total_steps": result.total_steps,
            "assigned_worker": result.assigned_worker.as_ref().map(WorkerId::as_str),
            "variables": result.variables,
            "created_at": result.created_at.to_rfc3339(),
            "caller_role": "You are the CALLER. You requested this work; you do not execute it.",
            "plan_summary": plan_summary,
            "next_action": {
                "command": format!("cs tackle {}", result.id.as_str()),
                "why": "cs tackle spawns a dedicated worker (worktree + tmux + fleet entry) that executes the formula; the caller must not run formula steps inline.",
                "do_not": [
                    "do not execute formula steps inline",
                    "do not call Agent() to act out personas",
                    "do not edit molecule files by hand",
                ],
            },
            "warnings": warnings,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Advance a molecule to its next step by providing evidence.
    #[tool(
        description = "Advance a molecule to its next step. PREREQUISITES: Molecule must be in 'running' or 'queued' state with an assigned worker. Call cosmon_observe first to see the current step and its exit criteria, then provide matching evidence. The first evolve promotes a 'queued' molecule to 'running'. When the last step is evolved, the molecule completes automatically. SURFACE SYNC: This mutates state — run `cs reconcile` after a batch of evolves to refresh STATUS.md / ISSUES.md / GitHub surfaces. DO NOT hand-edit those files; they are regenerated."
    )]
    fn cosmon_evolve(
        &self,
        Parameters(params): Parameters<EvolveParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = cosmon_core::id::MoleculeId::new(&params.molecule)
            .map_err(|e| McpError::invalid_params(format!("invalid molecule id: {e}"), None))?;

        let store = self.store_for(params.cwd.as_deref());
        let mol_data = store
            .load_molecule(&mol_id)
            .map_err(|e| McpError::invalid_params(format!("molecule not found: {e}"), None))?;

        let formula_text = std::fs::read_to_string(&params.formula_path)
            .map_err(|e| McpError::invalid_params(format!("formula file not found: {e}"), None))?;
        let formula = Formula::parse(&formula_text)
            .map_err(|e| McpError::invalid_params(format!("invalid formula: {e}"), None))?;

        let request = EvolveRequest {
            evidence: params.evidence,
            timestamp: Utc::now(),
        };

        let outcome = evolve::evolve(
            mol_data.status,
            mol_data.current_step,
            &mol_data.completed_steps,
            &formula,
            &request,
        )
        .map_err(|e| McpError::internal_error(format!("evolve failed: {e}"), None))?;

        let step_id = cosmon_core::id::StepId::new(&outcome.completed_step.id)
            .map_err(|e| McpError::internal_error(format!("invalid step id: {e}"), None))?;

        let mut updated = mol_data;
        updated.completed_steps.push(step_id);
        updated.updated_at = Utc::now();
        match &outcome.new_state {
            NewState::Active { current_step, .. } => {
                updated.current_step = *current_step;
            }
            NewState::Completed => {
                updated.status = MoleculeStatus::Completed;
            }
            _ => {}
        }

        store
            .save_molecule(&updated.id.clone(), &updated)
            .map_err(|e| McpError::internal_error(format!("failed to persist: {e}"), None))?;

        let output = serde_json::json!({
            "molecule": updated.id.as_str(),
            "completed_step": outcome.completed_step.id,
            "new_status": updated.status.to_string(),
            "new_step": match &outcome.new_state {
                NewState::Active { step_id, .. } => Some(step_id.clone()),
                _ => None,
            },
            "warnings": outcome.warnings,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Observe a molecule's current state, steps, and assignment.
    ///
    /// THESIS Part XVIII: also returns the shared coupling report
    /// (`poll_count`, `transitions`, `energy`, `entropy`, `temperature`)
    /// on the same wire format as `cosmon_wait`, so agents use one
    /// vocabulary across both read-only verbs. `poll_count` is hard-coded
    /// to 1 and `transitions` to 0 because a snapshot is a single read;
    /// `energy` aggregates `log/energy.jsonl` via
    /// [`coupling_report_snapshot`] and obeys omit-if-none.
    #[tool(
        description = "Observe a molecule's current state, steps, exit criteria, and worker assignment. Call this BEFORE cosmon_evolve to understand what evidence is needed for the current step. Also returns the shared coupling report (poll_count=1, transitions=0, plus optional energy {input_tokens, output_tokens, cost_usd} when log/energy.jsonl is available, entropy and temperature reserved for future probes) so you get the same metrics vocabulary as cosmon_wait for the same molecule (THESIS Part XVIII)."
    )]
    fn cosmon_observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = cosmon_core::id::MoleculeId::new(&params.molecule)
            .map_err(|e| McpError::invalid_params(format!("invalid molecule id: {e}"), None))?;

        let state_dir = self.state_dir_for(params.cwd.as_deref());
        let store = FileStore::new(&state_dir);
        let mol_data = store
            .load_molecule(&mol_id)
            .map_err(|e| McpError::invalid_params(format!("molecule not found: {e}"), None))?;

        // Build the shared coupling report from the same state_dir + log
        // that `cosmon_wait` reads. One kernel, one vocabulary.
        let metrics = coupling_report_snapshot(&state_dir, &mol_id);

        let mut output = serde_json::Map::new();
        output.insert(
            "id".to_owned(),
            serde_json::Value::String(mol_data.id.as_str().to_owned()),
        );
        output.insert(
            "formula".to_owned(),
            serde_json::Value::String(mol_data.formula_id.as_str().to_owned()),
        );
        output.insert(
            "status".to_owned(),
            serde_json::Value::String(mol_data.status.to_string()),
        );
        output.insert(
            "current_step".to_owned(),
            serde_json::json!(mol_data.current_step),
        );
        output.insert(
            "total_steps".to_owned(),
            serde_json::json!(mol_data.total_steps),
        );
        output.insert(
            "completed_steps".to_owned(),
            serde_json::json!(mol_data
                .completed_steps
                .iter()
                .map(cosmon_core::id::StepId::as_str)
                .collect::<Vec<_>>()),
        );
        output.insert(
            "assigned_worker".to_owned(),
            serde_json::json!(mol_data.assigned_worker.as_ref().map(WorkerId::as_str)),
        );
        output.insert(
            "variables".to_owned(),
            serde_json::json!(mol_data.variables),
        );
        output.insert(
            "collapse_reason".to_owned(),
            serde_json::json!(mol_data.collapse_reason),
        );
        output.insert(
            "collapsed_step".to_owned(),
            serde_json::json!(mol_data.collapsed_step),
        );
        output.insert("links".to_owned(), serde_json::json!(mol_data.links));
        output.insert(
            "created_at".to_owned(),
            serde_json::Value::String(mol_data.created_at.to_rfc3339()),
        );
        output.insert(
            "updated_at".to_owned(),
            serde_json::Value::String(mol_data.updated_at.to_rfc3339()),
        );
        output.insert(
            "poll_count".to_owned(),
            serde_json::json!(metrics.poll_count),
        );
        output.insert(
            "transitions".to_owned(),
            serde_json::json!(metrics.transitions),
        );
        if let Some(energy) = &metrics.energy {
            if let Ok(val) = serde_json::to_value(energy) {
                output.insert("energy".to_owned(), val);
            }
        }
        if let Some(entropy) = &metrics.entropy {
            if let Ok(val) = serde_json::to_value(entropy) {
                output.insert("entropy".to_owned(), val);
            }
        }
        if let Some(temperature) = metrics.temperature {
            output.insert("temperature".to_owned(), serde_json::json!(temperature));
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&serde_json::Value::Object(output)).unwrap_or_default(),
        )]))
    }

    /// Show ensemble (fleet) status: workers and molecule summary counts.
    #[tool(
        description = "Show fleet status: all workers and molecule counts by status (pending/queued/running/frozen/completed/collapsed). This is typically the FIRST tool to call — check what workers exist and what molecules are in flight before creating new ones."
    )]
    fn cosmon_ensemble(
        &self,
        Parameters(params): Parameters<EnsembleParams>,
    ) -> Result<CallToolResult, McpError> {
        let store = self.store_for(params.cwd.as_deref());

        let fleet = store
            .load_fleet()
            .map_err(|e| McpError::internal_error(format!("failed to load fleet: {e}"), None))?;

        // Resolve project_id for scoping — graceful fallback to no filter.
        let project_id = {
            let config_path = self.config_path_for(params.cwd.as_deref());
            cosmon_filestore::load_project_config(&config_path)
                .ok()
                .and_then(|c| c.project.project_id)
        };

        let filter = MoleculeFilter {
            status: params.status.as_deref().and_then(|s| s.parse().ok()),
            project: project_id,
            ..MoleculeFilter::default()
        };

        let molecules = store.list_molecules(&filter).map_err(|e| {
            McpError::internal_error(format!("failed to list molecules: {e}"), None)
        })?;

        let workers: Vec<serde_json::Value> = fleet
            .workers
            .values()
            .map(|w| {
                serde_json::json!({
                    "id": w.id.as_str(),
                    "agent": w.agent_id.as_str(),
                    "role": w.role.to_string(),
                    "status": w.status.to_string(),
                    "repo": w.repo,
                    "clearance": w.clearance.to_string(),
                    "molecule": w.current_molecule.as_ref().map(|m| m.as_str().to_owned()),
                })
            })
            .collect();

        let output = serde_json::json!({
            "workers": workers,
            "molecules": {
                "pending": molecules.iter().filter(|m| m.status == MoleculeStatus::Pending).count(),
                "queued": molecules.iter().filter(|m| m.status == MoleculeStatus::Queued).count(),
                "running": molecules.iter().filter(|m| m.status == MoleculeStatus::Running).count(),
                "frozen": molecules.iter().filter(|m| m.status == MoleculeStatus::Frozen).count(),
                "completed": molecules.iter().filter(|m| m.status == MoleculeStatus::Completed).count(),
                "collapsed": molecules.iter().filter(|m| m.status == MoleculeStatus::Collapsed).count(),
                "total": molecules.len(),
            },
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Send a nudge message to a worker's tmux session.
    ///
    /// Used by system agents (e.g. mail-courier) to notify workers of
    /// new messages or events. The message is sent via the transport
    /// layer (tmux send-keys), not via mailboxes.
    #[tool(
        description = "Send a nudge message to a worker's tmux session. Used by system agents to notify workers of events."
    )]
    fn cosmon_nudge(
        &self,
        Parameters(params): Parameters<NudgeParams>,
    ) -> Result<CallToolResult, McpError> {
        let worker_id = WorkerId::new(&params.worker)
            .map_err(|e| McpError::invalid_params(format!("invalid worker id: {e}"), None))?;

        let _store = self.store(); // satisfy &self usage for tool macro
                                   // Fleet-scoped tmux socket. Honor COSMON_TMUX_SOCKET when set, else
                                   // derive from the server's project config (sibling-isolation invariant,
                                   // delib-20260414-6d73).
        let socket = std::env::var("COSMON_TMUX_SOCKET").unwrap_or_else(|_| {
            let config_path = self.store_dir.as_path().parent().map_or_else(
                || self.store_dir.join("config.toml"),
                |p| p.join("config.toml"),
            );
            cosmon_filestore::resolve_tmux_socket_name(&config_path)
        });
        let backend = cosmon_transport::TmuxBackend::new(&socket);

        backend
            .send_input(&worker_id, &params.message)
            .map_err(|e| McpError::internal_error(format!("nudge failed: {e}"), None))?;

        let output = serde_json::json!({
            "worker": params.worker,
            "nudged": true,
            "message_length": params.message.len(),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Declare the calling agent's cognitive state.
    ///
    /// Agents call this to signal what they are doing: working on a task,
    /// waiting for another agent's response, done with their mission, or idle.
    /// This self-reported status is displayed in `cs ensemble` LIVE column.
    #[tool(
        description = "Declare your cognitive state: working, waiting, done, idle. Shown in cs ensemble. Call this when your state changes."
    )]
    fn cosmon_declare(
        &self,
        Parameters(params): Parameters<DeclareParams>,
    ) -> Result<CallToolResult, McpError> {
        // Validate status.
        let valid = ["working", "waiting", "done", "idle", "error"];
        if !valid.contains(&params.status.as_str()) {
            return Err(McpError::invalid_params(
                format!(
                    "invalid status '{}', must be one of: {}",
                    params.status,
                    valid.join(", ")
                ),
                None,
            ));
        }

        // Write cognitive status to a file.
        let state_dir = self.state_dir_for(params.cwd.as_deref());
        let cognitive_dir = state_dir.join("cognitive");
        std::fs::create_dir_all(&cognitive_dir)
            .map_err(|e| McpError::internal_error(format!("failed to create dir: {e}"), None))?;

        // Derive worker ID from the session — use COSMON_WORKER_ID env var
        // or fall back to a generic name.
        let worker_id = std::env::var("COSMON_WORKER_ID").unwrap_or_else(|_| "unknown".to_owned());

        let status_json = serde_json::json!({
            "status": params.status,
            "detail": params.detail,
            "updated_at": Utc::now().to_rfc3339(),
        });

        let path = cognitive_dir.join(format!("{worker_id}.json"));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&status_json).unwrap_or_default(),
        )
        .map_err(|e| McpError::internal_error(format!("failed to write: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&status_json).unwrap_or_default(),
        )]))
    }

    /// Log energy (token) consumption for tracking and budgeting.
    ///
    /// Call this after completing a significant action to track token usage.
    /// Data feeds into `cosmon_energy` reports and fleet efficiency metrics.
    #[tool(
        description = "Log token consumption. Call after significant actions to track energy usage and fleet efficiency."
    )]
    fn cosmon_energy_log(
        &self,
        Parameters(params): Parameters<EnergyLogParams>,
    ) -> Result<CallToolResult, McpError> {
        use cosmon_core::energy::EnergyRecord;
        use cosmon_state::file_energy_tracker::FileEnergyTracker;
        use cosmon_state::EnergyTracker;

        let worker_id = std::env::var("COSMON_WORKER_ID").unwrap_or_else(|_| "unknown".to_owned());
        let worker = cosmon_core::id::WorkerId::new(&worker_id)
            .unwrap_or_else(|_| cosmon_core::id::WorkerId::new("unknown").unwrap());
        let molecule = cosmon_core::id::MoleculeId::new("cs-00000000-none")
            .unwrap_or_else(|_| cosmon_core::id::MoleculeId::new("cs-00000000-none").unwrap());
        let step = cosmon_core::id::StepId::new("energy-log").unwrap();

        let record = EnergyRecord {
            timestamp: Utc::now(),
            worker,
            molecule,
            step,
            model: "claude".to_owned(),
            input_tokens: cosmon_core::energy::TokenCount::new(params.input_tokens),
            output_tokens: cosmon_core::energy::TokenCount::new(params.output_tokens),
            cost: cosmon_core::energy::TokenCost::new(0.0),
        };

        let state_dir = self.state_dir_for(params.cwd.as_deref());
        let tracker = FileEnergyTracker::new(&state_dir);
        tracker
            .record(&record)
            .map_err(|e| McpError::internal_error(format!("failed to log energy: {e}"), None))?;

        let total = params.input_tokens + params.output_tokens;
        let output = serde_json::json!({
            "logged": true,
            "total_tokens": total,
            "worker": worker_id,
            "description": params.description,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Freeze a molecule — pause execution. Can be resumed later with thaw.
    #[tool(
        description = "Freeze a molecule — pause execution. The molecule must be in 'running' or 'queued' state. Resume later with cosmon_thaw. Use when a molecule needs to yield resources or wait for external input."
    )]
    fn cosmon_freeze(
        &self,
        Parameters(params): Parameters<FreezeParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = cosmon_core::id::MoleculeId::new(&params.molecule)
            .map_err(|e| McpError::invalid_params(format!("invalid molecule id: {e}"), None))?;

        let store = self.store_for(params.cwd.as_deref());
        let mut mol_data = store
            .load_molecule(&mol_id)
            .map_err(|e| McpError::invalid_params(format!("molecule not found: {e}"), None))?;

        if !matches!(
            mol_data.status,
            MoleculeStatus::Running | MoleculeStatus::Queued
        ) {
            return Err(McpError::invalid_params(
                format!(
                    "cannot freeze molecule in {} state (must be running or queued)",
                    mol_data.status
                ),
                None,
            ));
        }

        mol_data.status = MoleculeStatus::Frozen;
        mol_data.updated_at = Utc::now();

        store
            .save_molecule(&mol_id, &mol_data)
            .map_err(|e| McpError::internal_error(format!("failed to persist: {e}"), None))?;

        let output = serde_json::json!({
            "molecule": mol_id.as_str(),
            "status": "frozen",
            "frozen_at_step": mol_data.current_step,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Thaw a frozen molecule — resume execution from where it was paused.
    #[tool(
        description = "Thaw a frozen molecule — resume from where it was paused. The molecule returns to 'queued' (if assigned) or 'pending' (if unassigned). A worker must then pick it up and call cosmon_evolve to continue."
    )]
    fn cosmon_thaw(
        &self,
        Parameters(params): Parameters<ThawParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = cosmon_core::id::MoleculeId::new(&params.molecule)
            .map_err(|e| McpError::invalid_params(format!("invalid molecule id: {e}"), None))?;

        let store = self.store_for(params.cwd.as_deref());
        let mut mol_data = store
            .load_molecule(&mol_id)
            .map_err(|e| McpError::invalid_params(format!("molecule not found: {e}"), None))?;

        if mol_data.status != MoleculeStatus::Frozen {
            return Err(McpError::invalid_params(
                format!(
                    "cannot thaw molecule in {} state (must be frozen)",
                    mol_data.status
                ),
                None,
            ));
        }

        // Thaw to Queued (worker picks it up and promotes to Running).
        mol_data.status = if mol_data.assigned_worker.is_some() {
            MoleculeStatus::Queued
        } else {
            MoleculeStatus::Pending
        };
        mol_data.updated_at = Utc::now();

        store
            .save_molecule(&mol_id, &mol_data)
            .map_err(|e| McpError::internal_error(format!("failed to persist: {e}"), None))?;

        let output = serde_json::json!({
            "molecule": mol_id.as_str(),
            "status": "active",
            "resumed_at_step": mol_data.current_step,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Collapse a molecule — mark it as failed with a reason. This is a terminal state.
    #[tool(
        description = "Collapse a molecule — mark it as failed with a reason. This is a terminal state. SURFACE SYNC: Mutates state — run `cs reconcile` after to refresh STATUS.md / ISSUES.md / GitHub. DO NOT hand-edit those files."
    )]
    fn cosmon_collapse(
        &self,
        Parameters(params): Parameters<CollapseParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = cosmon_core::id::MoleculeId::new(&params.molecule)
            .map_err(|e| McpError::invalid_params(format!("invalid molecule id: {e}"), None))?;

        let store = self.store_for(params.cwd.as_deref());
        let mut mol_data = store
            .load_molecule(&mol_id)
            .map_err(|e| McpError::invalid_params(format!("molecule not found: {e}"), None))?;

        if mol_data.status == MoleculeStatus::Completed
            || mol_data.status == MoleculeStatus::Collapsed
        {
            return Err(McpError::invalid_params(
                format!(
                    "cannot collapse molecule in {} state (already terminal)",
                    mol_data.status
                ),
                None,
            ));
        }

        mol_data.collapse_reason = Some(params.reason.clone());
        mol_data.collapsed_step = Some(mol_data.current_step);
        mol_data.status = MoleculeStatus::Collapsed;
        mol_data.updated_at = Utc::now();

        store
            .save_molecule(&mol_id, &mol_data)
            .map_err(|e| McpError::internal_error(format!("failed to persist: {e}"), None))?;

        let output = serde_json::json!({
            "molecule": mol_id.as_str(),
            "status": "collapsed",
            "reason": params.reason,
            "collapsed_at_step": mol_data.collapsed_step,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Complete — shortcut to mark molecules as done
    // -----------------------------------------------------------------------

    /// ✅ Complete a molecule — shortcut to mark as done without full evolve ceremony.
    #[allow(clippy::unnecessary_wraps)]
    #[tool(
        description = "Complete a molecule — mark it as done without the full evolve ceremony. \
        No formula or step validation needed. Supports batch: pass comma-separated IDs. \
        The molecule must not already be in a terminal state (completed/collapsed). \
        SURFACE SYNC: After a batch of completes, run `cs reconcile` to refresh \
        STATUS.md / ISSUES.md / GitHub surfaces. DO NOT hand-edit those files."
    )]
    fn cosmon_complete(
        &self,
        Parameters(params): Parameters<CompleteParams>,
    ) -> Result<CallToolResult, McpError> {
        let reason = params
            .reason
            .unwrap_or_else(|| "completed via MCP".to_owned());
        let ids: Vec<&str> = params.molecule.split(',').map(str::trim).collect();
        let store = self.store_for(params.cwd.as_deref());
        let mut results: Vec<serde_json::Value> = Vec::new();

        for raw_id in &ids {
            let mol_id = match MoleculeId::new(*raw_id) {
                Ok(id) => id,
                Err(e) => {
                    results.push(serde_json::json!({
                        "molecule": raw_id,
                        "error": format!("invalid molecule id: {e}"),
                    }));
                    continue;
                }
            };

            let mut mol_data = match store.load_molecule(&mol_id) {
                Ok(d) => d,
                Err(e) => {
                    results.push(serde_json::json!({
                        "molecule": raw_id,
                        "error": format!("molecule not found: {e}"),
                    }));
                    continue;
                }
            };

            if mol_data.status.is_terminal() {
                results.push(serde_json::json!({
                    "molecule": raw_id,
                    "error": format!("already {} (terminal)", mol_data.status),
                }));
                continue;
            }

            let prev_status = mol_data.status;
            mol_data.status = MoleculeStatus::Completed;
            mol_data.updated_at = Utc::now();

            if let Err(e) = store.save_molecule(&mol_id, &mol_data) {
                results.push(serde_json::json!({
                    "molecule": raw_id,
                    "error": format!("failed to persist: {e}"),
                }));
                continue;
            }

            results.push(serde_json::json!({
                "molecule": mol_id.as_str(),
                "previous_status": prev_status.to_string(),
                "status": "completed",
                "reason": reason,
            }));
        }

        let output = if results.len() == 1 {
            serde_json::to_string_pretty(&results[0]).unwrap_or_default()
        } else {
            serde_json::to_string_pretty(&results).unwrap_or_default()
        };

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    // -----------------------------------------------------------------------
    // Wait — bounded poll until a molecule reaches a target status
    // -----------------------------------------------------------------------

    /// ⏳ Wait for a molecule to reach a terminal (or requested) status.
    ///
    /// Closes the `cs tackle` / `cosmon_wait` / `cs done` canonical trinity.
    /// Stateless, idempotent, and bounded: exits as soon as the condition
    /// is met or the timeout expires. See `cosmon-state::wait` for the
    /// shared kernel.
    #[tool(
        description = "USE THIS to wait for tackle workers to finish. Blocks until a \
        molecule reaches a terminal (or requested) status, then returns the full molecule \
        state so the caller has everything it needs without a follow-up observe. \
        Canonical workflow: cosmon_nucleate → (human: cs tackle) → cosmon_wait → cs done. \
        Parameters: `molecule` (required), `for` (default: [\"completed\",\"collapsed\"]), \
        `timeout_seconds` (default 600), `poll_interval_seconds` (default 5). \
        Stateless and read-only — this is kubectl wait, not kubectl watch. Polling on an \
        already-terminal molecule returns immediately with zero wasted polls. \
        Returns {molecule, status, reached, elapsed_seconds, current_step, total_steps, \
        poll_count, transitions} plus optional metrics (omit-if-none): `energy` \
        {input_tokens, output_tokens, cost_usd} when the energy log is available, \
        `entropy` {input_bits, output_bits} and `temperature` reserved for future probes. \
        Inspect these metrics to build intuition about what your requests cost and how \
        healthy the molecule actually was (transitions flapping between states is a \
        useful unhealth signal). Errors with `timeout` if the deadline passes before \
        the condition is met."
    )]
    fn cosmon_wait(
        &self,
        Parameters(params): Parameters<WaitParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = MoleculeId::new(&params.molecule)
            .map_err(|e| McpError::invalid_params(format!("invalid molecule id: {e}"), None))?;

        // Default target set = terminal statuses, same as the CLI. Empty
        // input is rejected because an empty `for` would wait forever
        // (every status is outside the set), which is never intended.
        let raw = params
            .r#for
            .unwrap_or_else(|| vec!["completed".to_owned(), "collapsed".to_owned()]);
        let mut targets: Vec<MoleculeStatus> = Vec::with_capacity(raw.len());
        for s in &raw {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: MoleculeStatus = trimmed.parse().map_err(|e| {
                McpError::invalid_params(format!("invalid status `{trimmed}`: {e}"), None)
            })?;
            if !targets.contains(&parsed) {
                targets.push(parsed);
            }
        }
        if targets.is_empty() {
            return Err(McpError::invalid_params(
                "`for` must list at least one status",
                None,
            ));
        }

        let timeout = std::time::Duration::from_secs(params.timeout_seconds.unwrap_or(600));
        let poll_interval =
            std::time::Duration::from_secs(params.poll_interval_seconds.unwrap_or(5).max(1));

        let state_dir = self.state_dir_for(params.cwd.as_deref());
        let store = FileStore::new(&state_dir);
        match wait_for_status_with_metrics(
            &store,
            &state_dir,
            &mol_id,
            &targets,
            timeout,
            poll_interval,
        ) {
            Ok(outcome) => {
                let output = render_wait_outcome_json(&outcome);
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&output).unwrap_or_default(),
                )]))
            }
            Err(WaitError::MoleculeNotFound(id)) => Err(McpError::invalid_params(
                format!("molecule not found: {id}"),
                None,
            )),
            Err(WaitError::Store(msg)) => Err(McpError::internal_error(
                format!("state store error: {msg}"),
                None,
            )),
            Err(WaitError::Timeout {
                elapsed,
                last_status,
            }) => Err(McpError::internal_error(
                format!(
                    "cosmon_wait timed out after {:.1}s — {} is still `{}` (targets: {:?})",
                    elapsed.as_secs_f64(),
                    mol_id,
                    last_status,
                    targets.iter().map(ToString::to_string).collect::<Vec<_>>(),
                ),
                None,
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Interaction tools: decay, merge, transform
    // -----------------------------------------------------------------------

    /// 💫 Decay a molecule into child molecules (1 → N, HOMOGENEOUS).
    #[tool(
        description = "HOMOGENEOUS decay only — split one molecule into N children that all share the parent's variables (same topic/scope verbatim). The source must be an 'idea' or 'issue' kind and completes; products are nucleated with the given formula and each gets a typed DecayedFrom link. Use for genuinely uniform splits (e.g. 3 identical review slots). DO NOT use for heterogeneous decomposition — if the children have distinct topics, scopes, or dependencies (e.g. a deliberation synthesis enumerating N different follow-up tasks), call cosmon_nucleate per child instead and wire dependencies manually via blocks/blocked-by. SURFACE SYNC: Mutates state — run `cs reconcile` after to refresh STATUS.md / ISSUES.md / IDEAS.md / GitHub. DO NOT hand-edit those files."
    )]
    #[allow(clippy::too_many_lines)] // ADR-062 added the `collapse_cause: None` field which pushed this past the 100-line limit; refactoring is out of scope for the K3 minimum hook.
    fn cosmon_decay(
        &self,
        Parameters(params): Parameters<DecayParams>,
    ) -> Result<CallToolResult, McpError> {
        let source_id = MoleculeId::new(&params.source)
            .map_err(|e| McpError::invalid_params(format!("invalid source id: {e}"), None))?;

        let store = self.store_for(params.cwd.as_deref());
        let mut source = store
            .load_molecule(&source_id)
            .map_err(|e| McpError::invalid_params(format!("source not found: {e}"), None))?;

        let source_kind = source.kind.unwrap_or(cosmon_core::kind::MoleculeKind::Task);
        if !source_kind.can_decay() {
            return Err(McpError::invalid_params(
                format!("kind '{source_kind}' cannot decay (only idea and issue can)"),
                None,
            ));
        }

        let count = params.count.unwrap_or(1);
        let product_kind: cosmon_core::kind::MoleculeKind = params
            .product_kind
            .as_deref()
            .unwrap_or("task")
            .parse()
            .map_err(|e| McpError::invalid_params(format!("invalid product kind: {e}"), None))?;

        let formulas_dir = self.formulas_dir_for(params.cwd.as_deref());
        let formula_path = formulas_dir.join(format!("{}.formula.toml", params.formula));
        let toml_text = std::fs::read_to_string(&formula_path)
            .map_err(|e| McpError::invalid_params(format!("formula not found: {e}"), None))?;
        let formula = Formula::parse(&toml_text)
            .map_err(|e| McpError::invalid_params(format!("invalid formula: {e}"), None))?;

        let mut product_ids = Vec::new();
        let mut rng = rand::thread_rng();
        for _ in 0..count {
            let nuc = nucleate::nucleate(
                nucleate::NucleateRequest {
                    formula: &formula,
                    variables: source.variables.clone(),
                    assign: source.assigned_worker.clone(),
                },
                &mut rng,
            )
            .map_err(|e| McpError::internal_error(format!("nucleation failed: {e}"), None))?;

            let product = MoleculeData {
                id: nuc.id.clone(),
                fleet_id: source.fleet_id.clone(),
                formula_id: nuc.formula_id.clone(),
                status: nuc.status,
                variables: nuc.variables.clone(),
                assigned_worker: nuc.assigned_worker.clone(),
                created_at: nuc.created_at,
                updated_at: nuc.created_at,
                total_steps: nuc.total_steps,
                current_step: 0,
                completed_steps: Vec::new(),
                collapse_reason: None,
                collapse_cause: None,
                collapse_reason_kind: None,
                collapsed_step: None,
                links: Vec::new(),
                kind: Some(product_kind),
                class: cosmon_core::molecule_class::MoleculeClass::default(),
                typed_links: vec![cosmon_core::interaction::MoleculeLink::DecayedFrom {
                    id: source_id.clone(),
                }],
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
                adapter: None,
            };
            store
                .save_molecule(&nuc.id, &product)
                .map_err(|e| McpError::internal_error(format!("failed to save: {e}"), None))?;
            product_ids.push(nuc.id);
        }

        source.status = MoleculeStatus::Completed;
        source.updated_at = Utc::now();
        for pid in &product_ids {
            source
                .typed_links
                .push(cosmon_core::interaction::MoleculeLink::DecayProduct { id: pid.clone() });
        }
        store
            .save_molecule(&source_id, &source)
            .map_err(|e| McpError::internal_error(format!("failed to save: {e}"), None))?;

        let output = serde_json::json!({
            "interaction": "decay",
            "source": source_id.as_str(),
            "products": product_ids.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
            "product_kind": product_kind.to_string(),
            "reason": params.reason,
            "next_steps": [
                "Use cosmon_observe on each product to see its first step.",
                "Use cosmon_evolve to advance each product."
            ],
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// 🔀 Merge molecules into a synthesis (N → 1).
    #[tool(
        description = "Merge multiple molecules into one synthesis (N → 1). Sources must be 'task', 'idea', or 'issue' kind. All sources complete; a new product molecule is nucleated with the given formula. Use this when research converges into a decision or tasks consolidate. SURFACE SYNC: Mutates state — run `cs reconcile` after to refresh STATUS.md / ISSUES.md / GitHub. DO NOT hand-edit those files."
    )]
    #[allow(clippy::too_many_lines)]
    fn cosmon_merge(
        &self,
        Parameters(params): Parameters<MergeParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.sources.len() < 2 {
            return Err(McpError::invalid_params(
                "merge requires at least 2 source molecules",
                None,
            ));
        }

        let source_ids: Vec<MoleculeId> = params
            .sources
            .iter()
            .map(|s| {
                MoleculeId::new(s)
                    .map_err(|e| McpError::invalid_params(format!("invalid id: {e}"), None))
            })
            .collect::<Result<_, _>>()?;

        let store = self.store_for(params.cwd.as_deref());
        let mut sources: Vec<MoleculeData> = Vec::new();
        for sid in &source_ids {
            let mol = store
                .load_molecule(sid)
                .map_err(|e| McpError::invalid_params(format!("not found: {e}"), None))?;
            let kind = mol.kind.unwrap_or(cosmon_core::kind::MoleculeKind::Task);
            if !kind.can_merge() {
                return Err(McpError::invalid_params(
                    format!("{sid} (kind: {kind}) cannot merge"),
                    None,
                ));
            }
            sources.push(mol);
        }

        let product_kind: cosmon_core::kind::MoleculeKind = params
            .product_kind
            .as_deref()
            .unwrap_or("decision")
            .parse()
            .map_err(|e| McpError::invalid_params(format!("invalid kind: {e}"), None))?;

        let mut merged_vars = HashMap::new();
        for src in &sources {
            merged_vars.extend(src.variables.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        let formulas_dir = self.formulas_dir_for(params.cwd.as_deref());
        let formula_path = formulas_dir.join(format!("{}.formula.toml", params.formula));
        let toml_text = std::fs::read_to_string(&formula_path)
            .map_err(|e| McpError::invalid_params(format!("formula not found: {e}"), None))?;
        let formula = Formula::parse(&toml_text)
            .map_err(|e| McpError::invalid_params(format!("invalid formula: {e}"), None))?;

        let nuc = nucleate::nucleate(
            nucleate::NucleateRequest {
                formula: &formula,
                variables: merged_vars,
                assign: sources[0].assigned_worker.clone(),
            },
            &mut rand::thread_rng(),
        )
        .map_err(|e| McpError::internal_error(format!("nucleation failed: {e}"), None))?;

        let product = MoleculeData {
            id: nuc.id.clone(),
            fleet_id: sources[0].fleet_id.clone(),
            formula_id: nuc.formula_id.clone(),
            status: nuc.status,
            variables: nuc.variables.clone(),
            assigned_worker: nuc.assigned_worker.clone(),
            created_at: nuc.created_at,
            updated_at: nuc.created_at,
            total_steps: nuc.total_steps,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: Some(product_kind),
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: vec![cosmon_core::interaction::MoleculeLink::MergedFrom {
                ids: source_ids.clone(),
            }],
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
            adapter: None,
        };
        store
            .save_molecule(&nuc.id, &product)
            .map_err(|e| McpError::internal_error(format!("failed to save: {e}"), None))?;

        for (sid, mut src) in source_ids.iter().zip(sources) {
            src.status = MoleculeStatus::Completed;
            src.updated_at = Utc::now();
            src.typed_links
                .push(cosmon_core::interaction::MoleculeLink::MergedInto { id: nuc.id.clone() });
            store
                .save_molecule(sid, &src)
                .map_err(|e| McpError::internal_error(format!("failed to save: {e}"), None))?;
        }

        let output = serde_json::json!({
            "interaction": "merge",
            "sources": source_ids.iter().map(MoleculeId::as_str).collect::<Vec<_>>(),
            "product": nuc.id.as_str(),
            "product_kind": product_kind.to_string(),
            "reason": params.reason,
            "next_steps": [
                format!("Use cosmon_observe on '{}' to see the product's first step.", nuc.id),
            ],
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// 🔄 Transform a molecule's kind.
    #[tool(
        description = "Transform a molecule's cognitive kind (e.g., idea → task, issue → task). The molecule keeps its identity and state but changes its nature. Valid transforms: idea → task/decision/issue, issue → task, task → issue. Use this when an idea becomes actionable or an issue needs reclassification. SURFACE SYNC: Mutates state — run `cs reconcile` after to refresh STATUS.md / ISSUES.md / IDEAS.md / GitHub. DO NOT hand-edit those files."
    )]
    fn cosmon_transform(
        &self,
        Parameters(params): Parameters<TransformParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = MoleculeId::new(&params.molecule)
            .map_err(|e| McpError::invalid_params(format!("invalid id: {e}"), None))?;

        let store = self.store_for(params.cwd.as_deref());
        let mut mol = store
            .load_molecule(&mol_id)
            .map_err(|e| McpError::invalid_params(format!("not found: {e}"), None))?;

        let from_kind = mol.kind.unwrap_or(cosmon_core::kind::MoleculeKind::Task);
        let to_kind: cosmon_core::kind::MoleculeKind = params
            .to
            .parse()
            .map_err(|e| McpError::invalid_params(format!("invalid kind: {e}"), None))?;

        if !from_kind.can_transform_to(to_kind) {
            return Err(McpError::invalid_params(
                format!(
                    "cannot transform {from_kind} → {to_kind} (valid: {:?})",
                    from_kind
                        .valid_transforms()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                ),
                None,
            ));
        }

        mol.kind = Some(to_kind);
        mol.updated_at = Utc::now();
        mol.typed_links
            .push(cosmon_core::interaction::MoleculeLink::TransformedFrom { kind: from_kind });
        store
            .save_molecule(&mol_id, &mol)
            .map_err(|e| McpError::internal_error(format!("failed to save: {e}"), None))?;

        let output = serde_json::json!({
            "interaction": "transform",
            "molecule": mol_id.as_str(),
            "from": from_kind.to_string(),
            "to": to_kind.to_string(),
            "reason": params.reason,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    // -----------------------------------------------------------------------
    // Query tools (archive-service pattern: search, get, list, count, export,
    // stats, aggregate, energy)
    // -----------------------------------------------------------------------

    /// Search molecules by text across IDs, formula names, variables, and worker assignments.
    #[tool(
        description = "Search molecules by text across IDs, formula names, variables, and worker assignments. Returns matching molecules ranked by relevance."
    )]
    fn cosmon_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(50);
        let query = params.query.to_lowercase();

        let all = self
            .store_for(params.cwd.as_deref())
            .list_molecules(&MoleculeFilter::default())
            .map_err(|e| {
                McpError::internal_error(format!("failed to list molecules: {e}"), None)
            })?;

        let mut scored: Vec<(usize, &MoleculeData)> = all
            .iter()
            .filter_map(|m| {
                let mut score = 0usize;
                if m.id.as_str().to_lowercase().contains(&query) {
                    score += 10;
                }
                if m.formula_id.as_str().to_lowercase().contains(&query) {
                    score += 5;
                }
                if let Some(ref w) = m.assigned_worker {
                    if w.as_str().to_lowercase().contains(&query) {
                        score += 5;
                    }
                }
                for v in m.variables.values() {
                    if v.to_lowercase().contains(&query) {
                        score += 2;
                    }
                }
                if m.collapse_reason
                    .as_deref()
                    .is_some_and(|r| r.to_lowercase().contains(&query))
                {
                    score += 3;
                }
                if score > 0 {
                    Some((score, m))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by_key(|e| std::cmp::Reverse(e.0));

        let results: Vec<serde_json::Value> = scored
            .into_iter()
            .take(limit)
            .map(|(score, m)| {
                serde_json::json!({
                    "id": m.id.as_str(),
                    "formula": m.formula_id.as_str(),
                    "status": m.status.to_string(),
                    "assigned_worker": m.assigned_worker.as_ref().map(WorkerId::as_str),
                    "current_step": m.current_step,
                    "total_steps": m.total_steps,
                    "relevance": score,
                    "updated_at": m.updated_at.to_rfc3339(),
                })
            })
            .collect();

        let output = serde_json::json!({
            "query": params.query,
            "total_matches": results.len(),
            "results": results,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Get a single molecule by ID with full detail.
    #[tool(
        description = "Get a single molecule by ID with full detail including steps, variables, links, and timestamps."
    )]
    fn cosmon_get(
        &self,
        Parameters(params): Parameters<GetParams>,
    ) -> Result<CallToolResult, McpError> {
        let mol_id = cosmon_core::id::MoleculeId::new(&params.id)
            .map_err(|e| McpError::invalid_params(format!("invalid molecule id: {e}"), None))?;

        let mol_data = self
            .store_for(params.cwd.as_deref())
            .load_molecule(&mol_id)
            .map_err(|e| McpError::invalid_params(format!("molecule not found: {e}"), None))?;

        let output = molecule_to_json(&mol_data);

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// List molecules with filtering, sorting, and pagination.
    #[tool(
        description = "List molecules with filtering by status/worker/formula, sorting, and pagination. Returns a page of molecules with total count."
    )]
    fn cosmon_list(
        &self,
        Parameters(params): Parameters<ListParams>,
    ) -> Result<CallToolResult, McpError> {
        let filter = build_filter(
            params.status.as_deref(),
            params.worker.as_deref(),
            params.formula.as_deref(),
        )?;

        let mut molecules = self
            .store_for(params.cwd.as_deref())
            .list_molecules(&filter)
            .map_err(|e| {
                McpError::internal_error(format!("failed to list molecules: {e}"), None)
            })?;

        let total = molecules.len();
        let sort_by = params.sort_by.as_deref().unwrap_or("updated_at");
        let descending = params.order.as_deref().unwrap_or("desc") == "desc";

        match sort_by {
            "created_at" => molecules.sort_by_key(|a| a.created_at),
            "status" => molecules.sort_by_key(|a| a.status.to_string()),
            _ => molecules.sort_by_key(|a| a.updated_at),
        }

        if descending {
            molecules.reverse();
        }

        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(50);
        let page: Vec<serde_json::Value> = molecules
            .iter()
            .skip(offset)
            .take(limit)
            .map(molecule_to_json)
            .collect();

        let output = serde_json::json!({
            "total": total,
            "offset": offset,
            "limit": limit,
            "molecules": page,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Count molecules matching filter criteria.
    #[tool(
        description = "Count molecules matching filter criteria. Fast alternative to list when you only need the count."
    )]
    fn cosmon_count(
        &self,
        Parameters(params): Parameters<CountParams>,
    ) -> Result<CallToolResult, McpError> {
        let filter = build_filter(
            params.status.as_deref(),
            params.worker.as_deref(),
            params.formula.as_deref(),
        )?;

        let molecules = self
            .store_for(params.cwd.as_deref())
            .list_molecules(&filter)
            .map_err(|e| {
                McpError::internal_error(format!("failed to list molecules: {e}"), None)
            })?;

        let output = serde_json::json!({
            "count": molecules.len(),
            "filter": {
                "status": params.status,
                "worker": params.worker,
                "formula": params.formula,
            },
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Export molecules in JSON array, NDJSON, or CSV format.
    #[tool(
        description = "Export molecules in json (array), ndjson (newline-delimited JSON), or csv format. Supports the same filters as list."
    )]
    fn cosmon_export(
        &self,
        Parameters(params): Parameters<ExportParams>,
    ) -> Result<CallToolResult, McpError> {
        let filter = build_filter(
            params.status.as_deref(),
            params.worker.as_deref(),
            params.formula.as_deref(),
        )?;

        let molecules = self
            .store_for(params.cwd.as_deref())
            .list_molecules(&filter)
            .map_err(|e| {
                McpError::internal_error(format!("failed to list molecules: {e}"), None)
            })?;

        let output = match params.format.as_str() {
            "json" => {
                let items: Vec<serde_json::Value> =
                    molecules.iter().map(molecule_to_json).collect();
                serde_json::to_string_pretty(&items).unwrap_or_default()
            }
            "ndjson" => molecules
                .iter()
                .map(molecule_to_json)
                .map(|v| serde_json::to_string(&v).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
            "csv" => {
                let mut lines = vec![
                    "id,formula,status,worker,current_step,total_steps,created_at,updated_at"
                        .to_string(),
                ];
                for m in &molecules {
                    lines.push(format!(
                        "{},{},{},{},{},{},{},{}",
                        m.id.as_str(),
                        m.formula_id.as_str(),
                        m.status,
                        m.assigned_worker.as_ref().map_or("", WorkerId::as_str),
                        m.current_step,
                        m.total_steps,
                        m.created_at.to_rfc3339(),
                        m.updated_at.to_rfc3339(),
                    ));
                }
                lines.join("\n")
            }
            other => {
                return Err(McpError::invalid_params(
                    format!("unsupported format: {other} (use json, ndjson, or csv)"),
                    None,
                ));
            }
        };

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// System statistics: molecule counts by status, formulas in use, average step progress.
    #[tool(
        description = "System statistics: molecule counts by status, formulas in use, workers, and average step progress."
    )]
    fn cosmon_stats(
        &self,
        Parameters(params): Parameters<StatsParams>,
    ) -> Result<CallToolResult, McpError> {
        let filter = MoleculeFilter {
            formula: params
                .formula
                .as_deref()
                .map(|f| {
                    FormulaId::new(f).map_err(|e| {
                        McpError::invalid_params(format!("invalid formula id: {e}"), None)
                    })
                })
                .transpose()?,
            ..MoleculeFilter::default()
        };

        let molecules = self
            .store_for(params.cwd.as_deref())
            .list_molecules(&filter)
            .map_err(|e| {
                McpError::internal_error(format!("failed to list molecules: {e}"), None)
            })?;

        let total = molecules.len();
        let pending = molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Pending)
            .count();
        let queued = molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Queued)
            .count();
        let running = molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Running)
            .count();
        let frozen = molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Frozen)
            .count();
        let completed = molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Completed)
            .count();
        let collapsed = molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Collapsed)
            .count();

        let formulas_in_use: Vec<String> = {
            let mut set: Vec<String> = molecules
                .iter()
                .map(|m| m.formula_id.as_str().to_owned())
                .collect();
            set.sort();
            set.dedup();
            set
        };

        let workers_active: Vec<String> = {
            let mut set: Vec<String> = molecules
                .iter()
                .filter_map(|m| m.assigned_worker.as_ref().map(|w| w.as_str().to_owned()))
                .collect();
            set.sort();
            set.dedup();
            set
        };

        let progress_values: Vec<f64> = molecules.iter().map(step_progress).collect();
        let avg_progress = avg(&progress_values);

        let oldest = molecules.iter().map(|m| m.created_at).min();
        let newest = molecules.iter().map(|m| m.created_at).max();

        let output = serde_json::json!({
            "total": total,
            "by_status": {
                "pending": pending,
                "queued": queued,
                "running": running,
                "frozen": frozen,
                "completed": completed,
                "collapsed": collapsed,
            },
            "formulas_in_use": formulas_in_use,
            "workers_active": workers_active,
            "average_progress": format!("{:.1}%", avg_progress * 100.0),
            "oldest_created": oldest.map(|t| t.to_rfc3339()),
            "newest_created": newest.map(|t| t.to_rfc3339()),
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Aggregate molecules by a grouping field (status, formula, or worker).
    #[tool(
        description = "Aggregate molecules by a grouping field: status, formula, or worker. Returns counts and summaries per group."
    )]
    fn cosmon_aggregate(
        &self,
        Parameters(params): Parameters<AggregateParams>,
    ) -> Result<CallToolResult, McpError> {
        let filter = MoleculeFilter {
            status: params.status.as_deref().and_then(|s| s.parse().ok()),
            ..MoleculeFilter::default()
        };

        let molecules = self
            .store_for(params.cwd.as_deref())
            .list_molecules(&filter)
            .map_err(|e| {
                McpError::internal_error(format!("failed to list molecules: {e}"), None)
            })?;

        let groups: Vec<serde_json::Value> = match params.group_by.as_str() {
            "status" => {
                let mut counts: HashMap<String, Vec<&MoleculeData>> = HashMap::new();
                for m in &molecules {
                    counts.entry(m.status.to_string()).or_default().push(m);
                }
                aggregate_groups(&counts)
            }
            "formula" => {
                let mut counts: HashMap<String, Vec<&MoleculeData>> = HashMap::new();
                for m in &molecules {
                    counts
                        .entry(m.formula_id.as_str().to_owned())
                        .or_default()
                        .push(m);
                }
                aggregate_groups(&counts)
            }
            "worker" => {
                let mut counts: HashMap<String, Vec<&MoleculeData>> = HashMap::new();
                for m in &molecules {
                    let key = m
                        .assigned_worker
                        .as_ref()
                        .map_or("unassigned".to_owned(), |w| w.as_str().to_owned());
                    counts.entry(key).or_default().push(m);
                }
                aggregate_groups(&counts)
            }
            other => {
                return Err(McpError::invalid_params(
                    format!("unsupported group_by: {other} (use status, formula, or worker)"),
                    None,
                ));
            }
        };

        let output = serde_json::json!({
            "group_by": params.group_by,
            "total": molecules.len(),
            "groups": groups,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// Energy consumption report: budget utilization, per-worker and per-molecule token usage.
    #[tool(
        description = "Energy consumption report: budget utilization, per-worker and per-molecule token usage. Reports on weekly, monthly, or per-molecule periods."
    )]
    fn cosmon_energy(
        &self,
        Parameters(params): Parameters<EnergyParams>,
    ) -> Result<CallToolResult, McpError> {
        let state_dir = self.state_dir_for(params.cwd.as_deref());
        let tracker = FileEnergyTracker::new(&state_dir);

        let period = match params.period.as_deref() {
            Some("monthly") => BudgetPeriod::Monthly,
            Some("weekly") | None => BudgetPeriod::Weekly,
            Some(mol_id) => {
                let id = cosmon_core::id::MoleculeId::new(mol_id).map_err(|e| {
                    McpError::invalid_params(
                        format!("invalid period (expected weekly, monthly, or molecule ID): {e}"),
                        None,
                    )
                })?;
                BudgetPeriod::PerMolecule(id)
            }
        };

        let report = tracker.report(&period).map_err(|e| {
            McpError::internal_error(format!("failed to generate energy report: {e}"), None)
        })?;

        let budget_info = tracker.budget().ok().map(|b| {
            serde_json::json!({
                "total": b.total.get(),
                "consumed": b.consumed.get(),
                "remaining": b.remaining().get(),
                "utilization": format!("{:.1}%", b.utilization() * 100.0),
                "alert": b.is_alert(),
                "period": b.period.to_string(),
            })
        });

        let by_worker: Vec<serde_json::Value> = report
            .by_worker
            .iter()
            .map(|(w, t)| {
                serde_json::json!({
                    "worker": w.as_str(),
                    "tokens": t.get(),
                })
            })
            .collect();

        let by_molecule: Vec<serde_json::Value> = report
            .by_molecule
            .iter()
            .map(|(m, t)| {
                serde_json::json!({
                    "molecule": m.as_str(),
                    "tokens": t.get(),
                })
            })
            .collect();

        let output = serde_json::json!({
            "period": period.to_string(),
            "total_tokens": report.total_tokens().get(),
            "productive_tokens": report.productive_tokens.get(),
            "entropy_tax": report.entropy_tax.get(),
            "free_energy_ratio": format!("{:.1}%", report.free_energy_ratio() * 100.0),
            "by_worker": by_worker,
            "by_molecule": by_molecule,
            "budget": budget_info,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }

    /// List available formula templates in the project's formulas directory.
    #[tool(
        description = "List available formula templates (workflow blueprints) in the current project. Returns each template's name, description, id_prefix, step count, and step IDs so agents can discover what formulas are available before calling cosmon_nucleate. Call this FIRST when entering a new project — do not scan the filesystem for .formula.toml files."
    )]
    fn cosmon_fleet_templates(
        &self,
        Parameters(params): Parameters<FleetTemplatesParams>,
    ) -> Result<CallToolResult, McpError> {
        let formulas_dir = self.formulas_dir_for(params.cwd.as_deref());

        let entries = std::fs::read_dir(&formulas_dir).map_err(|e| {
            McpError::internal_error(
                format!(
                    "cannot read formulas directory {}: {e}",
                    formulas_dir.display()
                ),
                None,
            )
        })?;

        let mut templates: Vec<serde_json::Value> = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|e| {
                McpError::internal_error(format!("failed to read directory entry: {e}"), None)
            })?;
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if !name_str.ends_with(".formula.toml") {
                continue;
            }

            let toml_text = std::fs::read_to_string(&path).map_err(|e| {
                McpError::internal_error(format!("failed to read {}: {e}", path.display()), None)
            })?;

            match Formula::parse(&toml_text) {
                Ok(formula) => {
                    let step_ids: Vec<&str> = formula.steps.iter().map(|s| s.id.as_str()).collect();
                    templates.push(serde_json::json!({
                        "name": formula.name.as_str(),
                        "description": formula.description,
                        "id_prefix": formula.id_prefix,
                        "step_count": formula.steps.len(),
                        "steps": step_ids,
                    }));
                }
                Err(e) => {
                    templates.push(serde_json::json!({
                        "file": name_str,
                        "error": format!("parse error: {e}"),
                    }));
                }
            }
        }

        // Sort by name for deterministic output.
        templates.sort_by(|a, b| {
            let a_name = a
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let b_name = b
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            a_name.cmp(b_name)
        });

        let output = serde_json::json!({
            "formulas_dir": formulas_dir.display().to_string(),
            "count": templates.len(),
            "templates": templates,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&output).unwrap_or_default(),
        )]))
    }
}

// ---------------------------------------------------------------------------
// Helper functions for query tools
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Blocking-link helpers (ADR-016 Phase 1)
// ---------------------------------------------------------------------------

/// Parse a list of raw molecule-id strings into validated `MoleculeId`s.
///
/// Reports the MCP parameter name in the error so agents can locate the
/// offending input. Mirrors the CLI's `parse_molecule_ids` so both paths
/// produce the same diagnostics.
fn parse_mol_ids(raw: &[String], field: &str) -> Result<Vec<MoleculeId>, McpError> {
    raw.iter()
        .map(|s| {
            MoleculeId::new(s).map_err(|e| {
                McpError::invalid_params(format!("invalid {field} molecule id `{s}`: {e}"), None)
            })
        })
        .collect()
}

/// Verify every referenced molecule exists before the caller commits any
/// state. A dangling reference would leave the DAG in a half-formed state
/// that no policy can schedule. MCP counterpart of the CLI validator.
fn validate_targets_exist_mcp(
    store: &cosmon_filestore::FileStore,
    ids: &[MoleculeId],
    field: &str,
) -> Result<(), McpError> {
    for id in ids {
        store.load_molecule(id).map_err(|_| {
            McpError::invalid_params(
                format!("{field} references unknown molecule `{id}` — create it first"),
                None,
            )
        })?;
    }
    Ok(())
}

/// Add a typed link to `target_id`'s `typed_links`, skipping the insert if an
/// equivalent link already exists. Idempotent by variant + key match.
/// MCP counterpart of the CLI's `add_symmetric_link`.
fn add_symmetric_link_mcp(
    store: &cosmon_filestore::FileStore,
    target_id: &MoleculeId,
    new_link: MoleculeLink,
) -> Result<(), McpError> {
    let mut target = store.load_molecule(target_id).map_err(|e| {
        McpError::internal_error(
            format!("failed to load {target_id} for symmetry update: {e}"),
            None,
        )
    })?;

    let already = target.typed_links.iter().any(|l| match (l, &new_link) {
        (MoleculeLink::Blocks { target: a }, MoleculeLink::Blocks { target: b })
        | (MoleculeLink::BlockedBy { source: a }, MoleculeLink::BlockedBy { source: b }) => a == b,
        _ => false,
    });
    if already {
        return Ok(());
    }

    target.typed_links.push(new_link);
    target.updated_at = chrono::Utc::now();
    store.save_molecule(target_id, &target).map_err(|e| {
        McpError::internal_error(
            format!("failed to persist symmetry update on {target_id}: {e}"),
            None,
        )
    })?;
    Ok(())
}

/// Compute step progress as a fraction in [0.0, 1.0].
#[allow(clippy::cast_precision_loss)] // step counts are small
fn step_progress(m: &MoleculeData) -> f64 {
    if m.total_steps > 0 {
        m.current_step as f64 / m.total_steps as f64
    } else {
        0.0
    }
}

/// Compute the average of a slice of f64 values.
#[allow(clippy::cast_precision_loss)] // length is small
fn avg(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// Build a `MoleculeFilter` from optional string parameters.
fn build_filter(
    status: Option<&str>,
    worker: Option<&str>,
    formula: Option<&str>,
) -> Result<MoleculeFilter, McpError> {
    Ok(MoleculeFilter {
        fleet: None,
        kind: None,
        status: status.and_then(|s| s.parse().ok()),
        worker: worker
            .map(|w| {
                WorkerId::new(w)
                    .map_err(|e| McpError::invalid_params(format!("invalid worker id: {e}"), None))
            })
            .transpose()?,
        formula: formula
            .map(|f| {
                FormulaId::new(f)
                    .map_err(|e| McpError::invalid_params(format!("invalid formula id: {e}"), None))
            })
            .transpose()?,
        search_text: None,
        project: None,
        tag_globs: Vec::new(),
    })
}

/// Convert a `MoleculeData` to a full JSON representation.
/// Render a [`cosmon_state::wait::WaitOutcome`] into the canonical JSON
/// body emitted by `cosmon_wait`. Extracted as a free function both to
/// keep `cosmon_wait` short (`clippy::too-many-lines`) and to share the
/// exact same wire format with future callers.
///
/// The optional metric fields (`energy`, `entropy`, `temperature`)
/// follow an **omit-if-none** discipline — absent backing data means
/// the key is not present in the response, so agents can degrade
/// gracefully when a probe is missing.
fn render_wait_outcome_json(outcome: &cosmon_state::wait::WaitOutcome) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    output.insert(
        "molecule".to_owned(),
        serde_json::Value::String(outcome.molecule.id.as_str().to_owned()),
    );
    output.insert(
        "status".to_owned(),
        serde_json::Value::String(outcome.reached.to_string()),
    );
    output.insert(
        "reached".to_owned(),
        serde_json::Value::String(outcome.reached.to_string()),
    );
    output.insert(
        "elapsed_seconds".to_owned(),
        serde_json::json!(outcome.elapsed.as_secs_f64()),
    );
    output.insert(
        "current_step".to_owned(),
        serde_json::json!(outcome.molecule.current_step),
    );
    output.insert(
        "total_steps".to_owned(),
        serde_json::json!(outcome.molecule.total_steps),
    );
    output.insert(
        "completed_steps".to_owned(),
        serde_json::json!(outcome
            .molecule
            .completed_steps
            .iter()
            .map(cosmon_core::id::StepId::as_str)
            .collect::<Vec<_>>()),
    );
    output.insert(
        "assigned_worker".to_owned(),
        serde_json::json!(outcome
            .molecule
            .assigned_worker
            .as_ref()
            .map(WorkerId::as_str)),
    );
    output.insert(
        "collapse_reason".to_owned(),
        serde_json::json!(outcome.molecule.collapse_reason),
    );
    output.insert(
        "poll_count".to_owned(),
        serde_json::json!(outcome.metrics.poll_count),
    );
    output.insert(
        "transitions".to_owned(),
        serde_json::json!(outcome.metrics.transitions),
    );
    if let Some(energy) = &outcome.metrics.energy {
        if let Ok(val) = serde_json::to_value(energy) {
            output.insert("energy".to_owned(), val);
        }
    }
    if let Some(entropy) = &outcome.metrics.entropy {
        if let Ok(val) = serde_json::to_value(entropy) {
            output.insert("entropy".to_owned(), val);
        }
    }
    if let Some(temperature) = outcome.metrics.temperature {
        output.insert("temperature".to_owned(), serde_json::json!(temperature));
    }
    serde_json::Value::Object(output)
}

fn molecule_to_json(m: &MoleculeData) -> serde_json::Value {
    serde_json::json!({
        "id": m.id.as_str(),
        "fleet": m.fleet_id.as_str(),
        "formula": m.formula_id.as_str(),
        "status": m.status.to_string(),
        "current_step": m.current_step,
        "total_steps": m.total_steps,
        "completed_steps": m.completed_steps.iter()
            .map(cosmon_core::id::StepId::as_str).collect::<Vec<_>>(),
        "assigned_worker": m.assigned_worker.as_ref().map(WorkerId::as_str),
        "variables": m.variables,
        "collapse_reason": m.collapse_reason,
        "collapsed_step": m.collapsed_step,
        "links": m.links,
        "created_at": m.created_at.to_rfc3339(),
        "updated_at": m.updated_at.to_rfc3339(),
    })
}

/// Build aggregate group summaries from a grouped map.
fn aggregate_groups(groups: &HashMap<String, Vec<&MoleculeData>>) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = groups
        .iter()
        .map(|(key, mols)| {
            let progress_values: Vec<f64> = mols.iter().copied().map(step_progress).collect();
            let avg_progress = avg(&progress_values);
            serde_json::json!({
                "key": key,
                "count": mols.len(),
                "average_progress": format!("{:.1}%", avg_progress * 100.0),
            })
        })
        .collect();
    result.sort_by(|a, b| {
        b.get("count")
            .and_then(serde_json::Value::as_u64)
            .cmp(&a.get("count").and_then(serde_json::Value::as_u64))
    });
    result
}

// ---------------------------------------------------------------------------
// ServerHandler — MCP protocol metadata + tool routing
// ---------------------------------------------------------------------------

impl ServerHandler for CosmonService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }

    /// Dispatch a tool call, installing the per-request [`HttpStatePin`] as a
    /// task-local for the duration so every tool's state / formulas / config
    /// resolution is pinned to the tenant root — making the `cwd` parameter
    /// inert on the HTTP path.
    ///
    /// This replaces the `#[tool_handler]` macro's generated `call_tool`
    /// (identical dispatch) with one extra step: the pin is read from the
    /// `http::request::Parts` that the Streamable-HTTP transport injects into
    /// the request extensions, then the whole tool future runs inside
    /// `HTTP_STATE_PIN.scope(...)`. On the stdio path no `Parts` are present,
    /// the pin is `None`, and `cwd` walk-up behaves exactly as before.
    ///
    /// The pin is re-derived from the request on **every** call — it is never
    /// cached on `self` or in the MCP session (panel D1: the session must not
    /// hold tenant state). Reusing a peer's `Mcp-Session-Id` therefore cannot
    /// pivot tenants: the host gate re-resolves the noyau from the bearer JWT
    /// per request and re-inserts the matching pin.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let pin = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<HttpStatePin>())
            .cloned();
        let tcc = ToolCallContext::new(self, request, context);
        HTTP_STATE_PIN
            .scope(pin, async move { self.tool_router.call(tcc).await })
            .await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.tool_router.list_all(),
            meta: None,
            next_cursor: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }
}

/// Full workflow instructions for the MCP server.
///
/// This is the ONLY documentation agents read to understand how to use Cosmon.
/// It must convey the complete workflow, not just a tool catalog.
const INSTRUCTIONS: &str = "\
Cosmon MCP server — orchestrate agent molecule lifecycles.

## The One Rule

**You are a CALLER, not a WORKER.** After nucleating a molecule, hand off \
via `cs tackle <id>`. Never execute molecule steps yourself.

## Mental Model (read this first)

1. **Molecules are state machines defined by formulas.** A molecule is a work \
   unit that advances through typed steps; the formula is its script.
2. **Surfaces (STATUS.md, ISSUES.md, IDEAS.md, docs/adr/INDEX.md, GitHub Issues) \
   are AUTO-GENERATED projections.** They are derived views, not inputs.
3. **The source of truth is `.cosmon/state/` (JSON on disk).** Mutate state via \
   MCP tools; run `cs reconcile` to refresh the surfaces.
4. **Execution dispatch is `cs tackle <id>`.** Callers of `cosmon_nucleate` are \
   REQUESTERS, not EXECUTORS. For any molecule whose kind implies worker \
   execution (🔧 task, 🐛 issue, 🧠 deliberation, ⚡ signal that triggers work), \
   the canonical dispatch verb is `cs tackle <id>` — it launches an isolated \
   Claude worker in a git worktree + tmux pane, injects the bootstrap prompt, \
   and binds the molecule to that worker. The caller never runs the formula \
   steps themselves.
5. **Walk-up discovery and project sovereignty.** Cosmon resolves its \
   `.cosmon/` directory by walking up from the caller's working directory. \
   Each project owns its own formulas, state, and surfaces — there is no \
   global registry. When entering a new project, call `cosmon_fleet_templates` \
   to discover what formulas exist. Do NOT scan the filesystem for \
   `.formula.toml` files, grep for formula names, or assume templates from \
   another project apply here. Anti-pattern: **filesystem archaeology** — \
   an agent that globs for `**/*.formula.toml` or reads \
   `.cosmon/formulas/*.toml` directly instead of calling the discovery tool. \
   The tool handles parse errors, sorts deterministically, and respects the \
   `cwd` routing — raw filesystem reads bypass all of that.

## Anti-Patterns (read before editing anything)

### Tier 1 — Critical (system-breaking)

These violate the source of truth or the execution model. They cause lost \
work, conflicts, or broken state.

1. **DO NOT act out formula steps inline after calling `cosmon_nucleate`.** \
   This is the **agent-attacks-molecule-itself** anti-pattern: the caller \
   sees the freshly minted molecule and starts executing steps in their own \
   turn — editing files, calling `Agent()`, or hand-editing molecule state. \
   That work happens in the wrong process, outside the worktree guardrail, \
   and will be lost or conflict with the real worker. The `cosmon_nucleate` \
   response deliberately hides raw step bodies and names `cs tackle <id>` \
   as the required next action. Respect it.

2. **DO NOT edit STATUS.md, ISSUES.md, IDEAS.md, docs/adr/INDEX.md, or \
   GitHub issues directly.** They are regenerated by `cs reconcile` and \
   your edits will be silently overwritten. This is the single most common \
   way agents lose work. Change molecule state via MCP tools, then run \
   `cs reconcile`.

3. **Never invent coordination logic — cosmon IS the coordinator.** Do not \
   build ad-hoc dispatch, scheduling, or inter-agent messaging outside of \
   cosmon tools. The canonical dispatch verb is `cs tackle <id>`. After \
   `cosmon_nucleate ... --assign <worker>`, the only correct next action for \
   a worker-executable molecule is `cs tackle <molecule-id>`. Do not call \
   `Agent()`, do not run Bash/Edit/Write to simulate the steps, do not \
   `cosmon_evolve` from the caller session. Hand off and let the worker run.

### Tier 2 — Important (causes drift)

These do not break the system immediately but cause silent state drift \
that compounds into real problems.

4. Forgetting `cs reconcile` after a batch of mutations — surfaces drift silently.
5. Not passing `assign` to `cosmon_nucleate` — creates orphaned pending molecules.
6. Interpreting `pending` status as `running` — pending means NOTHING IS HAPPENING.

### Tier 3 — Hygiene (best practice)

These are discipline violations that reduce observability or waste effort.

7. Calling `cosmon_nucleate` without checking `cosmon_ensemble` first.
8. Not calling `cosmon_observe` before `cosmon_evolve`.

## Typical Flows (canonical workflows)

Each flow pairs MCP mutations with the companion CLI reconcile step. Run \
`cs reconcile` once after a batch of mutations, not after every single one.

- **Dispatch worker-executable work (THE canonical flow)**: \
  `cosmon_ensemble` (verify fleet) → \
  `cosmon_nucleate <formula> --vars k=v --assign <worker>` → \
  `cs tackle <molecule-id>` (launches worker in worktree+tmux, injects \
  bootstrap prompt) → \
  `cosmon_wait <molecule-id>` (block until terminal) → \
  `cs done <molecule-id>` (human-only teardown: merge, kill tmux, remove \
  worktree, purge fleet). \
  The caller never executes the formula steps. `cs tackle` is the ONLY \
  correct dispatch verb for task / issue / deliberation / worker-signal \
  molecules. See the `Anti-Patterns` section above (Tier 1).

- **Create tracked issue / task / idea (bookkeeping only, not yet dispatched)**: \
  `cosmon_ensemble` (verify fleet) → \
  `cosmon_nucleate <formula> --vars k=v --assign <worker>` → \
  `cs reconcile` (project into ISSUES.md / STATUS.md / GitHub). \
  Follow up with `cs tackle <id>` when you want the worker to actually run.

- **Advance a molecule's state**: \
  `cosmon_observe <id>` (read current step + exit criteria) → \
  `cosmon_evolve <id> --evidence '<summary>'` → \
  `cs reconcile` (surfaces reflect new status/step).

- **Inspect without mutating**: \
  `cosmon_observe <id>` (single molecule) or \
  `cosmon_ensemble` (fleet + counts) or \
  `cosmon_search <text>` / `cosmon_list --filter ...`.

- **Close out a molecule**: \
  `cosmon_complete <id> --reason '<why>'` (shortcut, batch-capable) or \
  `cosmon_collapse <id> --reason '<why>'` (failure, terminal) → \
  `cs reconcile`.

- **Restructure work (idea → tasks, tasks → decision)**: \
  `cosmon_decay <source> --formula <task-formula> --count N` or \
  `cosmon_merge --sources <a,b,c> --formula <formula>` or \
  `cosmon_transform <id> --to task` → \
  `cs reconcile`.

## Fleet Setup (entering a new project)

When you first interact with a Cosmon-enabled project:

1. **`cosmon_fleet_templates`** — discover available formulas. Returns name, \
   description, step count, and step IDs for each template in the project.
2. **`cosmon_ensemble`** — check what workers exist and what molecules are in \
   flight. If no workers exist, molecules cannot execute.
3. Pick the right formula and `cosmon_nucleate` with `--assign <worker>`.

**Anti-pattern: filesystem archaeology.** Do NOT glob for `**/*.formula.toml`, \
read `.cosmon/formulas/` directly, or assume formulas from another project \
apply. Each project owns its own templates. `cosmon_fleet_templates` is the \
canonical discovery tool — it handles parse errors, sorts deterministically, \
and respects `cwd` routing for multi-project MCP servers.

## Companion CLI (`cs`)

MCP and `cs` are complementary, not redundant:

- **MCP tools** (`cosmon_*`) — orchestration: nucleate, evolve, observe, \
  complete, decay, merge, transform. This is what agents call.
- **`cs` CLI** — everything MCP does not expose: setup (`cs init`), surface \
  projection (`cs reconcile`), worker lifecycle (`cs tackle`, `cs done`), \
  watchdog (`cs patrol --propel`), live loop (`cs watch`), governance.

Agents should prefer MCP for state mutations and reach for `cs` for \
reconcile + the handful of operations it exclusively owns. Run `cs --help` \
for the full command list.

## Core workflow tools (in order of typical use)

1. **cosmon_ensemble** — ALWAYS call first. Check what workers exist and what \
   molecules are in flight. If no workers exist, molecules cannot execute.

2. **cosmon_nucleate** — Create a molecule from a formula template. \
   Pass 'assign' to bind it to a worker. Without a worker, the molecule is \
   PENDING (inert — nothing will happen). With a worker, it becomes QUEUED. \
   Run `cs reconcile` after to project onto surfaces.

3. **cosmon_observe** — Inspect the molecule's current step and exit criteria. \
   Call this BEFORE evolving to understand what evidence is needed.

4. **cosmon_evolve** — Advance to the next step with evidence. The first evolve \
   promotes QUEUED → RUNNING. When the last step is evolved, the molecule completes. \
   Run `cs reconcile` after to refresh surfaces.

5. **cosmon_freeze / cosmon_thaw** — Pause and resume molecules.

6. **cosmon_collapse** — Mark a molecule as permanently failed (terminal).
7. **cosmon_complete** — Shortcut to mark a molecule as done (no evolve ceremony needed). \
   Supports batch via comma-separated IDs. Run `cs reconcile` after.

## Interactions (molecule-to-molecule operations)

8. **cosmon_decay** — 💫 Decompose one molecule into N children (idea → tasks).
9. **cosmon_merge** — 🔀 Synthesize N molecules into one (tasks → decision).
10. **cosmon_transform** — 🔄 Change a molecule's kind (idea → task).

All three mutate state — run `cs reconcile` after to refresh surfaces.

## Molecule kinds

Molecules have a cognitive nature (kind) orthogonal to their formula:
- 💡 **idea**: unstructured insight, can decay or transform
- 🔧 **task**: actionable work, can merge
- 📐 **decision**: architecture record (terminal kind)
- 🐛 **issue**: tracked problem, can decay
- ⚡ **signal**: ephemeral observation, auto-completes
- 🧠 **deliberation**: structured multi-perspective analysis that produces a synthesis

## Status lifecycle

```
pending → queued → running → completed
                 ↘ frozen ↗
                 ↘ collapsed (terminal)
```

- **pending**: created, no worker assigned — INERT, nothing happens
- **queued**: assigned to a worker, waiting to start
- **running**: actively being worked on
- **frozen**: paused, can be thawed
- **completed**: all steps done (terminal)
- **collapsed**: failed (terminal)

## Communication (ADR-015 Signal Bus)

Inter-agent messaging flows through a structured signal bus (SQLite WAL). \
The channel is selected by message priority:

- **Critical** → Dolt Bead (durable audit trail)
- **High** → Signal Bus + tmux push hint (structured + fast)
- **Normal** → Signal Bus (structured, queryable)
- **Low** → JSONL (ephemeral, minimal overhead)

Use **cosmon_nudge** for push hints only — it injects raw text into tmux. \
For all structured messaging between agents, use the signal bus tools \
(cosmon_signal/cosmon_listen/cosmon_pending — coming in Phase 2).

## Query tools

cosmon_search (text search), cosmon_get (by ID), cosmon_list (filter/sort/paginate), \
cosmon_count (fast count), cosmon_export (json/ndjson/csv), cosmon_stats (statistics), \
cosmon_aggregate (group by status/formula/worker), cosmon_energy (token tracking), \
cosmon_fleet_templates (formula catalog).

## Surface projection

Cosmon projects internal state onto standard files (STATUS.md, ISSUES.md, GitHub Issues) \
so that non-participants can see project status without using cosmon tools. \
These surfaces are derived views -- the source of truth is always `.cosmon/state/`.

**When to reconcile:** After batch operations that change molecule state \
(nucleate, evolve, collapse, decay, merge, transform, freeze, thaw), surfaces \
may be stale. Run `cs reconcile` to re-project all surfaces.

**Tools that affect surfaces:** cosmon_nucleate, cosmon_evolve, cosmon_collapse, cosmon_complete, \
cosmon_freeze, cosmon_thaw, and the interaction tools (decay, merge, transform). \
Any tool that changes molecule status, step, or kind can cause surface drift.

**Best practice:** After a batch of mutations, call `cs reconcile` once \
(not after every individual mutation). The projection is idempotent.

## Worker autonomy (cs tackle)

`cs tackle <molecule-id>` launches a Claude worker in an isolated git \
worktree with a bootstrap prompt built from the molecule's state.

**All workers run in bypassPermissions mode by default**, regardless of \
molecule kind (idea, task, issue, etc.). The worker is isolated in a git \
worktree — it cannot break main. The formula steps and exit criteria are \
the guardrails, not permission prompts.

- **Default**: fully autonomous (bypassPermissions)
- **Opt-in supervision**: `cs tackle <mol> --permission-mode plan`
- **Design principle**: autonomy is the default, supervision is opt-in

A worker that stops to ask permission at every step is useless for \
autonomous execution. If you are an agent launched via cs tackle, you \
should complete ALL steps without stopping to ask. Run the Definition of \
Done gates (build, test, lint, format), commit, push, and create a PR.

## Developer workflow

The full cycle for tackling a molecule:
1. `cs tackle <mol>` — launch worker in worktree + tmux
2. Worker implements, tests, commits, pushes, creates PR
3. `cs review <mol> --merge` — merge PR + cleanup (planned)
4. `cs complete <mol> --reason 'merged'` — mark done (planned)
5. `cs reconcile` — update surfaces

## Monitoring — the operator's toolkit

The human pilot (and any supervising agent) observes the fleet via a small, \
canonical set of tools. Do not invent alternatives, do not scrape logs by \
hand, do not attach to worker tmux sessions.

- **`cs peek`** — the fractal portal. TUI that lists sessions + molecules. \
  `p` shows the tmux pane capture of the selected worker, `j/k` navigate \
  (right pane auto-follows), planned detail tabs (`b` briefing, `l` log, \
  `e` events, `s` synthesis, `r` responses, `n` notes, `g` git) let the \
  operator descend into any molecule without leaving the TUI. One keystroke \
  = one fractal descent.
- **`cs peek --all`** — aggregates across every tmux socket and every \
  `.cosmon/` on disk. Use it from any directory to see the full \
  multi-galaxy fleet (cosmon + wiki2 + earshot + …).
- **`cs ensemble`** — snapshot view of molecule state (JSON-friendly with \
  `--json`). Use `--tag temp:hot` to see the actionable queue.
- **`cs wait <id> &`** — the ONLY correct way to block on a worker. Pairs \
  with `&` so the pilot stays responsive and is notified on completion.
- **`cs observe <id>`** — single-molecule state dump for scripts and \
  one-off checks. NEVER poll in a shell loop; use `cs wait` instead.

**Anti-patterns for the human operator:**
- `tmux attach` to a worker's session — breaks its rendering, confuses the \
  agent, and there is nothing to gain over `cs peek` + `p`.
- `watch cs observe …` / `while true; do cs observe …` — burns CPU, \
  misses transitions between polls. Use `cs wait`.
- `tail -f` on `.cosmon/state/**/events.jsonl` — readable but unstructured; \
  `cs peek`'s event tab (planned) is the supported view.
- `cat` on `briefing.md` / `synthesis.md` from a random terminal — works \
  but loses context. Prefer the detail tabs in `cs peek`.

Principle: **observability is not a dashboard, it is a fractal portal**. \
One tool (`cs peek`), recursive, from fleet overview down to per-molecule \
artifact. When tempted to reach for `tmux`, `tail`, or `cat` — reach for \
`cs peek` first.

## Architectural invariants (for operators debugging cross-project issues)

Cosmon has structural mechanisms that shape what the system *can* do, not \
just what it does today. Before adding a new command, renaming a verb, \
broadening scope, or debugging why a command did (or did not) do what you \
expected, read **`docs/architectural-invariants.md`**. It encodes:

- The **two-layer model** (Transactional Core + future Resident Runtime).
- The **three regimes** — Inert / Propelled / Autonomous — and which \
  commands are legal in each.
- **Per-command perimeters**: `cs tackle` is Inert → Propelled (human \
  only); `cs patrol --propel` maintains Propelled; `cs evolve` / \
  `cs complete` are worker-callable; `cs done` is human-only teardown.
- The **worker/human boundary**: workers cannot self-destroy, humans \
  assume the worker is done.
- A **coherence checklist** run before any non-trivial change.

The governing ADR is `docs/adr/016-autonomy-regimes-and-resident-runtime.md`. \
When a command behaves surprisingly across projects, the answer is almost \
always in the invariants document — read it before patching symptoms.";

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_filestore::FileStore;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        (tmp, store)
    }

    fn sample_mol(id: &str) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status: MoleculeStatus::Pending,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 2,
            current_step: 0,
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
            propel_count: 0,
            last_propelled_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
            adapter: None,
        }
    }

    #[test]
    fn test_parse_mol_ids_valid() {
        let raw = vec![
            "task-20260409-0001".to_owned(),
            "task-20260409-0002".to_owned(),
        ];
        let parsed = parse_mol_ids(&raw, "blocks").unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].as_str(), "task-20260409-0001");
    }

    #[test]
    fn test_parse_mol_ids_rejects_empty() {
        let raw = vec![String::new()];
        let err = parse_mol_ids(&raw, "blocks").unwrap_err();
        assert!(err.message.contains("blocks"));
    }

    #[test]
    fn test_validate_targets_exist_ok_when_all_present() {
        let (_tmp, store) = make_store();
        let a = sample_mol("task-20260409-mcpa");
        let b = sample_mol("task-20260409-mcpb");
        store.save_molecule(&a.id, &a).unwrap();
        store.save_molecule(&b.id, &b).unwrap();

        let ids = vec![a.id.clone(), b.id.clone()];
        validate_targets_exist_mcp(&store, &ids, "blocks").unwrap();
    }

    #[test]
    fn test_validate_targets_exist_errors_on_missing() {
        let (_tmp, store) = make_store();
        let ids = vec![MoleculeId::new("task-20260409-gone").unwrap()];
        let err = validate_targets_exist_mcp(&store, &ids, "blocks").unwrap_err();
        assert!(err.message.contains("unknown molecule"));
    }

    #[test]
    fn test_add_symmetric_link_mcp_adds_to_empty_target() {
        let (_tmp, store) = make_store();
        let target = sample_mol("task-20260409-mcp1");
        store.save_molecule(&target.id, &target).unwrap();

        let source_id = MoleculeId::new("task-20260409-src1").unwrap();
        add_symmetric_link_mcp(
            &store,
            &target.id,
            MoleculeLink::BlockedBy {
                source: source_id.clone(),
            },
        )
        .unwrap();

        let reloaded = store.load_molecule(&target.id).unwrap();
        assert_eq!(reloaded.typed_links.len(), 1);
        match &reloaded.typed_links[0] {
            MoleculeLink::BlockedBy { source } => assert_eq!(source, &source_id),
            _ => panic!("expected BlockedBy variant"),
        }
    }

    #[test]
    fn test_add_symmetric_link_mcp_is_idempotent() {
        let (_tmp, store) = make_store();
        let target = sample_mol("task-20260409-mcp2");
        store.save_molecule(&target.id, &target).unwrap();

        let source_id = MoleculeId::new("task-20260409-src2").unwrap();
        let link = MoleculeLink::BlockedBy {
            source: source_id.clone(),
        };

        // Add twice — should remain a single entry.
        add_symmetric_link_mcp(&store, &target.id, link.clone()).unwrap();
        add_symmetric_link_mcp(&store, &target.id, link).unwrap();

        let reloaded = store.load_molecule(&target.id).unwrap();
        assert_eq!(
            reloaded.typed_links.len(),
            1,
            "idempotent: second add must not duplicate"
        );
    }

    /// The MCP INSTRUCTIONS constant is the ONLY onboarding doc agents read.
    /// It must contain the key mental-model phrases so regressions (someone
    /// rewriting it and dropping the anti-patterns) are caught in CI.
    ///
    /// Background: a prior session lost work when an agent edited ISSUES.md
    /// directly, unaware that `cs reconcile` regenerates surfaces from state.
    /// The phrases pinned below are the minimum set that would have prevented
    /// that error.
    #[test]
    fn test_instructions_contains_mental_model_phrases() {
        // Source-of-truth statement.
        assert!(
            INSTRUCTIONS.contains("source of truth"),
            "INSTRUCTIONS must explicitly name the source of truth"
        );
        // Critical anti-pattern warning.
        assert!(
            INSTRUCTIONS.contains("DO NOT edit"),
            "INSTRUCTIONS must warn agents not to edit surfaces directly"
        );
        // Reconcile command must be surfaced.
        assert!(
            INSTRUCTIONS.contains("cs reconcile"),
            "INSTRUCTIONS must mention `cs reconcile` as the surface refresh step"
        );
        // Mental-model heading.
        assert!(
            INSTRUCTIONS.contains("Mental Model"),
            "INSTRUCTIONS must open with a Mental Model section"
        );
        // Anti-patterns heading.
        assert!(
            INSTRUCTIONS.contains("Anti-Patterns"),
            "INSTRUCTIONS must include an Anti-Patterns section"
        );
        // Typical flows heading.
        assert!(
            INSTRUCTIONS.contains("Typical Flows"),
            "INSTRUCTIONS must include a Typical Flows section"
        );
        // Companion CLI heading.
        assert!(
            INSTRUCTIONS.contains("Companion CLI"),
            "INSTRUCTIONS must explain the MCP ↔ CLI relationship"
        );
    }

    /// Guards against regression: the MCP
    /// INSTRUCTIONS string must physically contain `cs tackle` framed as
    /// THE canonical dispatch verb, must name the
    /// agent-attacks-molecule-itself anti-pattern, and must point at
    /// `docs/architectural-invariants.md` for operators debugging
    /// cross-project issues.
    ///
    /// Rationale: the absence of `cs tackle` in the only onboarding doc
    /// agents read is the primary enabler of the anti-pattern where a
    /// caller reads a freshly minted molecule and starts running its
    /// steps inline (calling `Agent()`, hand-editing files, etc.)
    /// instead of handing off to a worker. Making that absence impossible
    /// is cheap, structural, and independent of the schema-level defense
    /// added in the nucleate response.
    #[test]
    fn test_instructions_names_cs_tackle_and_invariants_pointer() {
        // `cs tackle` must appear — it is the canonical dispatch verb.
        assert!(
            INSTRUCTIONS.contains("cs tackle"),
            "INSTRUCTIONS must name `cs tackle` — the canonical dispatch verb"
        );

        // The anti-pattern must be named explicitly so agents can
        // recognize it in their own behavior.
        assert!(
            INSTRUCTIONS.contains("agent-attacks-molecule-itself"),
            "INSTRUCTIONS must name the `agent-attacks-molecule-itself` \
             anti-pattern so agents can recognize it in their own behavior"
        );

        // The caller/executor distinction must be surfaced in the
        // mental model or anti-patterns — this is what physically blocks
        // the anti-pattern at the onboarding layer.
        assert!(
            INSTRUCTIONS.contains("REQUESTERS, not EXECUTORS")
                || INSTRUCTIONS.contains("caller is a requester"),
            "INSTRUCTIONS must surface the caller=requester / \
             worker=executor distinction"
        );

        // The `cosmon_nucleate → cs tackle → cosmon_wait → cs done`
        // canonical flow must appear as a Typical Flow so agents have a
        // copy-pasteable recipe instead of improvising.
        let flows_idx = INSTRUCTIONS
            .find("## Typical Flows")
            .expect("INSTRUCTIONS must have a Typical Flows section");
        let companion_idx = INSTRUCTIONS[flows_idx..]
            .find("## Companion CLI")
            .map(|o| flows_idx + o)
            .expect("Typical Flows must be followed by Companion CLI");
        let flows = &INSTRUCTIONS[flows_idx..companion_idx];
        assert!(
            flows.contains("cs tackle"),
            "Typical Flows section must include a `cs tackle` dispatch flow"
        );
        assert!(
            flows.contains("cosmon_wait"),
            "Typical Flows section must include `cosmon_wait` as the \
             caller-side blocking step after dispatch"
        );
        assert!(
            flows.contains("cs done"),
            "Typical Flows section must include `cs done` as the \
             human-only teardown step"
        );

        // Pointer to architectural invariants — operators debugging
        // cross-project issues need a breadcrumb to the governing doc.
        assert!(
            INSTRUCTIONS.contains("docs/architectural-invariants.md"),
            "INSTRUCTIONS must point at `docs/architectural-invariants.md` \
             for operators debugging cross-project issues"
        );
        assert!(
            INSTRUCTIONS.contains("Architectural invariants"),
            "INSTRUCTIONS must have an `Architectural invariants` section \
             heading so the pointer is discoverable"
        );
        // The ADR reference grounds the pointer in a durable decision.
        assert!(
            INSTRUCTIONS.contains("016-autonomy-regimes-and-resident-runtime.md"),
            "INSTRUCTIONS must cite the governing ADR (016) so readers \
             know which decision the invariants document encodes"
        );
    }

    /// INSTRUCTIONS must contain Mental Model item 5 (walk-up discovery and
    /// project sovereignty) and the Fleet Setup section with its anti-pattern
    /// warning, so agents entering a new project call `cosmon_fleet_templates`
    /// instead of scanning the filesystem.
    #[test]
    fn test_instructions_contains_fleet_templates_guidance() {
        assert!(
            INSTRUCTIONS.contains("cosmon_fleet_templates"),
            "INSTRUCTIONS must mention `cosmon_fleet_templates` as the discovery tool"
        );
        assert!(
            INSTRUCTIONS.contains("Fleet Setup"),
            "INSTRUCTIONS must have a Fleet Setup section"
        );
        assert!(
            INSTRUCTIONS.contains("filesystem archaeology"),
            "INSTRUCTIONS must name the filesystem archaeology anti-pattern"
        );
        assert!(
            INSTRUCTIONS.contains("Walk-up discovery"),
            "Mental Model must include item 5 on walk-up discovery"
        );
        assert!(
            INSTRUCTIONS.contains("project sovereignty"),
            "Mental Model item 5 must name project sovereignty"
        );
    }

    /// `cosmon_nucleate` response must frame the caller as a requester, not
    /// an executor. This contract physically blocks the
    /// "agent-attacks-molecule-itself" anti-pattern
    /// at the schema level: callers are told what to do next (`cs tackle`),
    /// why, and — crucially — what NOT to do. Raw formula steps must NOT
    /// leak into the caller-facing response, since exposing them has been
    /// observed to nudge agents into acting out the steps inline.
    ///
    /// This is a source-level check because the rmcp tool macro expands to
    /// a static handler that we cannot invoke reflectively without a full
    /// server harness; the same technique is used by
    /// `test_mutation_tool_descriptions_mention_reconcile`.
    #[test]
    fn test_cosmon_nucleate_response_schema_enforces_caller_role() {
        let src = include_str!("tools.rs");
        let nuc_idx = src
            .find("fn cosmon_nucleate")
            .expect("fn cosmon_nucleate not found");
        // The fn body + json! response is well under 15 KB; this window
        // intentionally stops before `fn cosmon_evolve` so we do not see
        // unrelated code.
        let evolve_idx = src[nuc_idx..]
            .find("fn cosmon_evolve")
            .map(|o| nuc_idx + o)
            .expect("fn cosmon_evolve not found after cosmon_nucleate");
        let body = &src[nuc_idx..evolve_idx];

        assert!(
            body.contains("\"caller_role\""),
            "nucleate response must include caller_role key"
        );
        assert!(
            body.contains("You are the CALLER"),
            "caller_role must state the caller does not execute the work"
        );
        assert!(
            body.contains("\"next_action\""),
            "nucleate response must include next_action object"
        );
        assert!(
            body.contains("\"command\""),
            "next_action must include a command field"
        );
        assert!(
            body.contains("cs tackle"),
            "next_action.command must prescribe `cs tackle <id>`"
        );
        assert!(
            body.contains("\"why\""),
            "next_action must include a why field explaining the command"
        );
        assert!(
            body.contains("\"do_not\""),
            "next_action must include a do_not guardrail array"
        );
        assert!(
            body.contains("do not execute formula steps inline"),
            "do_not must forbid executing steps inline"
        );
        assert!(
            body.contains("do not call Agent()"),
            "do_not must forbid calling Agent() to act out personas"
        );
        assert!(
            body.contains("do not edit molecule files by hand"),
            "do_not must forbid hand-editing molecule files"
        );
        assert!(
            body.contains("\"plan_summary\""),
            "nucleate response must include a one-sentence plan_summary"
        );

        // Raw formula steps must NOT appear in the caller-facing response.
        // The removed `next_steps` array used to leak step-shaped hints and
        // is banned by this contract. `total_steps` (a scalar count) stays.
        assert!(
            !body.contains("\"next_steps\""),
            "nucleate response must not expose next_steps \
             (removed to block agent-attacks-molecule anti-pattern)"
        );
        assert!(
            !body.contains("\"steps\":"),
            "nucleate response must not expose a raw steps array; \
             callers should not see formula step bodies"
        );
    }

    /// Mutation tool descriptions must tell agents to reconcile. This is what
    /// appears in the tool catalog — the onboarding doc is a second line of
    /// defense, not the first.
    #[test]
    fn test_mutation_tool_descriptions_mention_reconcile() {
        // The rmcp tool macro expands to a static description; we can't read
        // it reflectively without wiring up the full server. Instead, read the
        // source file and grep for the `cs reconcile` cue on each mutation
        // description. If this file is ever split, update the path.
        let src = include_str!("tools.rs");

        // Find each `fn cosmon_<mutation>` and check that the preceding ~15
        // lines contain `cs reconcile`. This is a cheap proxy for "the
        // `#[tool(description=...)]` above it mentions reconcile".
        for mutation in [
            "fn cosmon_nucleate",
            "fn cosmon_evolve",
            "fn cosmon_complete",
            "fn cosmon_collapse",
            "fn cosmon_decay",
            "fn cosmon_merge",
            "fn cosmon_transform",
        ] {
            let idx = src
                .find(mutation)
                .unwrap_or_else(|| panic!("{mutation} not found in tools.rs"));
            // Look back ~2000 chars to cover the attribute block.
            let start = idx.saturating_sub(2000);
            let window = &src[start..idx];
            assert!(
                window.contains("cs reconcile"),
                "{mutation}: tool description must mention `cs reconcile`"
            );
        }
    }

    /// `cosmon_wait` on an already-terminal molecule returns immediately
    /// via the shared `wait_for_status` kernel. This drives the happy path
    /// of the `cosmon_nucleate → tackle → cosmon_wait → done` trinity: by
    /// the time the wait call runs, the worker has already flipped the
    /// molecule to Completed, so no poll cycles are needed.
    #[test]
    fn test_cosmon_wait_returns_immediately_on_completed_molecule() {
        let (tmp, store) = make_store();
        let mut mol = sample_mol("task-20260409-wmcp");
        mol.status = MoleculeStatus::Completed;
        mol.current_step = mol.total_steps;
        store.save_molecule(&mol.id, &mol).unwrap();

        let started = std::time::Instant::now();
        let outcome = cosmon_state::wait::wait_for_status(
            &store,
            &mol.id,
            &[MoleculeStatus::Completed, MoleculeStatus::Collapsed],
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(1),
        )
        .expect("should return immediately");
        let wall = started.elapsed();
        assert_eq!(outcome.reached, MoleculeStatus::Completed);
        assert_eq!(outcome.molecule.id, mol.id);
        assert!(
            wall < std::time::Duration::from_millis(500),
            "already-terminal wait must not poll — wall={wall:?}"
        );
        // `tmp` kept alive until here so the file-backed store is still valid.
        drop(tmp);
    }

    /// `cosmon_wait` maps `WaitError::Timeout` onto an MCP internal error —
    /// mirrors what agents see when the deadline expires.
    #[test]
    fn test_cosmon_wait_surfaces_timeout() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-wtmo"); // Pending — never terminal.
        store.save_molecule(&mol.id, &mol).unwrap();

        let err = cosmon_state::wait::wait_for_status(
            &store,
            &mol.id,
            &[MoleculeStatus::Completed],
            std::time::Duration::from_millis(5),
            std::time::Duration::from_millis(1),
        )
        .expect_err("should time out");
        assert!(matches!(err, WaitError::Timeout { .. }));
        drop(tmp);
    }

    /// Missing molecule path: the wait kernel must surface
    /// [`WaitError::MoleculeNotFound`] so `cosmon_wait` can map it onto
    /// `McpError::invalid_params` instead of a misleading timeout.
    #[test]
    fn test_cosmon_wait_rejects_missing_molecule() {
        let (tmp, store) = make_store();
        let ghost = MoleculeId::new("task-20260409-ghst").unwrap();
        let err = cosmon_state::wait::wait_for_status(
            &store,
            &ghost,
            &[MoleculeStatus::Completed],
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(1),
        )
        .expect_err("missing molecule should fail fast");
        assert!(matches!(err, WaitError::MoleculeNotFound(_)));
        drop(tmp);
    }

    /// The per-call helpers must route a client-supplied `cwd` through
    /// `resolve_state_dir_from` / `resolve_formulas_dir_from` so a long-lived
    /// MCP server answers each request against the caller's project, not its
    /// own startup-time dirs. Absent cwd must fall back to the configured
    /// server paths (backward compatible with clients that do not yet pass
    /// `cwd`).
    ///
    /// We can't clear `COSMON_STATE_DIR` / `COSMON_FORMULAS_DIR` inside the
    /// test because `cosmon-mcp` forbids unsafe code and Rust's
    /// `env::remove_var` is unsafe. If those env vars are set when the test
    /// runs, the walk-up path is overridden by the env and the assertions
    /// below become meaningless; skip the walk-up checks in that case
    /// rather than lying about coverage.
    #[test]
    fn test_state_dir_for_routes_cwd_through_walk_up() {
        let tmp = tempfile::tempdir().unwrap();

        // Caller project A with its own .cosmon/ — real dirs so walk-up
        // actually hits them. Per ADR-069, a cosmon project root is a
        // `.cosmon/` carrying `config.toml`; seed that marker so
        // walk-up recognises this fixture as a project.
        let project_a = tmp.path().join("proj_a");
        let cosmon_a = project_a.join(".cosmon");
        let nested_a = project_a.join("src/deep");
        std::fs::create_dir_all(&cosmon_a).unwrap();
        std::fs::create_dir_all(cosmon_a.join("formulas")).unwrap();
        std::fs::create_dir_all(&nested_a).unwrap();
        std::fs::write(cosmon_a.join("config.toml"), "# seeded by test\n").unwrap();

        // "Server" is initialized against an unrelated root.
        let server_root = tmp.path().join("server_root");
        std::fs::create_dir_all(&server_root).unwrap();
        let service = CosmonService {
            store_dir: Arc::new(server_root.clone()),
            formulas_dir: Arc::new(server_root.join("formulas")),
            tool_router: CosmonService::tool_router(),
        };

        // Absent cwd → server defaults. This branch never reads the
        // environment, so it is safe regardless of ambient env vars.
        assert_eq!(service.state_dir_for(None), server_root);
        assert_eq!(service.formulas_dir_for(None), server_root.join("formulas"));

        // Supplied cwd → walk-up finds project A's .cosmon/. Only assert
        // the walk-up outcome when no environment override is in force.
        let cwd_a = nested_a.to_string_lossy().into_owned();
        // Walk-up now canonicalizes paths (e.g. /var → /private/var on macOS).
        let cosmon_a_canon = cosmon_a.canonicalize().unwrap();
        if std::env::var_os("COSMON_STATE_DIR").is_none() {
            assert_eq!(
                service.state_dir_for(Some(&cwd_a)),
                cosmon_a_canon.join("state"),
                "state_dir_for must walk up from supplied cwd to project A"
            );
        }
        if std::env::var_os("COSMON_FORMULAS_DIR").is_none() {
            assert_eq!(
                service.formulas_dir_for(Some(&cwd_a)),
                cosmon_a_canon.join("formulas"),
                "formulas_dir_for must walk up from supplied cwd to project A"
            );
        }
    }

    /// The tenant pin makes the `cwd` parameter inert: when an
    /// [`HttpStatePin`] is active, `state_dir_for` / `formulas_dir_for` /
    /// `config_path_for` return the pinned tenant paths **regardless** of
    /// what `cwd` the caller supplies. This is the resolver-level lock of
    /// the M2 tenant-isolation seam (the integration proof lives in
    /// `cosmon-rpp-adapter/tests/tenant_isolation_test.rs`).
    #[tokio::test]
    async fn test_active_pin_renders_cwd_inert() {
        let tmp = tempfile::tempdir().unwrap();
        let server_root = tmp.path().join("server_root");
        std::fs::create_dir_all(&server_root).unwrap();
        let service = CosmonService {
            store_dir: Arc::new(server_root.clone()),
            formulas_dir: Arc::new(server_root.join("formulas")),
            tool_router: CosmonService::tool_router(),
        };

        // A pin rooted at tenant "a"; a spoofed cwd pointing at tenant "b".
        let tenant_a = tmp.path().join("galaxies/a/.cosmon");
        let pin = HttpStatePin::new(tenant_a.join("state"), tenant_a.join("formulas"));
        let spoof_cwd = tmp.path().join("galaxies/b").to_string_lossy().into_owned();

        HTTP_STATE_PIN
            .scope(Some(pin), async move {
                // Every resolver ignores the spoofed cwd and returns the
                // pinned tenant paths.
                assert_eq!(
                    service.state_dir_for(Some(&spoof_cwd)),
                    tenant_a.join("state")
                );
                assert_eq!(
                    service.formulas_dir_for(Some(&spoof_cwd)),
                    tenant_a.join("formulas")
                );
                assert_eq!(
                    service.config_path_for(Some(&spoof_cwd)),
                    tenant_a.join("config.toml")
                );
                // Even the "absent cwd" branch is overridden by the pin.
                assert_eq!(service.state_dir_for(None), tenant_a.join("state"));
            })
            .await;
    }

    #[test]
    fn test_add_symmetric_link_mcp_distinct_targets_not_merged() {
        // Two different sources should each produce their own link —
        // idempotency is per-edge, not per-variant.
        let (_tmp, store) = make_store();
        let target = sample_mol("task-20260409-mcp3");
        store.save_molecule(&target.id, &target).unwrap();

        add_symmetric_link_mcp(
            &store,
            &target.id,
            MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260409-srcA").unwrap(),
            },
        )
        .unwrap();
        add_symmetric_link_mcp(
            &store,
            &target.id,
            MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260409-srcB").unwrap(),
            },
        )
        .unwrap();

        let reloaded = store.load_molecule(&target.id).unwrap();
        assert_eq!(reloaded.typed_links.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Remote tool-exposure partition (deny-remote set)
    // -----------------------------------------------------------------------

    /// The full local service registers every deny-remote verb — the partition
    /// is a *subtraction*, so the baseline must contain what we later remove.
    /// If a deny-remote name ever stops being a real tool, this test fails
    /// loudly rather than letting `remove_route` silently no-op.
    #[test]
    fn full_service_registers_every_deny_remote_tool() {
        let svc = CosmonService::new();
        for name in DENY_REMOTE_TOOLS {
            assert!(
                svc.tool_router.has_route(name),
                "local service must register {name} for the partition to remove it; \
                 a missing route means the deny-remote list has drifted from the tools"
            );
        }
    }

    /// The remote service must expose NONE of the deny-remote verbs. This is
    /// turing exploit #1's defense at the surface level: an absent tool cannot
    /// be a `tools/call` target.
    #[test]
    fn remote_service_denies_every_deny_remote_tool() {
        let svc = CosmonService::new_remote();
        for name in DENY_REMOTE_TOOLS {
            assert!(
                !svc.tool_router.has_route(name),
                "remote connector must NOT register {name} (deny-remote)"
            );
        }
    }

    /// The remote partition removes *exactly* the deny-remote set and nothing
    /// else: every other tool the local service exposes must survive on the
    /// remote connector. Guards against an over-broad partition silently
    /// dropping a remote-safe read/write verb.
    #[test]
    fn remote_service_keeps_every_remote_safe_tool() {
        let full = CosmonService::new();
        let remote = CosmonService::new_remote();

        let denied: std::collections::HashSet<&str> = DENY_REMOTE_TOOLS.iter().copied().collect();

        let full_names: Vec<String> = full
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert!(
            !full_names.is_empty(),
            "sanity: the full service must expose at least one tool"
        );

        for name in &full_names {
            let should_survive = !denied.contains(name.as_str());
            assert_eq!(
                remote.tool_router.has_route(name),
                should_survive,
                "tool {name}: remote presence must equal (not in deny-remote set)"
            );
        }

        // Count identity: |remote| == |full| - |deny-remote ∩ full|.
        assert_eq!(
            remote.tool_router.list_all().len(),
            full_names.len() - DENY_REMOTE_TOOLS.len(),
            "remote surface must be exactly the full surface minus the deny-remote set"
        );

        // Concrete remote-safe spot-checks — the READ/WRITE verbs a tenant
        // legitimately drives from Claude Desktop must remain reachable.
        for keep in [
            "cosmon_observe",
            "cosmon_ensemble",
            "cosmon_nucleate",
            "cosmon_wait",
            "cosmon_search",
        ] {
            assert!(
                remote.tool_router.has_route(keep),
                "remote connector must keep the remote-safe verb {keep}"
            );
        }
    }
}
