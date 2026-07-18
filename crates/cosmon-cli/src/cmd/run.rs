// SPDX-License-Identifier: AGPL-3.0-only

//! `cs run` — spin up the resident runtime on a molecule DAG.
//!
//! This is the Layer B (resident runtime) entry point from ADR-016. It
//! compiles a [`Plan`] from a root molecule and its transitive dependency
//! closure, instantiates a [`Policy`], and runs the event loop until the
//! plan drains or the timeout expires.
//!
//! # Distinct perimeter
//!
//! `cs run` does **not** replace `cs tackle` (single-molecule, L1) or
//! `cs wait` (observation primitive). It composes with them: an operator
//! can `cs tackle` a molecule manually, `cs run` a DAG that contains
//! sibling molecules, or mix both.
//!
//! # ADR-016 coherence
//!
//! Stateless invocation (no daemon — runs until drained or deadline).
//! The runtime is a **client** of the transactional core: every mutation
//! goes through the shared [`StateStore`]. Humans can `cs observe` while
//! the loop is running.
//!
//! # Output: shared renderer with `cs watch`
//!
//! `cs run` and `cs watch` both observe the same on-disk state store and
//! project changes into the same diff-based event log. The rendering
//! lives in [`crate::event_log`] and both commands reuse it — operators
//! see identical output shapes for equivalent transitions. The only
//! differences:
//!
//! - `cs run` is action-bearing: Ctrl-C trips the runtime shutdown and
//!   the DAG stops dispatching. `cs watch` is read-only.
//! - `cs run` heartbeat is labeled `run`, `cs watch` is labeled `watch`.
//! - `cs run` suppresses the stdout/stderr of the child `cs tackle`
//!   subprocesses it spawns so the event log stays clean — the runtime
//!   prints what changed, not the chatter of each dispatch.
//!
//! The rendering loop runs in a background thread that polls the
//! filestore on its own cadence; the main thread drives the runtime
//! synchronously. When the runtime returns, the main thread signals the
//! renderer to drain and shut down.
//!
//! [`Plan`]: cosmon_graph::Plan
//! [`Policy`]: cosmon_runtime::Policy
//! [`StateStore`]: cosmon_state::StateStore

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Local, Utc};
use colored::Colorize;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_runtime::{
    compile_plan, load_parallel_limits, load_step_models, DagPolicy, ModelResolver, NoOpPolicy,
    Runtime, RuntimeConfig, ShutdownReason, SubprocessExecutor,
};
use cosmon_state::{MoleculeFilter, StateStore};

use crate::event_log::{
    clear_line, poll_and_diff, print_baseline, print_events, render_heartbeat, Snapshot,
    HEARTBEAT_INTERVAL_MS, LOOP_SLEEP_MS, SPINNER_FRAMES,
};

use super::Context;

/// Run the resident runtime on a molecule DAG.
// CLI flag structs are naturally bool-heavy (one per toggle); the lint's
// "collapse into an enum" advice does not fit clap's derive model.
#[allow(clippy::struct_excessive_bools)]
#[derive(clap::Args)]
pub struct Args {
    /// Root molecule ID (supports prefix matching like other commands).
    ///
    /// Required for the legacy DAG-policy mode. With `--resident` the
    /// loop walks the whole ensemble, so this argument is optional
    /// (any value, including `_`, is accepted and ignored).
    #[arg(default_value = "")]
    pub molecule: String,

    /// Scheduling policy to use.
    #[arg(long, default_value = "dag", value_parser = ["dag", "noop"])]
    pub policy: String,

    /// Maximum seconds before the runtime exits. 0 means no timeout (default).
    #[arg(long, default_value_t = 0)]
    pub timeout: u64,

    /// Seconds between runtime ticks. Lower values are more responsive
    /// but increase store I/O.
    #[arg(long, default_value_t = 1)]
    pub poll_interval: u64,

    /// Skip automatic teardown of completed molecules after the run.
    #[arg(long)]
    pub no_teardown: bool,

    /// ADR-038 Limit 1: re-walk the store every N ticks to absorb
    /// descendants nucleated dynamically by workers (mission-controller
    /// decompose, deep-think step 4, etc.) that are not reachable from
    /// the runtime's root via pre-existing typed links. Zero disables the
    /// sweep (default) — the scope is frozen at compile-plan time, which
    /// is the pre-2026-04-14 behavior.
    #[arg(long, default_value_t = 0)]
    pub sweep_every: u32,

    /// Override the ADR-048 backlog-sanity guard on runtime bootstrap.
    ///
    /// When a dirty backlog would normally refuse runtime bootstrap
    /// (sediment ≥ threshold, default 5), `--force-runtime` bypasses the
    /// refusal and writes a `runtime_guard_override` audit event to
    /// `events.jsonl` so the override leaves a durable trail.
    #[arg(long)]
    pub force_runtime: bool,

    /// B3 — decreasing action budget (moussage bounds). Each applied
    /// runtime action costs one
    /// unit; when the budget floor is reached the loop exits with the
    /// NAMED reason `budget_exhausted` (exit code 90) instead of
    /// dispatching further. This is the well-founded measure that
    /// makes an unbounded moussage total. 0 = unbounded (default,
    /// operator-local behaviour unchanged). Server-side callers (the
    /// tenant drain path) pass the binding-derived value — the bound
    /// is never client-writable.
    #[arg(long, default_value_t = 0)]
    pub max_actions: u64,

    /// B1 — maximum DAG depth (longest dependency chain, in
    /// molecules). Checked at compile-plan time, BEFORE the loop
    /// starts: a plan deeper than the bound is refused with the NAMED
    /// error `max_depth_exceeded` (exit code 92), never started.
    /// 0 = unbounded (default).
    #[arg(long, default_value_t = 0)]
    pub max_depth: u32,

    /// B2 — maximum molecules tolerated in the fleet while draining.
    /// Checked at compile-plan time AND on every loop tick (so mid-run
    /// nucleations count); exceeding it exits with the NAMED reason
    /// `molecule_quota_exceeded` (exit code 91). 0 = unbounded
    /// (default).
    #[arg(long, default_value_t = 0)]
    pub max_molecules: u64,

    /// **ADR-095** — switch to the fully event-sourced Resident Runtime loop.
    ///
    /// When set, the legacy in-process `DagPolicy` is bypassed and the
    /// new [`cosmon_runtime::RuntimeLoop`] takes over. The loop:
    ///
    /// - Shells out to `cs ensemble --json`, `cs tackle`, `cs done`
    ///   exactly as a human operator would (RR-1).
    /// - Wakes on FS changes under `.cosmon/state/` (notify backend)
    ///   plus a `--poll-interval` heartbeat.
    /// - Writes an NDJSON trace line per loop iteration to
    ///   `.cosmon/state/runtime-trace.jsonl` (RR-5).
    /// - Exits cleanly on SIGTERM / Ctrl-C.
    /// - Drains when the ensemble has no `pending` and no `running`.
    ///
    /// Does not require `<molecule>` — the loop walks the whole
    /// ensemble. The positional argument is accepted but ignored in
    /// resident mode; pass `_` if you have nothing to name.
    #[arg(long)]
    pub resident: bool,

    /// **ADR-145** — model-affinity ordering of the ready frontier.
    ///
    /// On a single-resident-model local oracle (`ollama-g5`: 48 GB ≈ one
    /// 120 B model in VRAM), an alternating frontier reloads the model
    /// (~40 GB off disk) on every dispatch. With `--affinity` the runtime
    /// clusters same-model molecules contiguously and drains the resident
    /// model first, so a same-model batch pays the load cost once. The
    /// per-molecule model is PRE-RESOLVED from each molecule's formula-step
    /// `model =` pin (the ADR-142 Incarnation model), since a pending
    /// frontier molecule has no `ModelSelected` event yet.
    ///
    /// Off by default: cloud dispatch (many models, no resident constraint)
    /// keeps pure critical-path order. The reorder is a permutation — the
    /// DAG semantics and the set of dispatched molecules are unchanged; only
    /// the order within a ready batch differs. Legacy DAG-policy mode only
    /// (not `--resident`).
    #[arg(long)]
    pub affinity: bool,

    /// Model already warm in the oracle's VRAM at runtime start, so the
    /// affinity reorder drains its bucket first with no reload. Only read
    /// when `--affinity` is set; a cold start (unset) simply pays one extra
    /// load for the first bucket.
    #[arg(long)]
    pub resident_model: Option<String>,
}

/// Execute the `run` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    // ADR-095: `--resident` swaps the legacy in-process DagPolicy for the
    // CLI-client RuntimeLoop. The new path imports no state-mutating
    // crate at its module boundary, satisfies RR-1 through RR-5, and
    // writes an NDJSON trace to `.cosmon/state/runtime-trace.jsonl`.
    if args.resident {
        return run_resident(ctx, args);
    }

    // 1. Resolve the root molecule (exact, then prefix match).
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);
    let root_id = resolve_molecule_id(&store, &args.molecule)?;

    // ADR-048: backlog-sanity guard at runtime bootstrap. Must fire before
    // any DAG compile / dispatch so the walker never touches a sedimented
    // store. `--force-runtime` emits an audit event and proceeds.
    let report = super::guard::check_runtime_backlog_or_refuse(&store, args.force_runtime)?;
    super::guard::emit_runtime_guard_override("cs run", &root_id, &report);

    // 2. Build the policy.
    let config = RuntimeConfig {
        poll_interval: Duration::from_secs(args.poll_interval.max(1)),
        max_runtime: if args.timeout == 0 {
            None
        } else {
            Some(Duration::from_secs(args.timeout))
        },
        sweep_orphan_descendants_every: if args.sweep_every == 0 {
            None
        } else {
            Some(args.sweep_every)
        },
        // Phantom-workers part 2: inherit the production default (every
        // 10 ticks ≈ 10s at the default 1s poll). Operators wanting to
        // disable the recheck can tune this via a future flag, but the
        // default is on so the production code path closes the gap by
        // construction.
        liveness_recheck_every: Some(10),
    };

    // Compile the DAG once here and stash the molecule id closure. The
    // final summary reuses this set instead of invoking `compile_plan`
    // a second time after the runtime returns — walking the DAG is an
    // O(|closure|) store read and the topology does not change between
    // the start and end of a single `cs run` invocation.
    let (plan, edges) = compile_plan(&store, std::slice::from_ref(&root_id))?;
    let dag_ids: std::collections::HashSet<MoleculeId> = {
        let mut ids = std::collections::HashSet::new();
        ids.insert(root_id.clone());
        for (a, b) in &edges {
            ids.insert(a.clone());
            ids.insert(b.clone());
        }
        ids
    };

    // B1 — depth bound (moussage bounds, task-20260610-e5f6): a plan
    // deeper than the bound is REFUSED before the loop starts. Named
    // failure, never a stall (I4): stable token on stderr/JSON, stable
    // exit code 92.
    if args.max_depth > 0 {
        let depth = cosmon_runtime::dag_depth(&edges).max(1);
        if depth > args.max_depth as usize {
            if ctx.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "root": root_id.as_str(),
                        "error": "max_depth_exceeded",
                        "depth": depth,
                        "max_depth": args.max_depth,
                    })
                );
            } else {
                eprintln!(
                    "✗ max_depth_exceeded: DAG depth {} exceeds bound {} — refusing to start",
                    depth, args.max_depth
                );
            }
            std::process::exit(92);
        }
    }
    // B2 at compile time — the same named refusal applies if the plan
    // is already wider than the bound (the in-loop tick check covers
    // mid-run growth).
    if args.max_molecules > 0 && dag_ids.len() as u64 > args.max_molecules {
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({
                    "root": root_id.as_str(),
                    "error": "molecule_quota_exceeded",
                    "molecules": dag_ids.len(),
                    "max_molecules": args.max_molecules,
                })
            );
        } else {
            eprintln!(
                "✗ molecule_quota_exceeded: DAG has {} molecules, bound is {} — refusing to start",
                dag_ids.len(),
                args.max_molecules
            );
        }
        std::process::exit(91);
    }

    let policy: Box<dyn cosmon_runtime::Policy> = if args.policy == "noop" {
        Box::new(NoOpPolicy)
    } else {
        // ADR-043: collect parallel_limit declarations from every formula
        // referenced by molecules in the DAG. A zero-entry map = unbounded
        // (the pre-ADR-043 default); any Static limit becomes an enforced
        // per-step cap.
        let formulas_dir = cosmon_filestore::resolve_formulas_dir_from(&state_dir);
        let mut formula_ids: Vec<cosmon_core::id::FormulaId> = Vec::new();
        {
            let mut seen = std::collections::HashSet::new();
            for id in &dag_ids {
                if let Ok(m) = store.load_molecule(id) {
                    if seen.insert(m.formula_id.clone()) {
                        formula_ids.push(m.formula_id);
                    }
                }
            }
        }
        let limits = load_parallel_limits(&formulas_dir, &formula_ids);

        // Phantom-workers fix #2: when `cs run <root>` is invoked on a
        // molecule that is already terminal (Collapsed / Completed /
        // Frozen), the operator is explicitly asking the runtime to
        // continue past the root. Pre-seeding the named root into
        // `completed` makes its descendants eligible at tick 0 rather
        // than waiting for the root to be re-observed as terminal and
        // absorbed. Since task-20260706-4d1e a collapsed root also
        // releases its forward `Blocks` dependents on its own when
        // absorbed (blocked-by releases on done, not on verdict), so this
        // hook is now the tick-0 fast path rather than the sole unblock
        // mechanism it was under option B.
        // See `docs/diagnostic/2026-04-25-phantom-workers.md`.
        let pre_completed: Vec<MoleculeId> = match store.load_molecule(&root_id) {
            Ok(root) if root.status.is_terminal() => {
                if !ctx.json {
                    eprintln!(
                        "ℹ runtime: root {} is {} — pre-seeding skip-set so descendants can drain.",
                        root_id, root.status
                    );
                }
                vec![root_id.clone()]
            }
            _ => Vec::new(),
        };
        let mut dag_policy = DagPolicy::new(plan, edges)
            .with_limits(limits)
            .with_pre_completed(pre_completed);

        // ADR-145: model-affinity ordering of the ready frontier. The CLI
        // layer owns the filesystem lookups (the pure policy does not), so
        // the model resolver is built here from the DAG's formula-step
        // `model =` pins and injected. `load_step_models` PRE-RESOLVES each
        // pending molecule's Incarnation model (ADR-142) at frontier-ordering
        // time — there is no `ModelSelected` event before a molecule is
        // tackled. Off unless `--affinity`, so cloud dispatch is untouched.
        if args.affinity {
            let step_models = load_step_models(&formulas_dir, &formula_ids);
            let resolver = ModelResolver::new(move |mol: &cosmon_state::MoleculeData| {
                step_models
                    .get(&(mol.formula_id.clone(), mol.current_step))
                    .cloned()
            });
            dag_policy = dag_policy
                .with_affinity(resolver)
                .with_resident_model(args.resident_model.clone().filter(|s| !s.is_empty()));
        }

        Box::new(dag_policy)
    };

    // 3. Print header (unless --json).
    if !ctx.json {
        println!(
            "{} runtime on {} with {} policy (timeout {}s)",
            "Starting".green().bold(),
            root_id.as_str().bold(),
            args.policy.cyan(),
            args.timeout,
        );
    }

    // 4. Spawn the background renderer thread.
    //
    // The renderer observes the same on-disk state store and emits the
    // same diff-based event log as `cs watch`. Running it on its own
    // thread decouples the rendering cadence from the runtime tick
    // cadence — the operator sees state changes within a poll interval
    // (~250 ms) even if the runtime ticks less frequently.
    //
    // The renderer is gated behind `ctx.json`: JSON mode suppresses all
    // human-readable output so machine readers get a single summary
    // object at the end of the run.
    let renderer_stop = Arc::new(AtomicBool::new(false));
    let renderer_handle = if ctx.json {
        None
    } else {
        let stop = renderer_stop.clone();
        let dir = state_dir.clone();
        Some(thread::spawn(move || {
            if let Err(e) = render_loop(&dir, &stop) {
                eprintln!("⚠ event log renderer exited: {e}");
            }
        }))
    };

    // 5. Run the event loop. SubprocessExecutor is configured in quiet
    //    mode so child `cs tackle`/`cs done` invocations do not flood
    //    the event log with their own stdout.
    let store_box: Box<dyn StateStore> = Box::new(FileStore::new(&state_dir));
    let executor = Box::new(SubprocessExecutor::new(&state_dir).quiet(true));
    let liveness = Box::new(cosmon_runtime::TmuxLivenessCheck::new(
        super::tmux_socket_name(ctx),
    ));
    // Server-side drain bounds (task-20260610-e5f6): 0 = unbounded.
    let bounds = cosmon_runtime::RunBounds {
        max_actions: (args.max_actions > 0).then_some(args.max_actions),
        max_molecules: (args.max_molecules > 0)
            .then(|| usize::try_from(args.max_molecules).unwrap_or(usize::MAX)),
    };
    // Realized-model runtime consumer (round-3 / F-01): every tick, probe the
    // live claude/codex session of each in-scope Running molecule and emit
    // `ModelObserved` at the first model-bearing turn — durable on the journal
    // even if the worker crashes before `cs complete`.
    let probe_state_dir = state_dir.clone();
    let probe_backends = crate::energy_probe::discover_fleet_backends(
        &probe_state_dir,
        &super::tmux_socket_name(ctx),
    );
    let tick_probe: Box<dyn FnMut(&cosmon_core::id::MoleculeId)> = Box::new(move |mol_id| {
        crate::energy_probe::capture_realized_runtime(&probe_state_dir, mol_id, &probe_backends);
    });
    let mut runtime = Runtime::new(store_box, policy, executor, config)
        .with_liveness_check(liveness)
        .with_run_bounds(bounds)
        .with_tick_probe(tick_probe);

    // Wire Ctrl-C to the shutdown signal so the loop stops gracefully.
    let handle = runtime.shutdown_handle();
    let _ = ctrlc_wire(&handle);

    let report = runtime.run().map_err(|e| anyhow::anyhow!("{e}"))?;

    // 6. Stop the renderer, letting it drain one final poll first so the
    //    terminal transitions from the last runtime tick are visible.
    renderer_stop.store(true, Ordering::SeqCst);
    if let Some(h) = renderer_handle {
        let _ = h.join();
    }

    // 7. Load final state of molecules in the DAG for the summary.
    //    `dag_ids` was computed above from the one-shot compile_plan.
    let all_mols = store.list_molecules(&MoleculeFilter::default())?;
    let dag_mols: Vec<_> = all_mols
        .iter()
        .filter(|m| dag_ids.contains(&m.id))
        .collect();

    // 8. Output summary.
    if ctx.json {
        let mol_states: Vec<serde_json::Value> = dag_mols
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id.as_str(),
                    "status": m.status.to_string(),
                })
            })
            .collect();
        let json_out = serde_json::json!({
            "root": root_id.as_str(),
            "policy": args.policy,
            "reason": format!("{:?}", report.reason),
            "ticks": report.ticks,
            "actions_applied": report.actions_applied,
            "molecules": mol_states,
        });
        println!("{}", serde_json::to_string_pretty(&json_out)?);
    } else {
        let reason_str = match report.reason {
            ShutdownReason::PolicyDrained => "plan drained".green().to_string(),
            ShutdownReason::SignalTripped => "signal (Ctrl-C)".yellow().to_string(),
            ShutdownReason::Deadline => "timeout".red().to_string(),
            ShutdownReason::BudgetExhausted => "budget_exhausted (B3)".red().to_string(),
            ShutdownReason::MoleculeQuotaExceeded => {
                "molecule_quota_exceeded (B2)".red().to_string()
            }
        };
        println!(
            "\n{} {} ticks, {} actions — {}",
            "Done:".bold(),
            report.ticks,
            report.actions_applied,
            reason_str,
        );
        for m in &dag_mols {
            let status_str = colorize_status(m.status);
            println!("  {} {}", m.id, status_str);
        }
    }

    // 9. Auto-teardown: run `cs done` on every completed molecule in the DAG
    //    to clean up worktrees, tmux sessions, fleet entries, and merge branches.
    if !args.no_teardown {
        let mut torn_down = 0;
        for m in &dag_mols {
            if m.status.is_terminal() {
                let done_result = std::process::Command::new("cs")
                    .args(["done", m.id.as_str()])
                    .current_dir(state_dir.parent().unwrap_or(&state_dir))
                    .status();
                match done_result {
                    Ok(s) if s.success() => torn_down += 1,
                    _ => {
                        if !ctx.json {
                            eprintln!("  ⚠ teardown of {} failed (non-fatal)", m.id);
                        }
                    }
                }
            }
        }
        if !ctx.json && torn_down > 0 {
            println!("  Torn down {torn_down} completed molecule(s)");
        }
    }

    // Exit with non-zero if deadline hit (like cs wait).
    if report.reason == ShutdownReason::Deadline {
        std::process::exit(124);
    }
    // Moussage bounds (task-20260610-e5f6): named, stable exit codes so
    // a supervisor (or the tenant drain route) distinguishes the failure
    // shape without parsing output. 90 = B3 budget, 91 = B2 quota.
    if report.reason == ShutdownReason::BudgetExhausted {
        std::process::exit(90);
    }
    if report.reason == ShutdownReason::MoleculeQuotaExceeded {
        std::process::exit(91);
    }

    Ok(())
}

/// Background event-log loop. Polls the state store, prints diff events,
/// and refreshes a rolling heartbeat footer until `stop` is tripped.
///
/// Structurally identical to the poll + heartbeat tiers of [`cs watch`].
/// The propel tier is deliberately omitted: `cs run` owns dispatch
/// through the runtime, so propel nudges would double-fire.
///
/// `cs watch` — see `crate::cmd::watch`
fn render_loop(state_dir: &PathBuf, stop: &AtomicBool) -> anyhow::Result<()> {
    let store = FileStore::new(state_dir);
    let mut stdout = std::io::stdout();
    let tty = stdout.is_terminal();

    let session_start = Instant::now();
    let mut prev: Option<Snapshot> = None;

    // Baseline poll.
    let first = poll_and_diff(&store, prev.as_ref())?;
    print_baseline(&mut stdout, tty, Utc::now(), &first)?;
    prev = Some(first.snapshot);

    let poll_interval = Duration::from_millis(1000_u64.max(HEARTBEAT_INTERVAL_MS));
    let heartbeat_interval = Duration::from_millis(HEARTBEAT_INTERVAL_MS);

    let now0 = Instant::now();
    let mut next_poll = now0 + poll_interval;
    let mut next_heartbeat = now0 + heartbeat_interval;
    let mut spinner_idx = 0usize;

    loop {
        if stop.load(Ordering::SeqCst) {
            // Drain one final poll so the last runtime tick's transitions
            // are visible even if the stop signal raced ahead of them.
            let outcome = poll_and_diff(&store, prev.as_ref())?;
            print_events(&mut stdout, tty, Utc::now(), &outcome.events)?;
            // Clear the heartbeat footer so the summary prints cleanly.
            if tty {
                clear_line(&mut stdout, tty)?;
                stdout.flush()?;
            }
            return Ok(());
        }

        let now = Instant::now();
        let deadline = next_poll.min(next_heartbeat);
        if now < deadline {
            let wait = (deadline - now).min(Duration::from_millis(LOOP_SLEEP_MS));
            thread::sleep(wait);
            continue;
        }

        if now >= next_poll {
            let outcome = poll_and_diff(&store, prev.as_ref())?;
            print_events(&mut stdout, tty, Utc::now(), &outcome.events)?;
            prev = Some(outcome.snapshot);
            next_poll = Instant::now() + poll_interval;
        }

        if now >= next_heartbeat {
            if tty {
                clear_line(&mut stdout, tty)?;
                let running = prev.as_ref().map_or(0, |s| {
                    s.molecules
                        .values()
                        .filter(|m| m.status == MoleculeStatus::Running)
                        .count()
                });
                let workers = prev.as_ref().map_or(0, |s| s.workers.len());
                let frame = SPINNER_FRAMES[spinner_idx % SPINNER_FRAMES.len()];
                spinner_idx = spinner_idx.wrapping_add(1);
                write!(
                    stdout,
                    "{}",
                    render_heartbeat(
                        "run",
                        Local::now(),
                        session_start.elapsed(),
                        frame,
                        workers,
                        running
                    )
                    .dimmed()
                )?;
                stdout.flush()?;
            }
            next_heartbeat = Instant::now() + heartbeat_interval;
        }
    }
}

/// Resolve a molecule ID by exact match or prefix.
fn resolve_molecule_id(store: &FileStore, query: &str) -> anyhow::Result<MoleculeId> {
    if let Ok(exact_id) = MoleculeId::new(query) {
        if store.load_molecule(&exact_id).is_ok() {
            return Ok(exact_id);
        }
    }

    let all = store.list_molecules(&MoleculeFilter::default())?;
    let prefix_matches: Vec<_> = all
        .iter()
        .filter(|m| m.id.as_str().starts_with(query))
        .collect();

    match prefix_matches.len() {
        0 => Err(anyhow::anyhow!("no molecule matching \"{query}\"")),
        1 => Ok(prefix_matches[0].id.clone()),
        n => {
            let lines: Vec<_> = prefix_matches
                .iter()
                .map(|m| format!("  {}", m.id))
                .collect();
            Err(anyhow::anyhow!(
                "ambiguous prefix \"{query}\" matches {n} molecules:\n{}",
                lines.join("\n")
            ))
        }
    }
}

/// Best-effort Ctrl-C wiring.
fn ctrlc_wire(handle: &cosmon_runtime::ShutdownSignal) -> Result<(), ()> {
    let h = handle.clone();
    ctrlc::set_handler(move || h.trip()).map_err(|_| ())
}

/// **ADR-095** — Resident Runtime entry point.
///
/// Distinct from the legacy [`run`] body: instantiates the
/// CLI-shell-out [`cosmon_runtime::RuntimeLoop`], wires Ctrl-C / SIGTERM
/// to its shutdown flag, and drains.
fn run_resident(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    use cosmon_runtime::{
        ExitReason, ReadyFrontierScheduler, ResidentScheduler, RuntimeLoop, RuntimeLoopConfig,
    };

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    // `state_dir` is `.../.cosmon/state`. The loop wants the project root
    // (i.e. the cwd that contains `.cosmon/`) so its child `cs` calls
    // walk-up-discover the same store.
    let cwd = state_dir
        .parent()
        .and_then(std::path::Path::parent)
        .map_or_else(|| state_dir.clone(), std::path::Path::to_path_buf);

    let mut config = RuntimeLoopConfig::new(&cwd);
    config.poll_interval = Duration::from_secs(args.poll_interval.max(1));
    if args.timeout > 0 {
        config.max_runtime = Some(Duration::from_secs(args.timeout));
    }
    // Bind the loop's child `cs` calls to **this** binary, not whatever
    // `cs` happens to be on `$PATH`. Two concrete failure modes this
    // closes (task-20260518-8429):
    //
    // 1. A stale post-install `cs` on `$PATH` (e.g. installed before the
    //    JSON schema added `molecule_states`) will emit a shape the
    //    in-flight runtime cannot parse — the loop spins on
    //    `ensemble-read-failed` even though *this* binary speaks the
    //    correct protocol.
    // 2. Two cosmon checkouts side-by-side (worktree + main): the
    //    one-shot `cs run --resident` from the worktree should observe
    //    the same projection of state the worktree itself produces, not
    //    whichever sibling installed itself last.
    //
    // Falling back to `"cs"` keeps the tests in `cosmon-runtime` working
    // — they construct `RuntimeLoopConfig::new` directly and rely on
    // their stub being on `$PATH`.
    if let Ok(self_exe) = std::env::current_exe() {
        config.cs_binary = self_exe;
    }

    let scheduler: Box<dyn ResidentScheduler> = Box::new(ReadyFrontierScheduler::new());
    let mut runtime = RuntimeLoop::new(config, scheduler);
    let trace_path = runtime.trace_path().to_path_buf();

    if !ctx.json {
        println!(
            "{} resident runtime (ADR-095) — trace: {}",
            "Starting".green().bold(),
            trace_path.display(),
        );
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let s = shutdown.clone();
        let _ = ctrlc::set_handler(move || s.store(true, Ordering::SeqCst));
    }

    let summary = runtime.run(&shutdown).map_err(|e| anyhow::anyhow!("{e}"))?;

    let reason = match summary.exit {
        ExitReason::Drained => "drained",
        ExitReason::Shutdown => "shutdown",
        ExitReason::Deadline => "deadline",
        ExitReason::ConfigDrift => "config-drift",
    };

    if ctx.json {
        let json_out = serde_json::json!({
            "mode": "resident",
            "exit": reason,
            "ticks": summary.ticks,
            "tackles": summary.tackles,
            "dones": summary.dones,
            "reaps": summary.reaps,
            "briefless_parked": summary.briefless_parked,
            "trace": trace_path.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&json_out)?);
    } else {
        // Surface parked briefless molecules only when non-zero — they are the
        // exceptional case (an operator has molecules needing a brief restored
        // or a collapse), so a clean run stays terse (task-20260711-4310).
        let parked = if summary.briefless_parked > 0 {
            format!(", {} briefless parked", summary.briefless_parked)
        } else {
            String::new()
        };
        println!(
            "\n{} {} ticks, {} tackles, {} dones, {} reaps{} — {}",
            "Done:".bold(),
            summary.ticks,
            summary.tackles,
            summary.dones,
            summary.reaps,
            parked,
            reason,
        );
    }

    if summary.exit == ExitReason::Deadline {
        std::process::exit(124);
    }
    if summary.exit == ExitReason::ConfigDrift {
        // Config-honoring dispatch (delib-20260531-c761): the runtime
        // witnessed that its launch-time config / binary seal no longer
        // matches the on-disk state and halted *before* forming a dispatch.
        // Exit non-zero (EX_TEMPFAIL) so a supervisor relaunches a fresh
        // process that re-derives engine + config from disk — Q2b
        // bounded-ephemeral, never self-repair in place. The forensic
        // `ConfigDriftDetected` event is already in the runtime trace.
        if !ctx.json {
            eprintln!(
                "⚠ runtime halted: config/binary drift since launch — relaunch \
                 `cs run --resident` for a fresh derivation (trace: {})",
                trace_path.display(),
            );
        }
        std::process::exit(75);
    }
    Ok(())
}

/// Colorize a molecule status for human output.
fn colorize_status(status: MoleculeStatus) -> String {
    let s = status.to_string();
    match status {
        MoleculeStatus::Pending => s.cyan().to_string(),
        MoleculeStatus::Queued => s.blue().to_string(),
        MoleculeStatus::Running => s.green().to_string(),
        MoleculeStatus::Frozen => s.yellow().to_string(),
        MoleculeStatus::Starved => s.magenta().to_string(),
        MoleculeStatus::Completed => s.bold().green().to_string(),
        MoleculeStatus::Collapsed => s.red().to_string(),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::interaction::MoleculeLink;
    use cosmon_runtime::NoOpExecutor;
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        (tmp, store)
    }

    fn sample_mol(id: &str, status: MoleculeStatus, links: Vec<MoleculeLink>) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 1,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: cosmon_core::molecule_class::MoleculeClass::default(),
            typed_links: links,
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
    fn test_run_single_node_dag_dispatches_root() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260410-solo", MoleculeStatus::Pending, Vec::new());
        store.save_molecule(&mol.id, &mol).unwrap();

        let (plan, edges) = compile_plan(&store, std::slice::from_ref(&mol.id)).unwrap();
        let policy = DagPolicy::new(plan, edges);
        let config = RuntimeConfig {
            poll_interval: Duration::from_millis(10),
            max_runtime: Some(Duration::from_secs(2)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        };
        let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
        let mut runtime = Runtime::new(store_box, Box::new(policy), Box::new(NoOpExecutor), config);

        let report = runtime.run().unwrap();
        assert_eq!(report.reason, ShutdownReason::Deadline);
        assert_eq!(report.actions_applied, 1, "root should be dispatched");
    }

    /// Regression: `cs run` drained with "plan
    /// drained" while a runnable pending molecule remained, because the
    /// child was nucleated dynamically (only a `BlockedBy` link back to a
    /// tracked-but-completed parent) and the default config disables the
    /// periodic `refresh_scope` sweep. The last-chance rescue at the drain
    /// decision must pull the child into scope and dispatch it instead of
    /// exiting.
    #[test]
    fn test_run_rescues_dynamic_child_before_draining() {
        let (tmp, store) = make_store();
        let parent_id = MoleculeId::new("task-20260610-paaa").unwrap();
        let child_id = MoleculeId::new("task-20260610-cbbb").unwrap();

        // Parent already Completed and merged, but carries NO forward
        // `Blocks` link to the child — exactly the asymmetric shape a
        // mission-controller leaves when it nucleates `--blocked-by`.
        let mut parent = sample_mol("task-20260610-paaa", MoleculeStatus::Completed, Vec::new());
        parent.merged_at = Some(chrono::Utc::now());
        // Child is Pending and runnable: its only blocker (parent) is
        // Completed + merged. It is invisible to compile_plan's forward BFS.
        let child = sample_mol(
            "task-20260610-cbbb",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: parent_id.clone(),
            }],
        );
        store.save_molecule(&parent.id, &parent).unwrap();
        store.save_molecule(&child.id, &child).unwrap();

        // Compile the plan from the parent root only — the child is out of
        // scope at compile time, reproducing the frozen-scope condition.
        let (plan, edges) = compile_plan(&store, std::slice::from_ref(&parent_id)).unwrap();
        let policy = DagPolicy::new(plan, edges);
        let config = RuntimeConfig {
            poll_interval: Duration::from_millis(10),
            max_runtime: Some(Duration::from_secs(2)),
            // Sweep disabled (the `cs run` default) — only the drain-time
            // rescue can surface the child.
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        };
        let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
        let mut runtime = Runtime::new(store_box, Box::new(policy), Box::new(NoOpExecutor), config);

        let report = runtime.run().unwrap();

        // Pre-fix: the runtime returned PolicyDrained on tick 1 with the
        // child left Pending. Post-fix: the rescue dispatches the child, it
        // becomes Running, and the loop survives until the deadline.
        assert_eq!(
            report.reason,
            ShutdownReason::Deadline,
            "runtime must NOT drain while a dynamically-unblocked child is runnable"
        );
        let child_after = store.load_molecule(&child_id).unwrap();
        assert_eq!(
            child_after.status,
            MoleculeStatus::Running,
            "rescue pass should have dispatched the out-of-scope child"
        );
    }

    #[test]
    fn test_run_noop_policy_exits_immediately() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260410-noop", MoleculeStatus::Pending, Vec::new());
        store.save_molecule(&mol.id, &mol).unwrap();

        let config = RuntimeConfig {
            poll_interval: Duration::from_millis(10),
            max_runtime: Some(Duration::from_secs(5)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        };
        let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
        let mut runtime = Runtime::new(
            store_box,
            Box::new(NoOpPolicy),
            Box::new(NoOpExecutor),
            config,
        );

        let report = runtime.run().unwrap();
        assert_eq!(report.reason, ShutdownReason::PolicyDrained);
        assert_eq!(report.ticks, 1);
        assert_eq!(report.actions_applied, 0);

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Pending);
    }

    #[test]
    fn test_run_linear_chain_completes_all() {
        let (tmp, store) = make_store();
        let a_id = MoleculeId::new("task-20260410-aaaa").unwrap();
        let b_id = MoleculeId::new("task-20260410-bbbb").unwrap();
        let c_id = MoleculeId::new("task-20260410-cccc").unwrap();

        let a = sample_mol(
            "task-20260410-aaaa",
            MoleculeStatus::Pending,
            vec![MoleculeLink::Blocks {
                target: b_id.clone(),
            }],
        );
        let b = sample_mol(
            "task-20260410-bbbb",
            MoleculeStatus::Pending,
            vec![
                MoleculeLink::BlockedBy {
                    source: a_id.clone(),
                },
                MoleculeLink::Blocks {
                    target: c_id.clone(),
                },
            ],
        );
        let c = sample_mol(
            "task-20260410-cccc",
            MoleculeStatus::Pending,
            vec![MoleculeLink::BlockedBy {
                source: b_id.clone(),
            }],
        );

        store.save_molecule(&a.id, &a).unwrap();
        store.save_molecule(&b.id, &b).unwrap();
        store.save_molecule(&c.id, &c).unwrap();

        let (plan, edges) = compile_plan(&store, std::slice::from_ref(&a_id)).unwrap();
        let policy = DagPolicy::new(plan, edges);
        let config = RuntimeConfig {
            poll_interval: Duration::from_millis(10),
            max_runtime: Some(Duration::from_secs(5)),
            sweep_orphan_descendants_every: None,
            liveness_recheck_every: None,
        };
        let store_box: Box<dyn StateStore> = Box::new(FileStore::new(tmp.path()));
        let mut runtime = Runtime::new(store_box, Box::new(policy), Box::new(NoOpExecutor), config);

        let report = runtime.run().unwrap();
        assert_eq!(report.reason, ShutdownReason::Deadline);
        assert_eq!(
            report.actions_applied, 1,
            "should have dispatched only root A"
        );

        let a_mol = store.load_molecule(&a_id).unwrap();
        assert_eq!(a_mol.status, MoleculeStatus::Running, "A should be running");
        for id in &[b_id, c_id] {
            let mol = store.load_molecule(id).unwrap();
            assert_eq!(
                mol.status,
                MoleculeStatus::Pending,
                "{id} should still be pending (blocked)"
            );
        }
    }

    #[test]
    fn test_resolve_molecule_id_exact() {
        let (_tmp, store) = make_store();
        let mol = sample_mol("task-20260410-resv", MoleculeStatus::Pending, Vec::new());
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule_id(&store, "task-20260410-resv").unwrap();
        assert_eq!(resolved.as_str(), "task-20260410-resv");
    }

    #[test]
    fn test_resolve_molecule_id_prefix() {
        let (_tmp, store) = make_store();
        let mol = sample_mol("task-20260410-pfxm", MoleculeStatus::Pending, Vec::new());
        store.save_molecule(&mol.id, &mol).unwrap();

        let resolved = resolve_molecule_id(&store, "task-20260410-pfx").unwrap();
        assert_eq!(resolved.as_str(), "task-20260410-pfxm");
    }

    #[test]
    fn test_resolve_molecule_id_not_found() {
        let (_tmp, store) = make_store();
        let err = resolve_molecule_id(&store, "nonexistent").unwrap_err();
        assert!(err.to_string().contains("no molecule"));
    }
}
