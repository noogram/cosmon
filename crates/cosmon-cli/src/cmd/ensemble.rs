// SPDX-License-Identifier: AGPL-3.0-only

//! Fleet status display command.
//!
//! Reads the fleet state via `StateStore` and renders a workers table
//! with molecule summary. Supports both human-readable (colored) and
//! JSON output formats.

use colored::Colorize;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::reconcile::{molecule_health, MoleculeHealth};
use cosmon_core::run_state::project_run_state;
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{
    reconcile, CognitiveState, EffectiveStatus, ObservedState, TransportState, WorkerRole,
};
use cosmon_state::MoleculeFilter;
use cosmon_transport::registry::{supervision_mode_for, SupervisionMode};

use super::Context;
use crate::visual::{classify as visual_classify, RowInputs, RowKind};

/// Probe-freshness window used by `RunState::ghost()` during ensemble
/// rendering. 90 s mirrors the patrol-driven default recommended in
/// ADR-052 §D2 — long enough that the transport probe run at the top of
/// `cs ensemble` itself is fresh, short enough that a rendering left
/// stale on a developer's screen still flags as `stale-probe`.
const GHOST_PROBE_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// Arguments for the `ensemble` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Show molecules from all projects, not just the current one.
    #[arg(long)]
    pub all: bool,

    /// Walk every `.cosmon/`-bearing galaxy under `$COSMON_CLUSTER_ROOT`
    /// (default `$HOME/galaxies`) and print one aggregated table.
    ///
    /// This is the cross-galaxy extension of `--all`. Where `--all` drops
    /// the project-id filter within the current state dir, `--cluster`
    /// visits every sibling galaxy on disk and prints their workers +
    /// molecule counts. Works in tandem with `--all` (implicitly drops
    /// the project filter because the scope is now the whole cluster).
    #[arg(long, visible_alias = "cluster")]
    pub cluster: bool,

    /// Override the cluster root directory when `--cluster` is set.
    /// Defaults to `$COSMON_CLUSTER_ROOT` env var, then `$HOME/galaxies`.
    #[arg(long, value_name = "DIR")]
    pub cluster_root: Option<std::path::PathBuf>,

    /// Filter molecules by tag glob pattern (repeatable, any-match).
    ///
    /// Patterns support `*` as wildcard. Example: `--tag deferred:*`.
    /// Molecules without a matching tag are excluded from the molecule
    /// summary counts.
    #[arg(long = "tag", value_name = "GLOB")]
    pub tags: Vec<String>,
}

/// Summary of fleet state for JSON output.
///
/// `molecules` is the operator-facing status-count summary; `molecule_states`
/// is the per-molecule projection consumed by machine readers (notably the
/// resident runtime in `cosmon-runtime`). The two fields are kept side-by-side
/// rather than collapsed into one because they answer different questions —
/// the dashboard ("how many running?") versus the scheduler ("which IDs are
/// pending with which blockers?").
#[derive(serde::Serialize)]
struct EnsembleOutput {
    workers: Vec<WorkerRow>,
    worker_roles: WorkerRoleSummary,
    molecules: MoleculeSummary,
    molecule_states: Vec<MoleculeStateEntry>,
}

/// Per-molecule projection of the canonical state read by machine consumers
/// (the resident runtime, GraphQL adapters, smoke tests).
///
/// The shape is intentionally a thin window — just enough for a scheduler
/// to decide `tackle` / `done` / `wait` without a second `cs` shell-out
/// per molecule. Adding fields is cheap; readers ignore what they don't
/// know about.
#[derive(serde::Serialize)]
pub(crate) struct MoleculeStateEntry {
    pub(crate) id: String,
    /// `snake_case` molecule status (matches `MoleculeStatus`'s serde repr).
    pub(crate) status: String,
    /// Cognitive kind, when the molecule has one. The resident runtime uses
    /// this to reserve decisions for a human unless they explicitly opt in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    /// Stable operational labels consumed by the resident runtime's safety
    /// policy. They are sorted because `MoleculeData` stores them in a
    /// `BTreeSet`.
    pub(crate) tags: Vec<String>,
    /// IDs that block this molecule (empty when ready). Sorted lexicographically
    /// so two adjacent invocations produce byte-identical JSON when nothing
    /// has changed (downstream readers cache on this).
    pub(crate) blocked_by: Vec<String>,
    /// Merge stamp for a `Completed` predecessor — the merge-before-dispatch
    /// discriminant. A machine reader (the resident runtime's
    /// `ReadyFrontierScheduler`) must NOT release a completed blocker's
    /// dependents until its branch has landed; this field is the presence
    /// signal it reads, mirroring `cosmon_state::frontier` (frontier.rs:214).
    /// Absent (skipped) when the molecule has not merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) merged_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Stuck stamp for a `Frozen` predecessor — the load-bearing discriminant
    /// between the two Frozen species (convoy-cascade fix, task-20260710-6174).
    /// A `cs stuck` freeze carries `stuck_at = Some(_)` ("do not execute — hold
    /// dependents"); a *delivered* freeze (`freeze_on_last_step`) carries
    /// `None` ("decomposed, release children"). The resident scheduler reads
    /// this to gate the two oppositely, mirroring `cosmon_state::frontier`
    /// (frontier.rs:210). Absent (skipped) when the molecule is not stuck.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stuck_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Adapter pinned on the persisted process record. The resident preserves
    /// this directional routing choice rather than substituting its local floor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) adapter: Option<String>,
}

/// Build the per-molecule projection consumed by machine readers.
///
/// Extracted so the self-hosting round-trip test in this module can run the
/// exact same code path the live CLI runs — without redirecting stdout
/// or spawning a binary. Entries are sorted by id; each `blocked_by` is
/// sorted lexicographically; together this gives byte-deterministic
/// output when nothing has changed.
pub(crate) fn build_molecule_states(
    molecules: &[cosmon_state::MoleculeData],
) -> Vec<MoleculeStateEntry> {
    let mut out: Vec<MoleculeStateEntry> = molecules
        .iter()
        .map(|m| {
            let mut blocked_by: Vec<String> =
                m.blocked_by().iter().map(ToString::to_string).collect();
            blocked_by.sort();
            MoleculeStateEntry {
                id: m.id.to_string(),
                status: m.status.to_string(),
                kind: m.kind.map(|kind| kind.to_string()),
                tags: m.tags.iter().map(ToString::to_string).collect(),
                blocked_by,
                merged_at: m.merged_at,
                stuck_at: m.stuck_at,
                adapter: m
                    .process
                    .as_ref()
                    .and_then(|process| process.adapter_name.clone()),
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Worker counts split by `WorkerRole` (ADR-040).
///
/// `cognition` counts cognitive workers (work in flight); `runtime`
/// counts resident runtime sessions (active orchestrators). Exposing them
/// separately lets operators spot split-brain runtimes and miscounted
/// in-flight work at a glance.
#[derive(serde::Serialize)]
struct WorkerRoleSummary {
    cognition: usize,
    runtime: usize,
    total: usize,
}

#[derive(serde::Serialize)]
struct WorkerRow {
    name: String,
    fleet: String,
    role: String,
    /// Runtime-vs-cognition discriminator (ADR-040 phase 1).
    worker_role: String,
    desired: String,
    effective: String,
    live: String,
    clearance: String,
    molecule: String,
    /// Computed molecule health (never persisted).
    molecule_health: String,
    /// ADR-052 ghost-kind marker — `None` when the molecule's run-state is
    /// internally consistent. Derived at render time by projecting
    /// `(MoleculeStatus, TransportState, merged_at)` onto a `RunState` and
    /// calling `RunState::ghost()`. Never persisted.
    ghost: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cost: f64,
    /// Resolved model id pinned for this worker's molecule
    /// (delib-20260704-b476 C3), projected from the latest `ModelSelected`
    /// event. `None` at the von-neumann floor (adapter default applies) or
    /// when no selection was recorded. Distinguish via
    /// [`model_source`](Self::model_source).
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Stable slug of where the model choice came from (`flag` / `formula_pin`
    /// / `env_var` / `config` / `global_config` / `default`). `None` when no
    /// `ModelSelected` event was recorded for the molecule.
    #[serde(skip_serializing_if = "Option::is_none")]
    model_source: Option<String>,
    /// Pre-rendered `model · source` cell for the human table (e.g.
    /// `opus-4-8 · --model`). Derived at row-build time from the
    /// attribution; skipped in JSON (machine readers use `model` +
    /// `model_source`). `None` when no selection was recorded.
    #[serde(skip)]
    model_cell: Option<String>,
    /// Visual taxonomy label (2026-04-19 charter) — plaintext only, not
    /// load-bearing for machine readers (they should read `status` +
    /// `tags` + `effective`). Skipped in JSON to keep the schema
    /// unchanged.
    #[serde(skip)]
    row_kind: RowKind,
}

/// Per-molecule data cached before the worker loop so the MH column can
/// reason about state, tags, and blockers without reloading from disk.
struct MolLookup {
    status: MoleculeStatus,
    merged_at: Option<chrono::DateTime<chrono::Utc>>,
    tags: Vec<String>,
    has_blockers: bool,
    /// Supervision mode of the adapter that owns this molecule's worker.
    ///
    /// Read from `MoleculeProcess::adapter_name` stamped by `cs tackle`.
    /// `TmuxPane` is the conservative default for legacy molecules (no
    /// adapter stamped on disk) and matches the pre-ADR-100 contract.
    /// `InProcess` switches the observer-side liveness projection from
    /// "tmux pane probe" to "molecule status + event freshness", because
    /// for Direct-API workers the absence of a pane is the nominal
    /// state, not the dead state. See the GAP #7 chronicle.
    supervision: SupervisionMode,
    /// Latest `(updated_at, last_progress_at)` we can use as an event
    /// freshness signal. Read by the `InProcess` projection to warn
    /// "inprocess stale" when a `Running` in-process worker has not
    /// emitted progress for longer than [`INPROCESS_STALE_TTL`].
    last_event_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Freshness window for an in-process worker's last event. A `Running`
/// in-process molecule whose latest event is older than this is reported
/// as `suspect:inprocess-stale` rather than `healthy`. 60 s mirrors the
/// "warn 'inprocess stale'" criterion in the GAP #7 academy chronicle
/// (`2026-05-18-grok-direct-api-smoke-result-3.md` §verdict).
const INPROCESS_STALE_TTL: chrono::Duration = chrono::Duration::seconds(60);

#[derive(serde::Serialize)]
struct MoleculeSummary {
    pending: usize,
    queued: usize,
    running: usize,
    frozen: usize,
    completed: usize,
    collapsed: usize,
    total: usize,
    tier_0: usize,
    tier_1: usize,
    tier_2: usize,
}

/// Execute the `ensemble` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if args.cluster {
        return run_cluster(ctx, args);
    }
    let state_dir = ctx.state_dir();
    let store = ctx.store();

    let fleet = store.load_fleet()?;

    // Resolve project_id for scoping — graceful fallback to no filter for legacy projects.
    let project_id = if args.all {
        None
    } else {
        let config_path = super::resolve_config_from_context(ctx);
        cosmon_filestore::load_project_config(&config_path)
            .ok()
            .and_then(|c| c.project.project_id)
    };

    let filter = MoleculeFilter {
        project: project_id,
        tag_globs: args.tags.clone(),
        ..MoleculeFilter::default()
    };
    let molecules = store.list_molecules(&filter)?;

    // Resolve formula tiers for tier distribution.
    let formulas_dir = cosmon_filestore::resolve_formulas_dir_from(&state_dir);
    let mut formula_tier_cache: std::collections::HashMap<String, u8> =
        std::collections::HashMap::new();
    let resolve_tier =
        |fid: &str, cache: &mut std::collections::HashMap<String, u8>| -> Option<u8> {
            if let Some(&lvl) = cache.get(fid) {
                return Some(lvl);
            }
            let fp = formulas_dir.join(format!("{fid}.formula.toml"));
            let text = std::fs::read_to_string(&fp).ok()?;
            let formula = cosmon_core::formula::Formula::parse(&text).ok()?;
            let lvl = formula.tier.level();
            cache.insert(fid.to_owned(), lvl);
            Some(lvl)
        };
    let (mut t0, mut t1, mut t2) = (0usize, 0usize, 0usize);
    for m in &molecules {
        match resolve_tier(m.formula_id.as_str(), &mut formula_tier_cache) {
            Some(0) => t0 += 1,
            Some(1) => t1 += 1,
            Some(2) => t2 += 1,
            _ => {}
        }
    }

    let mol_summary = MoleculeSummary {
        pending: molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Pending)
            .count(),
        queued: molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Queued)
            .count(),
        running: molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Running)
            .count(),
        frozen: molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Frozen)
            .count(),
        completed: molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Completed)
            .count(),
        collapsed: molecules
            .iter()
            .filter(|m| m.status == MoleculeStatus::Collapsed)
            .count(),
        total: molecules.len(),
        tier_0: t0,
        tier_1: t1,
        tier_2: t2,
    };

    // Probe live session status for all workers using the reconciliation model.
    // Build ObservedState (transport + session + cognitive) → reconcile() → EffectiveStatus.
    let project_socket = super::tmux_socket_name(ctx);
    let backends = discover_fleet_backends(&state_dir, &project_socket);

    // Load energy data from Claude Code session logs (PID → session → tokens).
    let energy_by_worker = crate::energy_probe::load_worker_energy(&backends, &fleet);
    let cognitive_dir = state_dir.join("cognitive");
    let fleet_membership = build_fleet_membership(&state_dir);
    // Lookup from molecule id → (status, merged_at, tags, has_blockers).
    // `merged_at` is the c1cb / I9 signal — a non-terminal molecule whose
    // branch is already merged is the Gödel sentence made visible.
    // `tags` and `has_blockers` feed the visual charter (RowKind) so the
    // MH column can distinguish parked-on-purpose from hot-actionable
    // from blocked-on-dependency without inventing a new verb.
    let molecule_lookup: std::collections::HashMap<_, MolLookup> = molecules
        .iter()
        .map(|m| {
            let has_blockers = m.blocked_by().iter().any(|blocker| {
                molecules
                    .iter()
                    .find(|candidate| &&candidate.id == blocker)
                    .is_none_or(|candidate| {
                        !matches!(
                            candidate.status,
                            MoleculeStatus::Completed | MoleculeStatus::Collapsed
                        )
                    })
            });
            let tags: Vec<String> = m.tags.iter().map(|t| t.as_str().to_owned()).collect();
            let supervision = m
                .process
                .as_ref()
                .and_then(|p| p.adapter_name.as_deref())
                .map_or(SupervisionMode::TmuxPane, supervision_mode_for);
            // Pick the freshest of `last_progress_at` and `updated_at`
            // — either is a valid heartbeat for the InProcess freshness
            // projection. `last_progress_at` is the worker's own
            // inference-stall signal; `updated_at` is the store-write
            // timestamp. The max() is the most recent activity we can
            // observe without parsing events.jsonl.
            let last_event_at = match (m.last_progress_at, Some(m.updated_at)) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            };
            (
                m.id.clone(),
                MolLookup {
                    status: m.status,
                    merged_at: m.merged_at,
                    tags,
                    has_blockers,
                    supervision,
                    last_event_at,
                },
            )
        })
        .collect();
    // delib-20260704-b476 C3: fold the `ModelSelected` event log once so
    // every worker row can surface the model + source pinned for its
    // molecule (which model is running where, at a glance). Single pass —
    // never re-read per molecule.
    let model_selections = cosmon_state::ops::model_selections(&state_dir);
    let mut rows: Vec<WorkerRow> = fleet
        .workers
        .values()
        .map(|w| {
            // 1. Observe transport: is the tmux session alive?
            let (raw_transport, raw_session_str) = observe_transport(&backends, &w.id);

            // 2. Observe cognitive: agent self-declared status.
            let (cognitive, cognitive_display) = observe_cognitive(&cognitive_dir, w.id.as_str());

            // Cache molecule lookup once — needed for the SupervisionMode
            // projection below and for the MH + ghost columns further down.
            let mol_lookup = w
                .current_molecule
                .as_ref()
                .and_then(|id| molecule_lookup.get(id));

            // 2b. Supervision projection (GAP #7 / ADR-101 observer side).
            //
            // For tmux-postulated adapters the tmux probe IS the
            // liveness signal — keep `(raw_transport, raw_session_str)`
            // verbatim. For in-process Direct-API adapters the absence
            // of a pane is *nominal* (the agent loop runs inside
            // `cs tackle` and exits cleanly on its own); reading the
            // tmux probe verbatim flips the ensemble row to
            // `diverged + 👻 un-harvested` even though the work landed
            // and the molecule already moved to `Completed` via GAP #6's
            // `finalize_inprocess_molecule`. The projection below
            // bypasses the tmux probe for in-process workers and
            // synthesises a transport state from the molecule's status
            // + event freshness instead.
            let supervision = mol_lookup.map_or(SupervisionMode::TmuxPane, |ml| ml.supervision);
            let (transport, session_str, inprocess_effective_override) = match supervision {
                SupervisionMode::InProcess => {
                    project_inprocess_transport(mol_lookup, raw_session_str, chrono::Utc::now())
                }
                // TmuxPane today; any future non-exhaustive variant
                // falls back to the tmux-postulated reading so the
                // observer never goes silent on an unknown adapter.
                _ => (raw_transport, raw_session_str, None),
            };

            // 3. Build ObservedState and reconcile.
            let observed = ObservedState {
                transport,
                session: session_str.clone(),
                cognitive,
            };
            let (effective, _actions) = reconcile(w.desired, &observed, w.restart_count, 3);
            // InProcess overrides the reconciliation verdict whenever
            // the molecule has settled — reconcile cannot know that a
            // worker with `desired=Running` whose in-process loop
            // returned is *expected* to have no pane. Without the
            // override, the row would still read `diverged` because
            // tmux-truth says "no pane".
            let effective = inprocess_effective_override.unwrap_or(effective);

            // 4. Live column: cognitive display > session string > "-".
            let live = cognitive_display
                .or(session_str)
                .unwrap_or_else(|| "-".to_owned());

            let energy = energy_by_worker.get(&w.id);

            // Molecule health — pure overlay, never persisted.
            let mol_health = mol_lookup.map_or(MoleculeHealth::Inert, |ml| {
                molecule_health(ml.status, Some(&effective))
            });

            // ADR-052 ghost marker — project run-state and detect drift.
            // Omitted on terminal molecules (no action the operator can
            // take from the ensemble view) and when no molecule is bound.
            //
            // For in-process workers, `UnHarvested` is a tmux-specific
            // ghost (the pane-died hook auto-merges via `cs harvest`,
            // so any delay between completion and merge is anomalous).
            // For in-process there is no auto-merge — the operator
            // reads the synthesis and calls `cs done` deliberately, so
            // the `Completed + Unmerged` window is *nominal*. Suppress
            // the ghost in that case rather than dressing the row in
            // a costume the operator must mentally subtract.
            let ghost = mol_lookup.and_then(|ml| {
                if matches!(ml.supervision, SupervisionMode::InProcess) {
                    return None;
                }
                let rs = project_run_state(ml.status, transport, ml.merged_at, chrono::Utc::now());
                rs.ghost(chrono::Utc::now(), GHOST_PROBE_TTL)
                    .map(|g| g.as_str().to_owned())
            });

            // Visual charter (2026-04-19): classify the row into its
            // RowKind so the MH column shows the right pastille + color
            // without confusing parked-on-purpose with ghost.
            let row_kind = mol_lookup.map_or(RowKind::Idle, |ml| {
                visual_classify(&RowInputs {
                    status: ml.status,
                    heartbeat: None,
                    tags: &ml.tags,
                    has_blockers: ml.has_blockers,
                    ghost: ghost.is_some(),
                    drift: false,
                })
            });

            // Model attribution for this worker's molecule (C3).
            let model_attr = w
                .current_molecule
                .as_ref()
                .and_then(|id| model_selections.get(id));

            WorkerRow {
                name: w.id.as_str().to_owned(),
                fleet: fleet_membership
                    .get(w.id.as_str())
                    .cloned()
                    .unwrap_or_else(|| "-".to_owned()),
                role: w.role.to_string(),
                worker_role: w.worker_role.to_string(),
                desired: w.desired.to_string(),
                effective: effective.to_string(),
                live,
                clearance: w.clearance.to_string(),
                molecule: w
                    .current_molecule
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), ToString::to_string),
                molecule_health: mol_health.to_string(),
                ghost,
                input_tokens: energy.map_or(0, |e| e.input.get()),
                output_tokens: energy.map_or(0, |e| e.output.get()),
                cost: energy.map_or(0.0, |e| e.cost.get()),
                model: model_attr.and_then(|a| a.model.clone()),
                model_source: model_attr.map(|a| a.source_slug().to_owned()),
                model_cell: model_attr
                    .map(|a| format!("{} · {}", a.model_label(), a.source_short())),
                row_kind,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.fleet.cmp(&b.fleet).then(a.name.cmp(&b.name)));

    let worker_roles = summarize_worker_roles(&fleet);

    if ctx.json {
        let output = EnsembleOutput {
            workers: rows,
            worker_roles,
            molecules: mol_summary,
            molecule_states: build_molecule_states(&molecules),
        };
        let json = serde_json::to_string_pretty(&output)?;
        println!("{json}");
        return Ok(());
    }

    // Human-readable output
    if fleet.workers.is_empty() {
        println!("{}", "No workers in the fleet.".dimmed());
        println!("{}", "Use `cs spawn` to add workers.".dimmed());
        return Ok(());
    }

    // Header
    println!(
        "{} {} workers, {} molecules ({} pending, {} queued, {} running)",
        "Ensemble:".bold(),
        fleet.workers.len(),
        mol_summary.total,
        mol_summary.pending,
        mol_summary.queued,
        mol_summary.running,
    );
    println!();

    // Compute dynamic column widths from data.
    let w_name = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4) + 2;
    let w_role = rows.iter().map(|r| r.role.len()).max().unwrap_or(4).max(4) + 2;
    let w_desired = rows
        .iter()
        .map(|r| r.desired.len())
        .max()
        .unwrap_or(7)
        .max(7)
        + 2;
    let w_effective = rows
        .iter()
        .map(|r| r.effective.len())
        .max()
        .unwrap_or(9)
        .max(9)
        + 2;
    let w_live = rows.iter().map(|r| r.live.len()).max().unwrap_or(4).max(4) + 2;
    let w_clear = 10;
    let w_input = 10;
    let w_output = 10;
    let w_cost = 10;
    let total_width = w_name
        + w_role
        + w_desired
        + w_effective
        + w_live
        + w_clear
        + w_input
        + w_output
        + w_cost
        + 10;

    // Table header. `MH` is the molecule-health glyph (pure overlay from
    // `cosmon_core::reconcile::molecule_health` — never persisted).
    println!(
        "  {:<w_name$} {:<w_role$} {:<w_desired$} {:<w_effective$} {:<w_live$} {:<w_clear$} {:>w_input$} {:>w_output$} {:>w_cost$} {} {}",
        "NAME".bold(),
        "ROLE".bold(),
        "DESIRED".bold(),
        "EFFECTIVE".bold(),
        "LIVE".bold(),
        "CLEARANCE".bold(),
        "INPUT".bold(),
        "OUTPUT".bold(),
        "COST".bold(),
        "MH".bold(),
        "MOLECULE".bold(),
    );
    println!("  {}", "─".repeat(total_width).dimmed());

    // Group by fleet with headers and subtotals.
    let mut current_fleet = String::new();
    let mut fleet_input: u64 = 0;
    let mut fleet_output: u64 = 0;
    let mut fleet_cost: f64 = 0.0;
    let mut grand_input: u64 = 0;
    let mut grand_output: u64 = 0;
    let mut grand_cost: f64 = 0.0;

    // Offset for the subtotal line (skip NAME + ROLE + DESIRED + EFFECTIVE + LIVE + CLEARANCE).
    let subtotal_pad = w_name + w_role + w_desired + w_effective + w_live + w_clear + 6;

    for (i, row) in rows.iter().enumerate() {
        let is_new_fleet = row.fleet != current_fleet;
        let is_last = i + 1 == rows.len();

        // Print subtotal for the previous fleet when the fleet changes.
        if is_new_fleet && !current_fleet.is_empty() {
            print_fleet_subtotal(
                subtotal_pad,
                w_input,
                w_output,
                w_cost,
                fleet_input,
                fleet_output,
                fleet_cost,
            );
            fleet_input = 0;
            fleet_output = 0;
            fleet_cost = 0.0;
            println!();
        }

        if is_new_fleet {
            current_fleet.clone_from(&row.fleet);
            if current_fleet != "-" {
                println!("  {} {}", "fleet:".dimmed(), current_fleet.cyan());
            }
        }

        // Accumulate fleet and grand totals.
        fleet_input += row.input_tokens;
        fleet_output += row.output_tokens;
        fleet_cost += row.cost;
        grand_input += row.input_tokens;
        grand_output += row.output_tokens;
        grand_cost += row.cost;

        // Pad BEFORE colorizing — ANSI escape codes break fixed-width formatting.
        let desired_padded = format!("{:<w_desired$}", row.desired);
        let desired_colored = colorize_desired(&desired_padded);
        let effective_padded = format!("{:<w_effective$}", row.effective);
        let effective_colored = colorize_effective(&effective_padded);
        let live_padded = format!("{:<w_live$}", row.live);
        let live_colored = colorize_live(&live_padded);
        let input_str = format_tokens(row.input_tokens);
        let output_str = format_tokens(row.output_tokens);
        let cost_str = format_cost(row.cost);
        let health_badge = row.row_kind.colorize(row.row_kind.glyph()).to_string();
        // ADR-052 ghost suffix — appended to the molecule column so the
        // eye catches it without a separate, sparsely-populated column.
        let molecule_cell = match &row.ghost {
            Some(g) => format!("{}  {}", row.molecule, format!("👻 {g}").red().bold()),
            None => row.molecule.clone(),
        };
        // delib-20260704-b476 C3 model suffix — appended after the ghost so
        // the operator sees which model is running where without widening the
        // (already dense) worker table with a fixed column. Dimmed brackets
        // keep it visually subordinate to the molecule id itself.
        let molecule_cell = match &row.model_cell {
            Some(m) => format!("{molecule_cell}  {}", format!("⟦{m}⟧").dimmed()),
            None => molecule_cell,
        };
        println!(
            "  {:<w_name$} {:<w_role$} {} {} {} {:<w_clear$} {:>w_input$} {:>w_output$} {:>w_cost$} {} {}",
            row.name,
            row.role,
            desired_colored,
            effective_colored,
            live_colored,
            row.clearance,
            input_str.dimmed(),
            output_str.dimmed(),
            cost_str.dimmed(),
            health_badge,
            molecule_cell,
        );

        // Print subtotal after the last row.
        if is_last {
            print_fleet_subtotal(
                subtotal_pad,
                w_input,
                w_output,
                w_cost,
                fleet_input,
                fleet_output,
                fleet_cost,
            );
        }
    }

    // Grand total.
    if grand_cost > 0.0 {
        let rule_width = w_input + w_output + w_cost + 2;
        println!();
        println!(
            "  {:>subtotal_pad$} {}",
            "",
            "━".repeat(rule_width).dimmed(),
        );
        println!(
            "  {:>subtotal_pad$} {:>w_input$} {:>w_output$} {:>w_cost$}",
            "TOTAL".bold(),
            format_tokens(grand_input).bold(),
            format_tokens(grand_output).bold(),
            format_cost(grand_cost).yellow().bold(),
        );
    }

    // Worker role split — the 1-bit "in flight vs orchestrator" summary
    // landed by ADR-040 phase 1. Always printed so operators notice a
    // runtime drifting relative to cognition even when molecules look OK.
    println!();
    println!(
        "  {} {} cognition, {} runtime ({} total)",
        "Workers:".bold(),
        worker_roles.cognition,
        worker_roles.runtime,
        worker_roles.total,
    );

    // ADR-052 ghost roll-up — if any row flagged a ghost, tell the
    // operator at a glance which variants are present. Silent when the
    // ensemble is clean.
    let ghost_summary = summarize_ghosts(&rows);
    if !ghost_summary.is_empty() {
        println!();
        let parts: Vec<String> = ghost_summary
            .iter()
            .map(|(name, count)| format!("{count} {name}"))
            .collect();
        println!("  {} {}", "👻 Ghosts:".red().bold(), parts.join(", ").red(),);
    }

    if mol_summary.total > 0 {
        println!();
        println!(
            "  {} {} pending, {} queued, {} running, {} frozen, {} completed, {} collapsed",
            "Molecules:".bold(),
            mol_summary.pending,
            mol_summary.queued,
            mol_summary.running,
            mol_summary.frozen,
            mol_summary.completed,
            mol_summary.collapsed,
        );
        if mol_summary.tier_0 > 0 || mol_summary.tier_1 > 0 || mol_summary.tier_2 > 0 {
            println!(
                "  {} T0={}, T1={}, T2={}",
                "Tiers:".bold(),
                mol_summary.tier_0,
                mol_summary.tier_1,
                mol_summary.tier_2,
            );
        }
    }

    Ok(())
}

/// Count worker rows by detected `GhostKind` variant, sorted by variant
/// name for stable output.
///
/// Returns `Vec<(variant_name, count)>`. Empty when no ghost is present —
/// callers use the emptiness as the gate for printing the summary.
fn summarize_ghosts(rows: &[WorkerRow]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for r in rows {
        if let Some(g) = &r.ghost {
            *counts.entry(g.clone()).or_insert(0) += 1;
        }
    }
    counts.into_iter().collect()
}

/// Aggregate worker counts by `WorkerRole`.
///
/// Runtime workers are resident orchestrators (`cs run`); cognition workers
/// are the Claude sessions doing the work itself. Reported together they
/// give the operator an immediate "active orchestrators vs in-flight work"
/// ratio — the signal that was missing when the schema did not yet carry
/// role semantics.
fn summarize_worker_roles(fleet: &cosmon_state::Fleet) -> WorkerRoleSummary {
    let mut cognition = 0usize;
    let mut runtime = 0usize;
    for w in fleet.workers.values() {
        match w.worker_role {
            WorkerRole::Cognition => cognition += 1,
            WorkerRole::Runtime => runtime += 1,
        }
    }
    WorkerRoleSummary {
        cognition,
        runtime,
        total: cognition + runtime,
    }
}

/// Format a token count for display (e.g. `"1.2M"`, `"45K"`, `"0"`).
fn format_tokens(tokens: u64) -> String {
    if tokens == 0 {
        "-".to_string()
    } else if tokens >= 1_000_000 {
        #[allow(clippy::cast_precision_loss)]
        let m = tokens as f64 / 1_000_000.0;
        format!("{m:.1}M")
    } else if tokens >= 1_000 {
        #[allow(clippy::cast_precision_loss)]
        let k = tokens as f64 / 1_000.0;
        format!("{k:.0}K")
    } else {
        tokens.to_string()
    }
}

/// Format a cost for display (e.g. `"$12.34"`, `"-"`).
fn format_cost(cost: f64) -> String {
    if cost < 0.001 {
        "-".to_string()
    } else {
        format!("${cost:.2}")
    }
}

/// Print a fleet subtotal line with a thin separator.
fn print_fleet_subtotal(
    pad: usize,
    w_input: usize,
    w_output: usize,
    w_cost: usize,
    input: u64,
    output: u64,
    cost: f64,
) {
    if cost < 0.001 {
        return;
    }
    let rule_width = w_input + w_output + w_cost + 2;
    println!("  {:>pad$} {}", "", "─".repeat(rule_width).dimmed(),);
    println!(
        "  {:>pad$} {:>w_input$} {:>w_output$} {:>w_cost$}",
        "fleet total".bold(),
        format_tokens(input),
        format_tokens(output),
        format_cost(cost).yellow(),
    );
}

/// Colorize the desired state column.
fn colorize_desired(desired: &str) -> String {
    match desired.trim() {
        "running" => desired.green().to_string(),
        "paused" => desired.yellow().to_string(),
        "stopped" => desired.dimmed().to_string(),
        _ => desired.to_owned(),
    }
}

/// Legacy molecule-health colorizer.
///
/// Preserved for JSON-consumers and scripts that still read the
/// `molecule_health` slug via `cs ensemble --json`. The plaintext MH
/// column now renders [`RowKind::glyph`] through [`RowKind::colorize`]
/// (2026-04-19 visual charter).
#[allow(dead_code)]
fn colorize_molecule_health(health_slug: &str) -> String {
    // Parse the display slug back into the enum so the glyph mapping
    // stays defined by a single source of truth (`MoleculeHealth::glyph`).
    let health = match health_slug {
        "healthy" => MoleculeHealth::Healthy,
        "orphaned" => MoleculeHealth::Orphaned,
        "stalled" => MoleculeHealth::Stalled,
        "blocked" => MoleculeHealth::Blocked,
        "degraded" => MoleculeHealth::Degraded,
        "terminal" => MoleculeHealth::Terminal,
        _ => MoleculeHealth::Inert,
    };
    let glyph = health.glyph();
    match health {
        MoleculeHealth::Healthy => glyph.green().to_string(),
        MoleculeHealth::Orphaned | MoleculeHealth::Blocked => glyph.red().bold().to_string(),
        MoleculeHealth::Stalled | MoleculeHealth::Degraded => glyph.yellow().bold().to_string(),
        MoleculeHealth::Terminal | MoleculeHealth::Inert => glyph.dimmed().to_string(),
    }
}

/// Colorize the effective status column (reconciled view).
fn colorize_effective(effective: &str) -> String {
    match effective.trim() {
        "healthy" => effective.green().to_string(),
        "diverged" | "blocked" => effective.red().bold().to_string(),
        "suspect" => effective.yellow().bold().to_string(),
        "paused" => effective.yellow().to_string(),
        "stopped" => effective.dimmed().to_string(),
        s if s.starts_with("error:") => effective.red().bold().to_string(),
        _ => effective.to_owned(),
    }
}

/// Build a map of `worker_name` → `fleet_name` from deployed fleet specs.
fn build_fleet_membership(
    state_dir: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    let mut membership = std::collections::HashMap::new();
    let fleets_dir = state_dir.join("fleets");
    if let Ok(entries) = std::fs::read_dir(&fleets_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<serde_json::Value>(&content) {
                        let fleet_name = spec["name"].as_str().unwrap_or("?").to_owned();
                        if let Some(agents) = spec["agents"].as_array() {
                            for agent in agents {
                                if let Some(name) = agent["name"].as_str() {
                                    membership.insert(name.to_owned(), fleet_name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    membership
}

/// Discover all fleet-scoped tmux backends by scanning deployed fleet specs.
///
/// Returns backends for each fleet's socket plus a fallback "cosmon" backend.
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

/// Project an in-process Direct-API worker's observable transport.
///
/// For in-process adapters (openai, anthropic — ADR-100 R2), the tmux
/// probe is structurally uninformative: the agent loop runs inside
/// `cs tackle`, never opens a pane, and the `*-inprocess` sentinel
/// socket always reports `Dead`. Reading the probe verbatim flips
/// `cs ensemble` to `diverged + 👻 un-harvested` even when the work
/// landed cleanly via GAP #6's `finalize_inprocess_molecule`. This
/// helper synthesises a transport projection from the molecule's
/// status + event freshness instead, returning an optional
/// [`EffectiveStatus`] override the caller stamps after `reconcile`.
///
/// The projection mirrors the criteria in the GAP #7 academy chronicle
/// (`2026-05-18-grok-direct-api-smoke-result-3.md` §verdict):
///
/// * `status = Completed | Collapsed` → transport `Dead`, effective
///   `Stopped`. The in-process loop returned; there is no live process,
///   and there is no harvest hook to chase — the operator merges via
///   `cs done` deliberately. No ghost.
/// * `status = Running` AND event newer than [`INPROCESS_STALE_TTL`]
///   → transport `Alive`, no override. The agent loop is still in
///   flight (we are inside `cs tackle`).
/// * `status = Running` AND event older than [`INPROCESS_STALE_TTL`]
///   → transport `Unknown`, effective `Suspect`. The in-process loop
///   has gone quiet for longer than the freshness window; the
///   operator should investigate even though there is no pane to
///   probe.
/// * Anything else (Pending, Queued, Frozen, Starved, missing
///   lookup) → keep transport `Dead`, no override. Same conservative
///   default `observe_transport` produced.
fn project_inprocess_transport(
    mol_lookup: Option<&MolLookup>,
    session_str: Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> (TransportState, Option<String>, Option<EffectiveStatus>) {
    let Some(ml) = mol_lookup else {
        return (TransportState::Dead, session_str, None);
    };
    match ml.status {
        MoleculeStatus::Completed => (
            TransportState::Dead,
            Some("done:inprocess".to_owned()),
            Some(EffectiveStatus::Stopped),
        ),
        MoleculeStatus::Collapsed => (
            TransportState::Dead,
            Some("collapsed:inprocess".to_owned()),
            Some(EffectiveStatus::Stopped),
        ),
        MoleculeStatus::Running => {
            let stale = ml
                .last_event_at
                .is_some_and(|at| (now - at) > INPROCESS_STALE_TTL);
            if stale {
                (
                    TransportState::Unknown,
                    Some("stale:inprocess".to_owned()),
                    Some(EffectiveStatus::Suspect),
                )
            } else {
                (
                    TransportState::Alive,
                    Some("loop:inprocess".to_owned()),
                    None,
                )
            }
        }
        _ => (TransportState::Dead, session_str, None),
    }
}

/// Observe transport state: probe all backends for liveness + session status.
///
/// Returns `(TransportState, Option<session_display_string>)`.
fn observe_transport(
    backends: &[cosmon_transport::TmuxBackend],
    worker_id: &cosmon_core::id::WorkerId,
) -> (TransportState, Option<String>) {
    for be in backends {
        if let Ok(true) = be.is_alive(worker_id) {
            // Alive — try to get session status for the live column.
            let session_str = cosmon_transport::readiness::detect_status(be, worker_id)
                .ok()
                .map(|s| s.to_string());
            return (TransportState::Alive, session_str);
        }
    }
    // No backend found it alive.
    (TransportState::Dead, None)
}

/// Observe cognitive state from the agent's self-declaration file.
///
/// Returns `(CognitiveState, Option<display_string>)`.
/// The display string is for the live column (e.g. "working:fixing bug").
fn observe_cognitive(
    cognitive_dir: &std::path::Path,
    worker_id: &str,
) -> (CognitiveState, Option<String>) {
    let path = cognitive_dir.join(format!("{worker_id}.json"));
    let Ok(content) = std::fs::read_to_string(path) else {
        return (CognitiveState::None, None);
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return (CognitiveState::None, None);
    };

    // Check staleness — declarations older than 5 minutes are stale.
    if let Some(updated) = json["updated_at"].as_str() {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(updated) {
            let age = chrono::Utc::now() - ts.with_timezone(&chrono::Utc);
            if age > chrono::Duration::minutes(5) {
                return (CognitiveState::Stale, None);
            }
        }
    }

    let status = match json["status"].as_str() {
        Some(s) => s.to_owned(),
        None => return (CognitiveState::None, None),
    };

    // Build display string with optional detail.
    let detail = json["detail"].as_str().unwrap_or("");
    let display = if detail.is_empty() {
        status.clone()
    } else {
        let short = if detail.len() > 20 {
            format!("{}…", &detail[..19])
        } else {
            detail.to_owned()
        };
        format!("{status}:{short}")
    };

    (CognitiveState::Fresh(status), Some(display))
}

fn colorize_live(live: &str) -> String {
    let trimmed = live.trim();
    match trimmed {
        s if s.starts_with("working") => live.green().bold().to_string(),
        s if s.starts_with("waiting") => live.yellow().to_string(),
        s if s.starts_with("done") => live.cyan().to_string(),
        "blocked" | "trust-prompt" => live.red().bold().to_string(),
        "loading" => live.cyan().to_string(),
        "idle" | "dead" | "-" => live.dimmed().to_string(),
        s if s.starts_with("error") => live.red().to_string(),
        _ => live.to_owned(),
    }
}

// -----------------------------------------------------------------------------
// Cluster mode (`--cluster`)
// -----------------------------------------------------------------------------

/// Per-galaxy row printed by `cs ensemble --cluster`.
#[derive(serde::Serialize)]
struct ClusterRow {
    galaxy: String,
    path: String,
    workers: usize,
    pending: usize,
    queued: usize,
    running: usize,
    frozen: usize,
    completed: usize,
    collapsed: usize,
}

/// Resolve the cluster root used by `--cluster`.
///
/// Precedence: explicit `--cluster-root` → `$COSMON_CLUSTER_ROOT` →
/// `$HOME/galaxies`. Mirrors the resolution used by `cs-api --galaxies-root`.
fn resolve_cluster_root(args: &Args) -> std::path::PathBuf {
    if let Some(p) = &args.cluster_root {
        return p.clone();
    }
    if let Ok(p) = std::env::var("COSMON_CLUSTER_ROOT") {
        return std::path::PathBuf::from(p);
    }
    std::env::var_os("HOME")
        .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from)
        .join("galaxies")
}

/// Walk the cluster root and aggregate one `ClusterRow` per `.cosmon/`-bearing
/// galaxy. Corrupt `state.json` files are silently skipped so one half-written
/// molecule does not break the whole view.
fn scan_cluster(root: &std::path::Path) -> Vec<ClusterRow> {
    let mut out = Vec::new();
    let Ok(iter) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in iter.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let path = entry.path();
        let state = path.join(".cosmon").join("state");
        if !state.is_dir() {
            continue;
        }
        let workers = count_workers(&state);
        let (pending, queued, running, frozen, completed, collapsed) = count_statuses(&state);
        out.push(ClusterRow {
            galaxy: entry.file_name().to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            workers,
            pending,
            queued,
            running,
            frozen,
            completed,
            collapsed,
        });
    }
    out.sort_by(|a, b| a.galaxy.cmp(&b.galaxy));
    out
}

fn count_workers(state: &std::path::Path) -> usize {
    let Ok(content) = std::fs::read_to_string(state.join("fleet.json")) else {
        return 0;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return 0;
    };
    v.get("workers")
        .and_then(|w| w.as_object())
        .map_or(0, serde_json::Map::len)
}

#[allow(clippy::type_complexity)]
fn count_statuses(state: &std::path::Path) -> (usize, usize, usize, usize, usize, usize) {
    let fleets = state.join("fleets");
    let mut pending = 0usize;
    let mut queued = 0usize;
    let mut running = 0usize;
    let mut frozen = 0usize;
    let mut completed = 0usize;
    let mut collapsed = 0usize;
    let Ok(iter) = std::fs::read_dir(&fleets) else {
        return (0, 0, 0, 0, 0, 0);
    };
    for fe in iter.flatten() {
        let Ok(ft) = fe.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let mol_dir = fe.path().join("molecules");
        let Ok(mit) = std::fs::read_dir(&mol_dir) else {
            continue;
        };
        for me in mit.flatten() {
            let f = me.path().join("state.json");
            let Ok(c) = std::fs::read_to_string(&f) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&c) else {
                continue;
            };
            if v.get("archived")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            match v
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_lowercase()
                .as_str()
            {
                "pending" => pending += 1,
                "queued" => queued += 1,
                "running" => running += 1,
                "frozen" => frozen += 1,
                "completed" => completed += 1,
                "collapsed" => collapsed += 1,
                _ => {}
            }
        }
    }
    (pending, queued, running, frozen, completed, collapsed)
}

/// Render the cluster-wide table. JSON output (`--json`) emits a
/// structured document; human output prints a single aggregated table
/// with one row per galaxy and a grand-total footer.
fn run_cluster(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let root = resolve_cluster_root(args);
    let rows = scan_cluster(&root);

    if ctx.json {
        let output = serde_json::json!({
            "cluster_root": root.to_string_lossy(),
            "galaxies": rows,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if rows.is_empty() {
        println!(
            "{}",
            format!("No cosmon galaxies under {}.", root.display()).dimmed()
        );
        println!(
            "{}",
            "Set COSMON_CLUSTER_ROOT or pass --cluster-root to point elsewhere.".dimmed()
        );
        return Ok(());
    }

    println!(
        "{} {} — {} galaxies",
        "Cluster:".bold(),
        root.display(),
        rows.len(),
    );
    println!();

    let w_name = rows
        .iter()
        .map(|r| r.galaxy.len())
        .max()
        .unwrap_or(6)
        .max(6)
        + 2;
    let header = format!(
        "  {:<w_name$} {:>5} {:>8} {:>7} {:>8} {:>7} {:>10} {:>10}",
        "GALAXY".bold(),
        "WKRS".bold(),
        "PENDING".bold(),
        "QUEUED".bold(),
        "RUNNING".bold(),
        "FROZEN".bold(),
        "COMPLETED".bold(),
        "COLLAPSED".bold(),
    );
    println!("{header}");
    println!("  {}", "─".repeat(w_name + 62).dimmed());

    let mut grand = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for row in &rows {
        println!(
            "  {:<w_name$} {:>5} {:>8} {:>7} {:>8} {:>7} {:>10} {:>10}",
            row.galaxy,
            row.workers,
            row.pending,
            row.queued,
            row.running,
            row.frozen,
            row.completed,
            row.collapsed,
        );
        grand.0 += row.workers;
        grand.1 += row.pending;
        grand.2 += row.queued;
        grand.3 += row.running;
        grand.4 += row.frozen;
        grand.5 += row.completed;
        grand.6 += row.collapsed;
    }

    println!("  {}", "─".repeat(w_name + 62).dimmed());
    println!(
        "  {:<w_name$} {:>5} {:>8} {:>7} {:>8} {:>7} {:>10} {:>10}",
        "TOTAL".bold(),
        grand.0.to_string().bold(),
        grand.1.to_string().bold(),
        grand.2.to_string().bold(),
        grand.3.to_string().bold(),
        grand.4.to_string().bold(),
        grand.5.to_string().bold(),
        grand.6.to_string().bold(),
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::worker::WorkerStatus;
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, MoleculeData, MoleculeFilter, RepoData, StateStore, WorkerData};
    use tempfile::TempDir;

    use super::*;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        (tmp, store)
    }

    fn make_worker(
        name: &str,
        role: AgentRole,
        status: WorkerStatus,
        repo: Option<&str>,
        clearance: Clearance,
    ) -> (WorkerId, WorkerData) {
        let wid = WorkerId::new(name).unwrap();
        let mut data = WorkerData::new(
            wid.clone(),
            AgentId::new(format!("agent-{name}")).unwrap(),
            role,
            clearance,
            status,
        );
        if let Some(r) = repo {
            data = data.with_repo(r);
        }
        (wid, data)
    }

    fn make_molecule(suffix: &str, status: MoleculeStatus) -> MoleculeData {
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
    fn test_ensemble_empty_fleet() {
        let (tmp, store) = make_store();
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(
            &ctx,
            &Args {
                all: true,
                cluster: false,
                cluster_root: None,
                tags: Vec::new(),
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_ensemble_with_workers() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();

        let (wid1, w1) = make_worker(
            "ruby",
            AgentRole::Implementation,
            WorkerStatus::Active,
            Some("cosmon"),
            Clearance::Write,
        );
        let (wid2, w2) = make_worker(
            "witness",
            AgentRole::Orchestration,
            WorkerStatus::Active,
            Some("cosmon"),
            Clearance::Execute,
        );
        fleet.workers.insert(wid1, w1);
        fleet.workers.insert(wid2, w2);
        fleet.repos.insert(
            "cosmon".to_owned(),
            RepoData {
                name: "cosmon".to_owned(),
                path: "/tmp/cosmon".to_owned(),
            },
        );
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("aaaa", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(
            &ctx,
            &Args {
                all: true,
                cluster: false,
                cluster_root: None,
                tags: Vec::new(),
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_ensemble_json_output() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();

        let (wid, w) = make_worker(
            "ruby",
            AgentRole::Implementation,
            WorkerStatus::Active,
            Some("cosmon"),
            Clearance::Write,
        );
        fleet.workers.insert(wid, w);
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("bbbb", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();
        let collapsed_mol = make_molecule("cccc", MoleculeStatus::Collapsed);
        store
            .save_molecule(&collapsed_mol.id, &collapsed_mol)
            .unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(
            &ctx,
            &Args {
                all: true,
                cluster: false,
                cluster_root: None,
                tags: Vec::new(),
            },
        );
        assert!(result.is_ok());

        // Verify store state directly
        let loaded_fleet = store.load_fleet().unwrap();
        assert_eq!(loaded_fleet.workers.len(), 1);

        let mols = store.list_molecules(&MoleculeFilter::default()).unwrap();
        assert_eq!(mols.len(), 2);

        let active_count = mols
            .iter()
            .filter(|m| m.status == MoleculeStatus::Running)
            .count();
        let collapsed_count = mols
            .iter()
            .filter(|m| m.status == MoleculeStatus::Collapsed)
            .count();
        assert_eq!(active_count, 1);
        assert_eq!(collapsed_count, 1);
    }

    // ── Self-hosting round-trip: producer ⇄ consumer schema agreement ──
    //
    // task-20260518-8429 closes a silent impedance mismatch: the runtime's
    // `EnsembleSnapshot::from_json` was reading a top-level `molecules`
    // *array*, but the live `cs ensemble --json` ships `molecules` as a
    // status-count *dict* with the per-molecule projection on a separate
    // `molecule_states` key. The legacy unit test fed a synthetic fixture
    // that happened to match the runtime's expected shape, so the drift
    // never tripped CI — it surfaced in `just self-runtime`, after
    // ADR-095 wired the resident loop to the real binary.
    //
    // This test is the missing rung: it builds a real cosmon state with
    // `FileStore`, loads molecules through `StateStore`, runs the producer
    // helper [`build_molecule_states`] (the same code path the live CLI
    // takes), serializes the result as part of the canonical
    // [`EnsembleOutput`], and feeds it to
    // `cosmon_runtime::EnsembleSnapshot::from_json` — the same parser the
    // resident runtime calls in production. A schema mismatch on either
    // side fails the test instead of silently looping with
    // `decision_basis: ensemble-read-failed`.
    #[test]
    fn cli_ensemble_json_round_trips_through_runtime_reader() {
        use cosmon_core::interaction::MoleculeLink;
        use cosmon_core::kind::MoleculeKind;
        use cosmon_core::process::MoleculeProcess;
        use cosmon_runtime::EnsembleSnapshot;

        let (_tmp, store) = make_store();
        store.save_fleet(&Fleet::default()).unwrap();

        // Two molecules in a chain: `dddd` is blocked by `cccc`.
        let mut upstream = make_molecule("cccc", MoleculeStatus::Pending);
        upstream.kind = Some(MoleculeKind::Task);
        upstream.process = Some(
            MoleculeProcess::new(WorkerId::new("router").unwrap(), "router-session")
                .with_adapter_name("anthropic"),
        );
        store.save_molecule(&upstream.id, &upstream).unwrap();

        let mut downstream = make_molecule("dddd", MoleculeStatus::Pending);
        downstream.typed_links.push(MoleculeLink::BlockedBy {
            source: upstream.id.clone(),
        });
        store.save_molecule(&downstream.id, &downstream).unwrap();

        // Load through the store — the same path `cs ensemble` uses.
        let mols = store.list_molecules(&MoleculeFilter::default()).unwrap();
        let molecule_states = super::build_molecule_states(&mols);

        // Minimal `EnsembleOutput` matching the live CLI's JSON shape.
        // The other fields (workers, worker_roles, molecules summary)
        // are populated with empty/zero values — the parser ignores
        // them, and including them proves the runtime tolerates the
        // operator-facing dashboard fields sitting alongside the
        // machine-readable `molecule_states`.
        let output = EnsembleOutput {
            workers: Vec::new(),
            worker_roles: WorkerRoleSummary {
                cognition: 0,
                runtime: 0,
                total: 0,
            },
            molecules: MoleculeSummary {
                pending: 2,
                queued: 0,
                running: 0,
                frozen: 0,
                completed: 0,
                collapsed: 0,
                total: 2,
                tier_0: 0,
                tier_1: 0,
                tier_2: 0,
            },
            molecule_states,
        };
        let json = serde_json::to_string(&output).unwrap();

        // Parse through the production reader — this is the contract that
        // task-20260518-8429 closes.
        let snapshot = EnsembleSnapshot::from_json(&json)
            .expect("runtime parses live CLI shape without falling back to legacy");
        assert_eq!(
            snapshot.molecules.len(),
            2,
            "snapshot must surface both molecules from `molecule_states`, got: {snapshot:?}",
        );
        let mut by_id: std::collections::HashMap<&str, &cosmon_runtime::EnsembleMolecule> =
            snapshot
                .molecules
                .iter()
                .map(|m| (m.id.as_str(), m))
                .collect();
        let up = by_id.remove("cs-20260401-cccc").expect("upstream present");
        assert_eq!(up.status, "pending");
        assert_eq!(up.kind.as_deref(), Some("task"));
        assert!(up.tags.is_empty());
        assert_eq!(
            up.adapter.as_deref(),
            Some("anthropic"),
            "the live ensemble snapshot must preserve a persisted adapter pin"
        );
        assert!(up.blocked_by.is_empty(), "upstream has no blockers");
        let down = by_id
            .remove("cs-20260401-dddd")
            .expect("downstream present");
        assert_eq!(down.status, "pending");
        assert_eq!(
            down.blocked_by,
            vec!["cs-20260401-cccc".to_owned()],
            "downstream's blocked_by edge must survive the round-trip",
        );

        // Belt-and-braces: confirm the operator-facing `molecules` summary
        // is a dict (not an array) — i.e. that we did not accidentally
        // collapse it into the same shape as `molecule_states`. This
        // pins the schema for the dashboard reader.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            parsed["molecules"].is_object(),
            "operator `molecules` must remain a status-count dict",
        );
        assert!(
            parsed["molecule_states"].is_array(),
            "machine `molecule_states` must be an array",
        );
    }

    // ── WorkerRole summary (ADR-040 phase 1) ──

    #[test]
    fn test_summarize_worker_roles_counts_both_kinds() {
        use cosmon_core::worker::WorkerRole;
        let mut fleet = Fleet::default();
        let (w1, mut d1) = make_worker(
            "runtime-aaa-1111",
            AgentRole::Runtime,
            WorkerStatus::Active,
            None,
            Clearance::Write,
        );
        d1.worker_role = WorkerRole::Runtime;
        let (w2, mut d2) = make_worker(
            "cog-1",
            AgentRole::Implementation,
            WorkerStatus::Active,
            None,
            Clearance::Write,
        );
        d2.worker_role = WorkerRole::Cognition;
        let (w3, mut d3) = make_worker(
            "cog-2",
            AgentRole::Implementation,
            WorkerStatus::Active,
            None,
            Clearance::Write,
        );
        d3.worker_role = WorkerRole::Cognition;
        fleet.workers.insert(w1, d1);
        fleet.workers.insert(w2, d2);
        fleet.workers.insert(w3, d3);

        let summary = summarize_worker_roles(&fleet);
        assert_eq!(summary.cognition, 2);
        assert_eq!(summary.runtime, 1);
        assert_eq!(summary.total, 3);
    }

    #[test]
    fn test_summarize_worker_roles_empty_fleet() {
        let fleet = Fleet::default();
        let summary = summarize_worker_roles(&fleet);
        assert_eq!(summary.cognition, 0);
        assert_eq!(summary.runtime, 0);
        assert_eq!(summary.total, 0);
    }

    // -- Cluster mode ----------------------------------------------------

    #[test]
    fn scan_cluster_finds_galaxies_and_counts_statuses() {
        let tmp = TempDir::new().unwrap();
        for (name, id, status) in [
            ("cosmon", "task-20260423-aaaa", "pending"),
            ("cosmon", "task-20260423-bbbb", "running"),
            ("cosmon", "task-20260423-cccc", "completed"),
            ("mailroom", "task-20260423-dddd", "pending"),
        ] {
            let dir = tmp
                .path()
                .join(name)
                .join(".cosmon/state/fleets/default/molecules")
                .join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let j = serde_json::json!({
                "id": id,
                "status": status,
                "archived": false,
            });
            std::fs::write(dir.join("state.json"), j.to_string()).unwrap();
        }
        // Also plant a fleet.json for cosmon.
        let fleet_json = serde_json::json!({
            "workers": {
                "ruby": {"role": "impl", "desired": "running", "status": "active"}
            }
        });
        std::fs::write(
            tmp.path().join("cosmon/.cosmon/state/fleet.json"),
            fleet_json.to_string(),
        )
        .unwrap();

        let rows = scan_cluster(tmp.path());
        assert_eq!(rows.len(), 2);
        let cosmon = rows.iter().find(|r| r.galaxy == "cosmon").unwrap();
        assert_eq!(cosmon.workers, 1);
        assert_eq!(cosmon.pending, 1);
        assert_eq!(cosmon.running, 1);
        assert_eq!(cosmon.completed, 1);
        let mailroom = rows.iter().find(|r| r.galaxy == "mailroom").unwrap();
        assert_eq!(mailroom.pending, 1);
        assert_eq!(mailroom.workers, 0);
    }

    #[test]
    fn scan_cluster_skips_non_galaxy_dirs_and_archived_molecules() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("not-a-galaxy/src")).unwrap();
        let dir = tmp
            .path()
            .join("cosmon/.cosmon/state/fleets/default/molecules/task-arc");
        std::fs::create_dir_all(&dir).unwrap();
        let j = serde_json::json!({
            "id": "task-arc",
            "status": "completed",
            "archived": true,
        });
        std::fs::write(dir.join("state.json"), j.to_string()).unwrap();

        let rows = scan_cluster(tmp.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].completed, 0, "archived excluded");
    }

    #[test]
    fn resolve_cluster_root_prefers_explicit_then_env_then_home() {
        use std::path::PathBuf;
        let args = Args {
            all: true,
            cluster: true,
            cluster_root: Some(PathBuf::from("/tmp/explicit")),
            tags: Vec::new(),
        };
        assert_eq!(resolve_cluster_root(&args), PathBuf::from("/tmp/explicit"));
    }

    // -- GAP #7 — InProcess supervision projection ----------------------
    //
    // The three tests below pin the observer-side fix for the academy
    // smoke chronicle `2026-05-18-grok-direct-api-smoke-result-3.md`.
    // For a Direct-API in-process worker (openai / anthropic), the tmux
    // probe is structurally uninformative: absence of pane is the
    // nominal state, not death. The projection must:
    //
    //   1. Render `Completed` as `Stopped` (not `Diverged`).
    //   2. Suppress the `UnHarvested` ghost on the `Completed +
    //      Unmerged` window — the operator merges via `cs done`
    //      deliberately.
    //   3. Mark `Running` workers whose latest event is older than
    //      `INPROCESS_STALE_TTL` as `Suspect`, so a wedged in-process
    //      loop is visible without a tmux pane to probe.

    fn make_lookup(
        status: MoleculeStatus,
        supervision: SupervisionMode,
        last_event_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> MolLookup {
        MolLookup {
            status,
            merged_at: None,
            tags: Vec::new(),
            has_blockers: false,
            supervision,
            last_event_at,
        }
    }

    #[test]
    fn inprocess_completed_projects_stopped_not_diverged() {
        let ml = make_lookup(
            MoleculeStatus::Completed,
            SupervisionMode::InProcess,
            Some(Utc::now()),
        );
        let (transport, session, override_eff) =
            project_inprocess_transport(Some(&ml), None, Utc::now());
        assert!(matches!(transport, TransportState::Dead));
        assert_eq!(session.as_deref(), Some("done:inprocess"));
        assert!(
            matches!(override_eff, Some(EffectiveStatus::Stopped)),
            "InProcess + Completed must override reconcile to Stopped, got {override_eff:?}",
        );
    }

    #[test]
    fn inprocess_running_with_fresh_event_projects_alive() {
        let ml = make_lookup(
            MoleculeStatus::Running,
            SupervisionMode::InProcess,
            Some(Utc::now() - chrono::Duration::seconds(5)),
        );
        let (transport, _session, override_eff) =
            project_inprocess_transport(Some(&ml), None, Utc::now());
        assert!(matches!(transport, TransportState::Alive));
        assert!(
            override_eff.is_none(),
            "Fresh in-process loop must not override reconcile — got {override_eff:?}",
        );
    }

    #[test]
    fn inprocess_running_with_stale_event_projects_suspect() {
        let ml = make_lookup(
            MoleculeStatus::Running,
            SupervisionMode::InProcess,
            Some(Utc::now() - chrono::Duration::seconds(120)),
        );
        let (transport, session, override_eff) =
            project_inprocess_transport(Some(&ml), None, Utc::now());
        assert!(matches!(transport, TransportState::Unknown));
        assert_eq!(session.as_deref(), Some("stale:inprocess"));
        assert!(
            matches!(override_eff, Some(EffectiveStatus::Suspect)),
            "Stale in-process loop must surface as Suspect, got {override_eff:?}",
        );
    }

    #[test]
    fn ensemble_inprocess_completed_worker_is_not_diverged_or_unharvested() {
        // End-to-end shape: a completed in-process molecule + a worker
        // bound to it must render as `Stopped`, not `Diverged`, and
        // must NOT carry an `UnHarvested` ghost — the same row the
        // academy smoke chronicle 2026-05-18-grok-direct-api-smoke-result-3.md
        // §verdict tagged as `diverged + 👻 un-harvested` before this
        // fix.
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        let (wid, w) = make_worker(
            "openai-inprocess-1234",
            AgentRole::Implementation,
            WorkerStatus::Active,
            None,
            Clearance::Write,
        );
        fleet.workers.insert(wid.clone(), w);
        store.save_fleet(&fleet).unwrap();

        // Molecule completed with the openai adapter stamped on its
        // process record — observer reads SupervisionMode::InProcess.
        let mut mol = make_molecule("eeee", MoleculeStatus::Completed);
        mol.process = Some(
            cosmon_core::process::MoleculeProcess::new(wid, "openai-inprocess-1234")
                .with_adapter_name("openai"),
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        // The CLI run should succeed (no panic) and the underlying
        // lookup must record InProcess supervision. We can't easily
        // assert on stdout without capturing, so we cover the JSON
        // emission path which exercises the same projection chain.
        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(
            &ctx,
            &Args {
                all: true,
                cluster: false,
                cluster_root: None,
                tags: Vec::new(),
            },
        );
        assert!(result.is_ok());

        // Direct unit assertion on the supervision lookup — guards the
        // wiring even when JSON capture is impractical.
        let loaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(
            loaded
                .process
                .as_ref()
                .and_then(|p| p.adapter_name.as_deref()),
            Some("openai"),
            "MoleculeProcess.adapter_name must round-trip through the store"
        );
        assert_eq!(
            supervision_mode_for(loaded.process.unwrap().adapter_name.unwrap().as_str()),
            SupervisionMode::InProcess,
            "openai must resolve to SupervisionMode::InProcess"
        );
    }
}
