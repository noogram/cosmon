// SPDX-License-Identifier: AGPL-3.0-only

//! Project pulse command — `cs status`.
//!
//! Shows a quick DAG overview of the project state, like `git status` for
//! agent orchestration. Two modes:
//!
//! - **Compact** (default): one-line summary with attention bar
//! - **Verbose** (`--verbose`): full dashboard with molecules, sessions,
//!   contributions, and surfaces
//!
//! # Staleness is the point
//!
//! A pulse made only of totals is a gauge nobody reads. Every number in the
//! old line was true and none of them said what needed doing: a molecule sat
//! `pending` for 39 days behind a bare `4 alive`, `surfaces ✅` printed beside
//! a 19-day-old reconcile, and `28 🔀 to merge` had been `20` the same
//! afternoon with nothing to say so. So the line now carries three
//! derivatives as well as the levels:
//!
//! - **Age** — the oldest backlog molecule and how many are past the
//!   threshold, from [`cosmon_core::staleness`], the same arithmetic
//!   `cs peek` renders in its vitals line. One definition, two surfaces.
//! - **Reconcile freshness** — its own signal, never folded into the surface
//!   tick. The tick answers "do the projected files still match their
//!   hashes"; it has never had anything to say about *when* they were
//!   projected, and a reader must not be able to take it for that.
//! - **Unmerged growth** — a delta against a sample kept in
//!   `<state>/status-gauge.json`, refreshed at most hourly so the window
//!   stays wide enough to mean something.
//!
//! # A lease is not backlog
//!
//! A molecule named by the pilot-lease ledger carries the cockpit between
//! sessions and converges by design never. It is excluded from every
//! rendered backlog counter and reported on its own (`+1 lease`), because a
//! number that is permanently wrong by one teaches the reader to discount it.
//! `--json` keeps `molecules.alive` meaning exactly what it always meant and
//! gains `molecules.alive_excluding_leases`, `molecules.leases` and the
//! `backlog` block: an existing key never changes under a caller.
//!
//! # A dead worker is not alive
//!
//! A `Running` molecule whose worker session is gone is residue, not work in
//! progress. The classification is [`super::ensemble::molecule_ghost`], the
//! function behind `cs ensemble`'s ghost column, so the two commands cannot
//! disagree. The headline shows `N 💀 dead` instead of counting the molecule
//! among the alive, names it with `cs purge <worker-id>`, and `clean` is false
//! while one exists. `--json` keeps `molecules.alive` and adds
//! `molecules.alive_healthy`, `molecules.dead_workers`, `molecules.dead` and
//! `hygiene.dead_workers`.
//!
//! # `cs status <id>` — one molecule
//!
//! With a molecule id, `status` answers about *that* molecule instead of the
//! DAG: its status, phase, `updated_at` and whether it is terminal, read from
//! `state.json` alone. `git status` addresses a path the same way, and the
//! reading is the same one either way — "how is this doing".
//!
//! It is the local twin of `GET /v1/molecules/{id}/status`, which exists so a
//! remote client can implement `wait` by polling something cheap. Both project
//! the same [`cosmon_state::ops::molecule_status()`] verb, so the two surfaces
//! cannot answer differently — the CLI/UI parity the audit tracks.
//!
//! Why not `cs observe <id>`, which already prints a status? Because `observe`
//! is the *full* read — a coupling report, the token totals, the model
//! attribution, each a scan of a log that grows with the molecule's life. That
//! is the right answer for a human looking once and the wrong one for anything
//! asking repeatedly.

use std::collections::{BTreeMap, HashMap};

use colored::Colorize;
use cosmon_core::id::MoleculeId;
use cosmon_core::kill_switch::{Autonomous, KillSwitch};
use cosmon_core::kind::MoleculeKind;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_core::staleness::{self, BacklogAge, BacklogItem};
use cosmon_core::transport::TransportBackend;
use cosmon_state::MoleculeFilter;

use super::Context;

/// Arguments for the `status` subcommand.
#[derive(clap::Args)]
pub struct Args {
    // One-paragraph doc on purpose: a second one flips clap's whole page
    // to the long layout and every sibling option loses its compact form.
    // The rationale for "exact id, never a prefix" is in the `NOTE:` of
    // `examples::STATUS`, where it is read once rather than per-argument.
    /// Molecule id — without it, the DAG-wide pulse; with it, that one molecule
    pub molecule: Option<String>,
}

/// JSON output structure for `cs status --json`.
#[derive(serde::Serialize)]
struct StatusOutput {
    molecules: MoleculeCounts,
    sessions: SessionSummary,
    contributions: Vec<ContributionInfo>,
    /// Level **and** derivative for the unmerged-branch count.
    unmerged: UnmergedGauge,
    surfaces: SurfaceStatus,
    /// Age of the backlog — the signal a session needs first.
    backlog: BacklogInfo,
    /// The harvest queue — `Completed`, un-archived molecules whose work
    /// exists only on `feat/<id>` until `cs done` or `cs collapse` runs.
    harvestable: HarvestableInfo,
    /// Pending dependents held by a blocker absent from the resident view.
    missing_blockers: Vec<MissingBlocker>,
    attention: AttentionInfo,
    /// Four-family taxonomy snapshot.
    /// Keyed by kind token (`infra | project | social-hub | editorial
    /// | nascent`) to totals, plus a flat `total` and `nascent`.
    galaxies: GalaxiesSummary,
    /// Issue #97 — whether finishing a mission left unresolved residue.
    /// Active molecule branches and branches retained with a lifecycle audit
    /// disposition are visible but do not make this false.
    hygiene: HygieneInfo,
    /// Issue #108 — every kill-switch file under `~/.cosmon/`, whether it is
    /// present, and which autonomous components it stops. Listed even when
    /// inactive so a reader learns the whole catalogue from one call.
    kill_switches: Vec<KillSwitchInfo>,
}

/// One kill-switch as `cs status --json` reports it.
#[derive(serde::Serialize)]
struct KillSwitchInfo {
    /// File name under `~/.cosmon/`.
    file: &'static str,
    /// Whether the file exists now.
    active: bool,
    /// The components it stops, by [`Autonomous::label`].
    stops: Vec<&'static str>,
}

/// The kill-switch catalogue with the live presence of each file.
fn kill_switch_infos(active: &[KillSwitch]) -> Vec<KillSwitchInfo> {
    KillSwitch::ALL
        .into_iter()
        .map(|s| KillSwitchInfo {
            file: s.file_name(),
            active: active.contains(&s),
            stops: s.scope().into_iter().map(Autonomous::label).collect(),
        })
        .collect()
}

/// The human line naming the active switches, or `None` when none is set —
/// a quiet fleet prints nothing extra.
fn render_kill_switch_line(active: &[KillSwitch]) -> Option<String> {
    if active.is_empty() {
        return None;
    }
    let files: Vec<String> = active
        .iter()
        .map(|s| format!("~/.cosmon/{}", s.file_name()))
        .collect();
    let note = if active.contains(&KillSwitch::StandDown) {
        "all autonomous activity stopped"
    } else {
        "scoped stop"
    };
    Some(format!("kill-switch: {} — {note}", files.join(", ")))
}

/// Whether `cs status` is clean, as defined by issue #97 and #172: no zombie
/// session, no dead worker, no un-harvested `Completed` molecule, and no
/// unresolved molecule branch.
/// Active work and branches retained with a recorded disposition remain
/// visible without being mislabeled as residue.
#[derive(serde::Serialize)]
struct HygieneInfo {
    clean: bool,
    zombie_sessions: usize,
    /// Issue #172 — `Running` molecules whose worker session is gone. A dead
    /// worker is residue: nothing is working, and nothing will until it is
    /// reclaimed with `cs purge`.
    dead_workers: usize,
    harvestable: usize,
    /// Backward-compatible key: now counts unresolved branches, rather than
    /// every live or deliberately retained molecule branch.
    unmerged_branches: usize,
    active_branches: usize,
    retained_audited_branches: usize,
}

impl HygieneInfo {
    /// Build the single clean predicate from unresolved residue only.
    fn new(
        zombie_sessions: usize,
        dead_workers: usize,
        harvestable: usize,
        unmerged_branches: usize,
        active_branches: usize,
        retained_audited_branches: usize,
    ) -> Self {
        Self {
            clean: zombie_sessions == 0
                && dead_workers == 0
                && harvestable == 0
                && unmerged_branches == 0,
            zombie_sessions,
            dead_workers,
            harvestable,
            unmerged_branches,
            active_branches,
            retained_audited_branches,
        }
    }
}

/// Per-kind galaxy totals as emitted inside `cs status --json`.
#[derive(serde::Serialize)]
struct GalaxiesSummary {
    /// Kind → count. Missing entries mean zero — callers should not
    /// rely on every token being present.
    pub by_kind: BTreeMap<String, usize>,
    /// Total galaxies known to the registry (sum of `by_kind`).
    pub total: usize,
    /// Galaxies with `NULL` `galaxy_kind`. Equal to `by_kind["nascent"]`
    /// when present; surfaced as its own field so dashboards do not
    /// have to second-guess the key ordering.
    pub nascent: usize,
}

#[derive(serde::Serialize)]
struct MoleculeCounts {
    /// Every non-terminal molecule, leases included. Unchanged meaning —
    /// a caller that has been reading this key keeps reading the same fact.
    alive: usize,
    /// The same count with pilot-lease missions removed. This is the number
    /// the rendered line shows, because a lease is not work waiting.
    alive_excluding_leases: usize,
    /// Non-terminal molecules named by the pilot-lease ledger.
    leases: usize,
    /// Issue #172 — `alive_excluding_leases` minus the dead workers: the
    /// molecules a live session is actually carrying. `alive` is unchanged.
    alive_healthy: usize,
    /// Issue #172 — `Running` molecules whose worker session is gone, from
    /// the same ghost classification `cs ensemble` uses.
    dead_workers: usize,
    /// The dead workers by name, with their reclaim gesture's target.
    dead: Vec<DeadWorker>,
    completed: usize,
    collapsed: usize,
    by_kind: HashMap<String, usize>,
    by_status: HashMap<String, usize>,
}

/// A worker whose session is gone while its molecule still reads `Running`
/// (issue #172): a [`cosmon_core::run_state::GhostKind::DeadPane`] or
/// `VanishedWorker`, classified by the same function `cs ensemble` uses.
#[derive(serde::Serialize, Clone)]
struct DeadWorker {
    worker: String,
    molecule: String,
}

/// Backlog age, as `cs status --json` emits it.
///
/// Projects [`cosmon_core::staleness::BacklogAge`] onto the wire. Seconds
/// rather than a rendered `39d`, because a formatted duration is a fact about
/// when it was formatted and a machine reader wants to compare.
#[derive(serde::Serialize)]
struct BacklogInfo {
    /// `Pending` molecules that are not leases.
    count: usize,
    /// Of those, how many are older than `stale_after_hours`.
    stale: usize,
    /// The threshold, emitted so a dashboard does not hard-code 48.
    stale_after_hours: i64,
    /// Age of the oldest one, in seconds. `None` on an empty backlog.
    oldest_age_seconds: Option<i64>,
    /// Which molecule that age belongs to, so a reader can go look.
    oldest_id: Option<String>,
    /// Lease missions skipped by `count`.
    leases_excluded: usize,
}

/// The harvest queue, as `cs status` emits it — issue #95: a molecule
/// that is `Completed` and un-archived is un-harvested work, and it must
/// never be silent. The same predicate `cs peek --phase harvestable` uses
/// ([`super::peek::PhaseFilter::is_harvestable`]), so the two surfaces
/// cannot disagree about which molecules are in the queue.
#[derive(serde::Serialize)]
struct HarvestableInfo {
    /// How many `Completed`, un-archived molecules are waiting.
    count: usize,
    /// Their ids, oldest first. Retained for callers that consumed the first
    /// #95 JSON shape; `items` adds the actionable location records.
    ids: Vec<String>,
    /// Full location and concrete harvest command for every waiting molecule.
    items: Vec<super::work_location::WorkLocation>,
}

/// A dependency the scheduler cannot clear because its source is not visible.
#[derive(serde::Serialize)]
struct MissingBlocker {
    dependent: MoleculeId,
    blocker: MoleculeId,
    reason: &'static str,
    /// The operator gesture required to replace the broken edge.
    repair: String,
}

/// Unmerged-branch level and its movement since the last sample.
///
/// `delta` is what the old line could not say. The sample lives in
/// `<state>/status-gauge.json` and is refreshed at most once an hour, so the
/// comparison window stays wide enough that a growing count is visible
/// instead of being flattened by the previous invocation a minute earlier.
#[derive(serde::Serialize)]
struct UnmergedGauge {
    /// Unresolved molecule branches (`feat/<id>`) not merged into the trunk
    /// and ahead of it. Active and retained-audited branches are visible in
    /// `contributions` but excluded from this actionable gauge. Operator refs
    /// are excluded entirely — see [`is_molecule_branch`].
    branches: usize,
    /// Total commits those branches carry ahead of the trunk.
    commits: usize,
    /// Branch count at the last sample, or `None` on the first run.
    previous_branches: Option<usize>,
    /// `branches - previous_branches`. `None` on the first run.
    delta: Option<i64>,
    /// Age of the sample `delta` is measured against, in seconds.
    since_seconds: Option<i64>,
}

#[derive(serde::Serialize)]
struct SessionSummary {
    active: Vec<SessionInfo>,
    zombies: Vec<SessionInfo>,
}

#[derive(serde::Serialize)]
struct SessionInfo {
    worker: String,
    molecule: String,
    status: String,
}

#[derive(serde::Serialize)]
struct ContributionInfo {
    /// Always a molecule branch (`feat/<id>`) — see [`is_molecule_branch`].
    branch: String,
    commits_ahead: usize,
    molecule: String,
    disposition: BranchDisposition,
    detail: String,
}

/// The lifecycle meaning of an unmerged molecule branch.
///
/// This is a projection of the existing molecule record, not a second audit
/// store: live status means work in progress, while `Collapsed` and an
/// explicit `merge-skipped` harvest are durable dispositions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum BranchDisposition {
    Active,
    RetainedAudited,
    Unresolved,
}

impl ContributionInfo {
    /// Whether this branch still needs a pilot decision.
    fn is_unresolved(&self) -> bool {
        self.disposition == BranchDisposition::Unresolved
    }
}

#[derive(serde::Serialize)]
struct SurfaceStatus {
    /// Do the projected files on disk still match their recorded hashes?
    ///
    /// This and nothing else. It was never a statement about *when* the
    /// projection happened, which is why `reconcile_stale` is a separate
    /// field rather than folded in here: a caller reading `up_to_date` today
    /// keeps getting the answer it has always got.
    up_to_date: bool,
    last_reconcile: Option<String>,
    stale_count: usize,
    /// How long ago the most recent projection ran, in seconds. `None` when
    /// nothing has ever been projected.
    reconcile_age_seconds: Option<i64>,
    /// True when that age is past [`cosmon_core::staleness::stale_reconcile_after`],
    /// or when there is no projection at all. Absence is not freshness.
    reconcile_stale: bool,
    /// Whether any surface has ever been projected. Distinguishes "clean"
    /// from "never ran", which the tick alone could not.
    projected: bool,
}

#[derive(serde::Serialize)]
struct AttentionInfo {
    alive: usize,
    budget: Option<usize>,
    percent: Option<f64>,
}

/// Execute the `status` command.
///
/// # Errors
///
/// Surfaces a malformed molecule id, an unknown molecule, and any state-store
/// read failure.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    if let Some(id) = args.molecule.as_deref() {
        return run_one(ctx, id);
    }
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let fleet = store.load_fleet()?;
    let molecules = store.list_molecules(&MoleculeFilter::default())?;
    let resident_view = if let Some(project_id) = super::ensemble::configured_project_id(ctx) {
        let scoped = molecules
            .iter()
            .filter(|m| m.project_id.as_ref() == Some(&project_id))
            .cloned()
            .collect();
        super::ensemble::include_referenced_legacy_blockers(scoped, &molecules)
    } else {
        molecules.clone()
    };
    let missing_blockers = missing_blockers(&resident_view, &molecules);

    // Which molecules carry a pilot lease. Read once, from the ledger
    // directory, so no id is hard-coded anywhere: the live instance
    // announces itself by having a ledger, and a future one will too.
    let leases = lease_missions(&state_dir);

    // --- Molecule counts ---
    let alive = molecules.iter().filter(|m| m.status.is_alive()).count();
    let lease_alive = molecules
        .iter()
        .filter(|m| m.status.is_alive() && leases.contains(&m.id))
        .count();
    let alive_excluding_leases = alive.saturating_sub(lease_alive);

    // --- Backlog age (shared with `cs peek`) ---
    let backlog = staleness::backlog_age(
        molecules.iter().map(|m| BacklogItem {
            id: m.id.clone(),
            status: m.status,
            created_at: Some(m.created_at),
            is_lease: leases.contains(&m.id),
        }),
        chrono::Utc::now(),
    );
    let completed = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Completed)
        .count();
    let collapsed = molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Collapsed)
        .count();

    // --- Harvest queue (issue #95) — same predicate `cs peek
    // --phase harvestable` uses, so the two views cannot disagree.
    let mut harvestable_mols: Vec<_> = molecules
        .iter()
        .filter(|m| super::peek::PhaseFilter::is_harvestable(m.status, m.archived))
        .collect();
    harvestable_mols.sort_by_key(|m| m.updated_at);
    let repo_root = super::work_location::repo_root(ctx);
    let items = harvestable_mols
        .iter()
        .map(|m| super::work_location::WorkLocation::from_state(m, &fleet, &repo_root))
        .collect();
    let harvestable = HarvestableInfo {
        count: harvestable_mols.len(),
        ids: harvestable_mols.iter().map(|m| m.id.to_string()).collect(),
        items,
    };

    let project_socket = super::tmux_socket_name(ctx);
    let backends = discover_fleet_backends(&state_dir, &project_socket);

    // --- Dead workers (issue #172) — a Running molecule whose session is
    // gone. Classified by `ensemble::molecule_ghost`, the function behind the
    // ensemble's ghost column, so the two commands cannot disagree.
    let dead_workers = dead_workers(&fleet, &molecules, &backends);

    // Leases are left out of the kind breakdown for the same reason they are
    // left out of the alive count it sits beside: the two must add up, and a
    // lease is not one of the things the reader is being asked to drain.
    let mut by_kind: HashMap<MoleculeKind, usize> = HashMap::new();
    for mol in &molecules {
        let kind = mol.kind.unwrap_or(MoleculeKind::Task);
        if mol.status.is_alive()
            && !leases.contains(&mol.id)
            && !dead_workers.iter().any(|d| d.molecule == mol.id.as_str())
        {
            *by_kind.entry(kind).or_default() += 1;
        }
    }

    let mut by_status: HashMap<MoleculeStatus, usize> = HashMap::new();
    for mol in &molecules {
        *by_status.entry(mol.status).or_default() += 1;
    }

    // --- Sessions (tmux) ---
    let live_sessions = discover_live_sessions(&backends);

    let mut active_sessions: Vec<SessionInfo> = Vec::new();
    let mut zombie_sessions: Vec<SessionInfo> = Vec::new();

    for (worker_name, _session_name) in &live_sessions {
        // Find the worker in fleet state
        let worker_data = fleet
            .workers
            .values()
            .find(|w| w.id.as_str() == worker_name);
        let mol_id = worker_data.and_then(|w| w.current_molecule.as_ref());
        let mol_status = mol_id.and_then(|mid| molecules.iter().find(|m| m.id == *mid));

        let mol_display = mol_id.map_or_else(|| "-".to_owned(), ToString::to_string);

        // Zombie: tmux session exists but molecule is terminal or missing
        let is_zombie = match mol_status {
            Some(m) => m.status.is_terminal(),
            None => mol_id.is_some(), // assigned but molecule not found
        };

        let info = SessionInfo {
            worker: worker_name.clone(),
            molecule: mol_display,
            status: if is_zombie {
                "zombie".to_owned()
            } else {
                "active".to_owned()
            },
        };

        if is_zombie {
            zombie_sessions.push(info);
        } else {
            active_sessions.push(info);
        }
    }

    // --- Contributions (git branches) ---
    let contributions = discover_contributions(&molecules);
    let unmerged = sample_unmerged_gauge(&state_dir, &contributions);

    // --- Surfaces ---
    let surface_status = check_surfaces(&state_dir);

    // --- Galaxies (neurion-backed) ---
    // Failure to read the neurion DB is non-fatal: pulse must never
    // depend on a sibling component being booted. Absent data shows
    // as total=0 rather than an error.
    let galaxies = load_galaxies_summary().unwrap_or_else(|_| GalaxiesSummary {
        by_kind: BTreeMap::new(),
        total: 0,
        nascent: 0,
    });

    // --- Attention ---
    let budget = fleet.attention_budget;
    let attention_percent = budget.map(|b| {
        if b == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let pct = (alive as f64 / b as f64) * 100.0;
            pct
        }
    });

    // --- Hygiene (issue #97) — whether finishing left residue behind.
    let active_branches = contributions
        .iter()
        .filter(|c| c.disposition == BranchDisposition::Active)
        .count();
    let retained_audited_branches = contributions
        .iter()
        .filter(|c| c.disposition == BranchDisposition::RetainedAudited)
        .count();
    let hygiene = HygieneInfo::new(
        zombie_sessions.len(),
        dead_workers.len(),
        harvestable.count,
        unmerged.branches,
        active_branches,
        retained_audited_branches,
    );

    // --- Kill switches (issue #108) — which stop controls are laid down.
    let active_switches = cosmon_cli::kill_switches::active();

    // --- Output ---
    if ctx.json {
        let output = StatusOutput {
            molecules: MoleculeCounts {
                alive,
                alive_excluding_leases,
                leases: lease_alive,
                alive_healthy: alive_excluding_leases.saturating_sub(dead_workers.len()),
                dead_workers: dead_workers.len(),
                dead: dead_workers.clone(),
                completed,
                collapsed,
                by_kind: by_kind.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                by_status: by_status.iter().map(|(s, v)| (s.to_string(), *v)).collect(),
            },
            sessions: SessionSummary {
                active: active_sessions,
                zombies: zombie_sessions,
            },
            contributions,
            unmerged,
            surfaces: surface_status,
            backlog: BacklogInfo {
                count: backlog.counted,
                stale: backlog.stale,
                stale_after_hours: staleness::stale_backlog_after().num_hours(),
                oldest_age_seconds: backlog.oldest.map(|d| d.num_seconds()),
                oldest_id: backlog.oldest_id.as_ref().map(ToString::to_string),
                leases_excluded: backlog.leases_excluded,
            },
            harvestable,
            missing_blockers,
            attention: AttentionInfo {
                alive,
                budget,
                percent: attention_percent,
            },
            galaxies,
            hygiene,
            kill_switches: kill_switch_infos(&active_switches),
        };
        let json = serde_json::to_string_pretty(&output)?;
        println!("{json}");
        return Ok(());
    }

    let pulse = Pulse {
        alive: alive_excluding_leases.saturating_sub(dead_workers.len()),
        dead_workers: &dead_workers,
        lease_alive,
        completed,
        collapsed,
        backlog: &backlog,
        harvestable: &harvestable,
        by_kind: &by_kind,
        active_sessions: &active_sessions,
        zombie_sessions: &zombie_sessions,
        contributions: &contributions,
        unmerged: &unmerged,
        surfaces: &surface_status,
        budget,
        attention_percent,
        galaxies: &galaxies,
        hygiene: &hygiene,
    };

    if ctx.verbose {
        render_verbose(&pulse);
    } else {
        render_compact(&pulse);
    }
    if let Some(line) = render_kill_switch_line(&active_switches) {
        println!("{line}");
    }
    for (dependent, blocker, status) in held_dependents(&molecules) {
        println!(
            "  Blocked: {dependent} waits for {blocker} ({status}); {}",
            release_guidance(status, &dependent, &blocker)
        );
    }
    for missing in &missing_blockers {
        println!(
            "  Blocked: {} waits for {} ({}); {}",
            missing.dependent, missing.blocker, missing.reason, missing.repair
        );
    }

    Ok(())
}

/// Find predecessors absent from the resident view, without releasing edges.
fn missing_blockers(
    molecules: &[cosmon_state::MoleculeData],
    all: &[cosmon_state::MoleculeData],
) -> Vec<MissingBlocker> {
    let by_id: std::collections::HashSet<_> = molecules.iter().map(|m| m.id.clone()).collect();
    let all_ids: std::collections::HashSet<_> = all.iter().map(|m| m.id.clone()).collect();
    let mut missing = Vec::new();
    for dependent in molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Pending)
    {
        for blocker in dependent.blocked_by() {
            if !by_id.contains(blocker) {
                missing.push(MissingBlocker {
                    dependent: dependent.id.clone(),
                    blocker: blocker.clone(),
                    reason: if all_ids.contains(blocker) {
                        "outside current project"
                    } else {
                        "missing molecule"
                    },
                    repair: format!(
                        "run `cs collapse {} --reason <reason>`, then re-nucleate it with --blocked-by <valid-blocker> or without the edge",
                        dependent.id
                    ),
                });
            }
        }
    }
    missing.sort_by(|left, right| {
        (&left.dependent, &left.blocker).cmp(&(&right.dependent, &right.blocker))
    });
    missing
}

/// Pending dependents whose named blocker has frozen or collapsed.
fn held_dependents(
    molecules: &[cosmon_state::MoleculeData],
) -> Vec<(MoleculeId, MoleculeId, MoleculeStatus)> {
    let by_id: HashMap<_, _> = molecules.iter().map(|m| (&m.id, m.status)).collect();
    let mut held = Vec::new();
    for dependent in molecules
        .iter()
        .filter(|m| m.status == MoleculeStatus::Pending)
    {
        for blocker in dependent.blocked_by() {
            if let Some(status @ (MoleculeStatus::Collapsed | MoleculeStatus::Frozen)) =
                by_id.get(blocker).copied()
            {
                held.push((dependent.id.clone(), blocker.clone(), status));
            }
        }
    }
    held.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    held
}

/// Name the operator action that can satisfy or replace a failed dependency.
fn release_guidance(
    status: MoleculeStatus,
    dependent: &MoleculeId,
    blocker: &MoleculeId,
) -> String {
    if status == MoleculeStatus::Frozen {
        format!(
            "finish {blocker}, then `cs complete {blocker} --reason <evidence>` and `cs done {blocker}` to release {dependent}"
        )
    } else {
        format!("collapse {dependent} and re-nucleate it with --blocked-by <completed-blocker>")
    }
}

/// Everything the two renderers read, gathered once.
///
/// A struct rather than a thirteenth positional argument: the renderers
/// differ in layout, not in inputs, and a shared bundle is what keeps them
/// from drifting into showing different facts.
struct Pulse<'a> {
    /// Alive molecules a live session carries: leases and dead workers
    /// already removed.
    alive: usize,
    /// Issue #172 — workers whose session is gone.
    dead_workers: &'a [DeadWorker],
    /// Alive lease missions, reported separately.
    lease_alive: usize,
    completed: usize,
    collapsed: usize,
    backlog: &'a BacklogAge,
    harvestable: &'a HarvestableInfo,
    by_kind: &'a HashMap<MoleculeKind, usize>,
    active_sessions: &'a [SessionInfo],
    zombie_sessions: &'a [SessionInfo],
    contributions: &'a [ContributionInfo],
    unmerged: &'a UnmergedGauge,
    surfaces: &'a SurfaceStatus,
    budget: Option<usize>,
    attention_percent: Option<f64>,
    galaxies: &'a GalaxiesSummary,
    hygiene: &'a HygieneInfo,
}

/// The age token the compact and verbose lines both show, or `None` when
/// there is no backlog to be old.
///
/// One function so the two renderers cannot disagree about what "oldest"
/// means, in the same spirit as the shared predicate underneath it.
fn render_backlog_age(backlog: &BacklogAge) -> Option<String> {
    let oldest = backlog.oldest?;
    let threshold = staleness::stale_backlog_after().num_hours();
    let age = staleness::format_age(oldest);
    let body = if backlog.stale > 0 {
        format!("oldest {age} \u{b7} {} >{threshold}h", backlog.stale)
    } else {
        format!("oldest {age}")
    };
    Some(if backlog.stale > 0 {
        body.yellow().to_string()
    } else {
        body
    })
}

/// The surfaces token — a tick that asserts only what it checked.
///
/// The tick is about content hashes and has never said anything about
/// reconcile age, so the age travels beside it rather than inside it. The
/// three states a reader must be able to tell apart: projected and fresh,
/// projected long ago, never projected at all. The old line rendered all
/// three as `✅`.
fn render_surfaces_token(surfaces: &SurfaceStatus) -> String {
    if !surfaces.projected {
        return "\u{2014} never reconciled".yellow().to_string();
    }
    if !surfaces.up_to_date {
        return format!("\u{26A0}\u{FE0F} {} stale", surfaces.stale_count)
            .yellow()
            .to_string();
    }
    let age = surfaces
        .reconcile_age_seconds
        .map(|s| staleness::format_age(chrono::Duration::seconds(s)));
    match age {
        Some(age) if surfaces.reconcile_stale => format!("\u{26A0}\u{FE0F} reconciled {age} ago")
            .yellow()
            .to_string(),
        Some(age) => format!("\u{2705} reconciled {age} ago"),
        None => "\u{2705}".to_owned(),
    }
}

/// The harvest-queue guidance line — issue #95: a `Completed`, un-archived
/// molecule is work that exists only on `feat/<id>` until `cs done` or
/// `cs collapse` runs, and it must never be silent. `None` when the queue
/// is empty, the same convention [`render_unmerged_token`] uses for a gauge
/// with nothing to report.
///
/// Every item gets its own line: a count plus generic verbs is not a location.
fn render_harvestable_lines(h: &HarvestableInfo) -> Vec<String> {
    if h.count == 0 {
        return Vec::new();
    }
    h.items
        .iter()
        .map(|item| {
            format!(
                "  {} {} — {}; or `cs collapse {} --reason \"…\"` to record abandonment \
                 (branch retained for audit)",
                "Harvest".bold(),
                item.molecule,
                item.render(),
                item.molecule
            )
        })
        .collect()
}

/// The unmerged-branch token, level and movement.
///
/// Returns `None` when there is nothing unmerged — an empty gauge printed
/// every run is how a real one stops being read.
fn render_unmerged_token(unmerged: &UnmergedGauge) -> Option<String> {
    if unmerged.branches == 0 {
        return None;
    }
    let mut token = format!("{}\u{1F500} branch residue", unmerged.branches);
    if let (Some(delta), Some(since)) = (unmerged.delta, unmerged.since_seconds) {
        if delta != 0 {
            let window = staleness::format_age(chrono::Duration::seconds(since));
            let sign = if delta > 0 { "+" } else { "" };
            let movement = format!(" ({sign}{delta} in {window})");
            token.push_str(&if delta > 0 {
                movement.yellow().to_string()
            } else {
                movement
            });
        }
    }
    Some(token)
}

/// Human-readable branch rows. Every unresolved branch is named, active work
/// is explicitly non-residue, and retained branches show the audit
/// disposition already recorded on the molecule.
fn render_contribution_lines(contributions: &[ContributionInfo]) -> Vec<String> {
    contributions
        .iter()
        .map(|item| match item.disposition {
            BranchDisposition::Active => format!(
                "  {} {} — active ({}) · not residue",
                "Branch".bold(),
                item.branch,
                item.detail
            ),
            BranchDisposition::RetainedAudited => format!(
                "  {} {} — retained · audit disposition: {}",
                "Branch".bold(),
                item.branch,
                item.detail
            ),
            BranchDisposition::Unresolved => format!(
                "  {} {} — {}; run `cs done {}` after resolving it",
                "Resolve".bold(),
                item.branch,
                item.detail,
                item.molecule
            ),
        })
        .collect()
}

/// Render compact one-line status.
fn render_compact(p: &Pulse) {
    // Line 1: molecule summary
    let mut parts: Vec<String> = Vec::new();

    // Kind breakdown (sorted for stable output)
    let mut kind_parts: Vec<String> = Vec::new();
    for kind in &[
        MoleculeKind::Idea,
        MoleculeKind::Task,
        MoleculeKind::Issue,
        MoleculeKind::Decision,
        MoleculeKind::Signal,
    ] {
        if let Some(&count) = p.by_kind.get(kind) {
            if count > 0 {
                kind_parts.push(format!("{}{}", count, kind.emoji()));
            }
        }
    }

    let kind_str = if kind_parts.is_empty() {
        String::new()
    } else {
        format!(": {}", kind_parts.join(" "))
    };
    let lease_str = if p.lease_alive > 0 {
        format!(" (+{} lease)", p.lease_alive)
    } else {
        String::new()
    };
    parts.push(format!("{} alive{kind_str}{lease_str}", p.alive));

    // Backlog age — the signal a session opening needs first.
    if let Some(age) = render_backlog_age(p.backlog) {
        parts.push(age);
    }

    // Dead workers (issue #172) — never counted among the alive.
    if !p.dead_workers.is_empty() {
        parts.push(
            format!("{}\u{1F480} dead", p.dead_workers.len())
                .red()
                .bold()
                .to_string(),
        );
    }

    // Sessions
    if !p.active_sessions.is_empty() {
        parts.push(format!(
            "{}{}active",
            p.active_sessions.len(),
            MoleculeStatus::Running.emoji()
        ));
    }
    if !p.zombie_sessions.is_empty() {
        parts.push(
            format!(
                "{}{}zombie",
                p.zombie_sessions.len(),
                " \u{1F480} " // skull emoji
            )
            .red()
            .to_string(),
        );
    }

    // Contributions, with their movement
    if let Some(token) = render_unmerged_token(p.unmerged) {
        parts.push(token);
    }

    // Harvest queue — un-harvested work must never be a silent count.
    if p.harvestable.count > 0 {
        parts.push(
            format!("{}\u{1F33E} harvest", p.harvestable.count) // sheaf-of-rice emoji
                .yellow()
                .to_string(),
        );
    }

    // Surfaces
    parts.push(format!("surfaces {}", render_surfaces_token(p.surfaces)));

    // Hygiene (issue #97) — an explicit "clean" so a reader does not have
    // to infer it from the absence of the zombie/harvest/to-merge tokens
    // above; when dirty, those tokens already name what needs `cs done` or
    // `cs collapse`, so nothing more is added here.
    if p.hygiene.clean {
        parts.push("clean".green().to_string());
    }

    println!(
        "{} {}",
        "\u{1F9EA} cosmon status".bold(), // test tube emoji
        parts.join(" | ")
    );

    // Attention bar
    if let (Some(b), Some(pct)) = (p.budget, p.attention_percent) {
        println!(
            "  {}: {}/{} ({:.0}%) {}",
            "Attention".bold(),
            p.alive,
            b,
            pct,
            render_bar(pct, 20)
        );
    }

    // Galaxies one-liner — suppress entirely when the registry is
    // empty (typical for a fresh project or a session before neurion
    // has run discovery). When populated, the line is dense by design:
    // operators scan "10 galaxies: 1 infra, 7 project, 2 social-hub,
    // 1 editorial" in a single saccade.
    if p.galaxies.total > 0 {
        println!(
            "  {}: {}",
            "Galaxies".bold(),
            render_galaxies_line(p.galaxies)
        );
    }

    // Harvest queue — un-harvested work is never silent (issue #95).
    for line in render_harvestable_lines(p.harvestable) {
        println!("{}", line.yellow());
    }
    render_branch_and_zombie_lines(p);
}

/// Name every non-summary hygiene item beneath the compact pulse.
fn render_branch_and_zombie_lines(p: &Pulse<'_>) {
    for d in p.dead_workers {
        println!(
            "{}",
            format!(
                "  {} dead worker {} ({}) — run `cs purge {}`",
                "Resolve".bold(),
                d.worker,
                d.molecule,
                d.worker
            )
            .red()
        );
    }
    for s in p.zombie_sessions {
        println!(
            "{}",
            format!(
                "  {} zombie {} ({}) — run `cs purge {} --force`",
                "Resolve".bold(),
                s.worker,
                s.molecule,
                s.worker
            )
            .red()
        );
    }
    for line in render_contribution_lines(p.contributions) {
        println!("{line}");
    }
}

/// Render verbose dashboard.
#[allow(clippy::too_many_lines)]
fn render_verbose(p: &Pulse) {
    println!("{}", "\u{1F9EA} cosmon status".bold());
    println!();

    // Molecules section
    println!(
        "  {}: {} alive, {} completed, {} collapsed",
        "Molecules".bold(),
        p.alive,
        p.completed,
        p.collapsed
    );

    // Kind breakdown
    for kind in &[
        MoleculeKind::Idea,
        MoleculeKind::Task,
        MoleculeKind::Issue,
        MoleculeKind::Decision,
        MoleculeKind::Signal,
    ] {
        if let Some(&count) = p.by_kind.get(kind) {
            if count > 0 {
                println!("    {} {} {}s", kind.emoji(), count, kind);
            }
        }
    }
    if p.lease_alive > 0 {
        println!(
            "    \u{1F511} {} pilot lease(s) \u{2014} carried between sessions, not backlog",
            p.lease_alive
        );
    }

    // Backlog section — named, because "alive" is a level and this is the
    // derivative that says whether the level is moving.
    println!();
    let threshold = staleness::stale_backlog_after().num_hours();
    if let Some(oldest) = p.backlog.oldest {
        let named = p
            .backlog
            .oldest_id
            .as_ref()
            .map_or_else(String::new, |id| format!(" ({id})"));
        println!(
            "  {}: {} pending, oldest {}{named}",
            "Backlog".bold(),
            p.backlog.counted,
            staleness::format_age(oldest),
        );
        if p.backlog.stale > 0 {
            println!(
                "    {} {} past {threshold}h \u{2014} `cs ensemble --tag temp:hot` to triage",
                "\u{26A0}\u{FE0F}".yellow(),
                p.backlog.stale
            );
        }
    } else {
        println!("  {}: empty", "Backlog".bold());
    }

    // Harvest queue section — issue #95: work sitting only on `feat/<id>`
    // must be as visible as the backlog it sits beside.
    let harvest_lines = render_harvestable_lines(p.harvestable);
    if !harvest_lines.is_empty() {
        println!();
        for line in harvest_lines {
            println!("{}", line.yellow());
        }
    }

    // Sessions section
    if !p.active_sessions.is_empty() || !p.zombie_sessions.is_empty() {
        println!();
        println!("  {}:", "Sessions".bold());
        for s in p.active_sessions {
            println!(
                "    {} {} ({}) running",
                "\u{25B6}\u{FE0F}".green(), // play button
                s.worker,
                s.molecule
            );
        }
        for s in p.zombie_sessions {
            println!(
                "    \u{1F480} {} ({}) {} \u{2014} kill it",
                s.worker,
                s.molecule,
                "zombie".red().bold()
            );
        }
    }

    // Contributions section — all molecule branches stay visible, but only
    // unresolved ones count against hygiene.
    if !p.contributions.is_empty() {
        println!();
        println!(
            "  {}: {} molecule branches, {} unresolved",
            "Contributions".bold(),
            p.contributions.len(),
            p.unmerged.branches
        );
        for line in render_contribution_lines(p.contributions) {
            println!("{line}");
        }
        // A movement of zero is not news, and a gauge that prints an empty
        // accusation every run is a gauge nobody reads.
        if let (Some(delta), Some(since)) = (p.unmerged.delta, p.unmerged.since_seconds) {
            if delta != 0 {
                let window = staleness::format_age(chrono::Duration::seconds(since));
                let sign = if delta > 0 { "+" } else { "" };
                println!("    {sign}{delta} branches in the last {window}");
            }
        }
    }

    // Surfaces section
    println!();
    println!(
        "  {}: {}",
        "Surfaces".bold(),
        render_surfaces_token(p.surfaces)
    );
    if !p.surfaces.up_to_date || p.surfaces.reconcile_stale {
        println!("    run `cs reconcile`");
    }

    // Hygiene (issue #97) — an explicit verdict, not left to be inferred
    // from the sections above being empty.
    println!();
    if p.hygiene.clean {
        println!("  {}: {}", "Hygiene".bold(), "clean".green());
    } else {
        println!(
            "  {}: {} zombie session(s), {} dead worker(s), {} un-harvested, {} unresolved branch(es)",
            "Hygiene".bold(),
            p.hygiene.zombie_sessions,
            p.hygiene.dead_workers,
            p.hygiene.harvestable,
            p.hygiene.unmerged_branches
        );
    }

    // Attention bar
    if let (Some(b), Some(pct)) = (p.budget, p.attention_percent) {
        println!();
        println!(
            "  {}: {}/{} ({:.0}%) {}",
            "Attention".bold(),
            p.alive,
            b,
            pct,
            render_bar(pct, 20)
        );
    }

    // Galaxies section — shown only when neurion has real data,
    // so an empty fleet stays silent.
    if p.galaxies.total > 0 {
        println!();
        println!("  {}: {} total", "Galaxies".bold(), p.galaxies.total);
        println!("    {}", render_galaxies_line(p.galaxies));
        println!(
            "    {} to classify (see `cs galaxies list`)",
            if p.galaxies.nascent == 0 {
                "none".to_owned()
            } else {
                p.galaxies.nascent.to_string()
            }
        );
    }
}

/// Render a Unicode block progress bar.
///
/// Uses `\u{2588}` (full block) and `\u{2591}` (light shade) to build
/// a visual bar of `width` characters representing the given percentage.
fn render_bar(percent: f64, width: usize) -> String {
    let clamped = percent.clamp(0.0, 100.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let filled = ((clamped / 100.0) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);

    let bar = format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(empty));

    // Colorize based on utilization
    if percent >= 90.0 {
        bar.red().bold().to_string()
    } else if percent >= 70.0 {
        bar.yellow().to_string()
    } else {
        bar.green().to_string()
    }
}

/// Discover all live tmux sessions across fleet backends.
///
/// Returns `(worker_name, session_name)` pairs.
fn discover_live_sessions(backends: &[cosmon_transport::TmuxBackend]) -> Vec<(String, String)> {
    let mut sessions = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for be in backends {
        if let Ok(list) = be.list_sessions() {
            for info in list {
                let name = info.worker_id.as_str().to_owned();
                if seen.insert(name.clone()) {
                    sessions.push((name, info.session_name));
                }
            }
        }
    }
    sessions
}

/// True when `branch` is a molecule branch (`feat/<id>`) rather than an
/// operator ref such as `backup/*`, a release bake branch, or `spore/*`.
///
/// Issue #97: the "to merge" count previously included every local branch
/// `git branch --no-merged main` returned, so it could never reach zero by
/// harvesting alone — an operator ref is never harvested by `cs done`. A
/// molecule branch is always named `feat/<id>` (`cs done`, `cs collapse`,
/// `cs stitch` all agree on the shape), so a valid [`MoleculeId`] after the
/// `feat/` prefix is what distinguishes molecule work from everything else.
fn is_molecule_branch(branch: &str) -> bool {
    branch
        .strip_prefix("feat/")
        .is_some_and(|id| MoleculeId::new(id).is_ok())
}

/// Discover git branches not merged to main (contributions).
///
/// Scoped to molecule branches (`feat/<id>`) only — see
/// [`is_molecule_branch`]. Other unmerged refs (`backup/*`, `spore/*`,
/// release bake branches) are operator state, not molecule work, and are
/// reported nowhere by `cs status` rather than inflated into this count.
fn discover_contributions(molecules: &[cosmon_state::MoleculeData]) -> Vec<ContributionInfo> {
    let output = std::process::Command::new("git")
        .args(["branch", "--no-merged", "main", "--format=%(refname:short)"])
        .output();

    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut contributions = Vec::new();

    for branch in stdout.lines() {
        let branch = branch.trim();
        if branch.is_empty() || !is_molecule_branch(branch) {
            continue;
        }

        // Count commits ahead of main
        let ahead = std::process::Command::new("git")
            .args(["rev-list", "--count", &format!("main..{branch}")])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<usize>()
                        .ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);

        if ahead > 0 {
            let Some(molecule) = branch.strip_prefix("feat/") else {
                continue;
            };
            let (disposition, detail) = classify_branch(
                molecules
                    .iter()
                    .find(|candidate| candidate.id.as_str() == molecule),
            );
            contributions.push(ContributionInfo {
                branch: branch.to_owned(),
                commits_ahead: ahead,
                molecule: molecule.to_owned(),
                disposition,
                detail,
            });
        }
    }

    contributions
}

/// Read the disposition of a branch from the molecule's existing lifecycle
/// record. No branch-side marker or parallel audit database is introduced.
fn classify_branch(molecule: Option<&cosmon_state::MoleculeData>) -> (BranchDisposition, String) {
    let Some(molecule) = molecule else {
        return (
            BranchDisposition::Unresolved,
            "no molecule state; restore or audit the state before deciding".to_owned(),
        );
    };

    if molecule.status.is_alive() {
        return (BranchDisposition::Active, molecule.status.to_string());
    }

    if molecule.status == MoleculeStatus::Collapsed {
        let detail = molecule.collapse_reason.as_deref().map_or_else(
            || "collapsed (legacy record has no reason)".to_owned(),
            |reason| format!("collapsed — {reason}"),
        );
        return (BranchDisposition::RetainedAudited, detail);
    }

    if molecule
        .non_integration
        .as_ref()
        .is_some_and(|record| record.reason == cosmon_state::NonIntegrationReason::MergeSkipped)
    {
        let detail = molecule.harvest_reason.as_deref().map_or_else(
            || "harvested with merge deliberately skipped".to_owned(),
            |reason| format!("harvested with merge skipped — {reason}"),
        );
        return (BranchDisposition::RetainedAudited, detail);
    }

    let detail = molecule.non_integration.as_ref().map_or_else(
        || format!("{} with no recorded harvest", molecule.status),
        |record| format!("harvest unresolved: {}", record.reason.as_str()),
    );
    (BranchDisposition::Unresolved, detail)
}

/// Check surface freshness from the snapshot file.
///
/// Returns two independent facts, deliberately not merged: whether the
/// projected files still hash to what was recorded, and how long ago the
/// projection ran. They answer different questions and a single tick standing
/// for both is how `surfaces ✅` came to sit beside a 19-day-old reconcile.
fn check_surfaces(state_dir: &std::path::Path) -> SurfaceStatus {
    let snapshot = cosmon_surface::snapshot::load_snapshot(state_dir);

    if snapshot.surfaces.is_empty() {
        return SurfaceStatus {
            up_to_date: true,
            last_reconcile: None,
            stale_count: 0,
            reconcile_age_seconds: None,
            // Nothing was ever projected, so nothing is known to be fresh.
            // Absence is not freshness.
            reconcile_stale: true,
            projected: false,
        };
    }

    // Find the most recent projection timestamp
    let newest = snapshot
        .surfaces
        .values()
        .filter_map(|s| chrono::DateTime::parse_from_rfc3339(&s.projected_at).ok())
        .max()
        .map(|ts| chrono::Utc::now() - ts.with_timezone(&chrono::Utc));
    let last_reconcile = newest.map(format_duration);

    // Count stale surfaces by checking if files on disk still match snapshot hashes
    let mut stale_count = 0;
    for (surface_path, snap) in &snapshot.surfaces {
        // Try to read the surface file relative to the project root
        // State dir is typically .cosmon/state/, project root is two levels up
        let project_root = state_dir.parent().and_then(|p| p.parent());
        if let Some(root) = project_root {
            let full_path = root.join(surface_path);
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let hash = sha256_hex(&content);
                if hash != snap.content_hash {
                    stale_count += 1;
                }
            }
            // Missing file = stale (surface was projected but file deleted)
            else if !full_path.exists() {
                stale_count += 1;
            }
        }
    }

    SurfaceStatus {
        up_to_date: stale_count == 0,
        last_reconcile,
        stale_count,
        reconcile_age_seconds: newest.map(|d| d.num_seconds()),
        reconcile_stale: newest.is_none_or(staleness::reconcile_is_stale),
        projected: true,
    }
}

/// The set of molecules carrying a pilot lease, read from the ledger dir.
///
/// Built through `leases_at`, the one sanctioned constructor, even though
/// this reader needs no trust root: the ledger is reached one way in shipped
/// code, and a second route would be the drift the guard exists to prevent.
///
/// A failure to read is an empty set, never an error: `cs status` must not
/// stop working because a sibling mechanism is missing, and treating an
/// unreadable ledger as "no leases" only ever restores the old, slightly
/// pessimistic count.
fn lease_missions(state_dir: &std::path::Path) -> std::collections::BTreeSet<MoleculeId> {
    cosmon_harvest::pilot_gesture::leases_at(state_dir)
        .ok()
        .and_then(|store| store.missions().ok())
        .unwrap_or_default()
}

/// Filename of the gauge sample, under the state dir.
const GAUGE_FILE: &str = "status-gauge.json";

/// How long a sample stands before it is replaced.
///
/// Without it, running `cs status` twice in a minute would overwrite the
/// sample with the current value and report a delta of zero forever — the
/// gauge would erase exactly the movement it exists to show.
fn gauge_window() -> chrono::Duration {
    chrono::Duration::hours(1)
}

/// The persisted unmerged-branch sample.
#[derive(serde::Serialize, serde::Deserialize)]
struct GaugeSample {
    /// Branch count at the time of the sample.
    unmerged_branches: usize,
    /// Commit total at the time of the sample.
    unmerged_commits: usize,
    /// When it was taken.
    sampled_at: chrono::DateTime<chrono::Utc>,
}

/// Compare the current unmerged count against the last sample, and refresh
/// the sample when it has aged past [`gauge_window`].
///
/// Every filesystem failure is swallowed into "no previous sample". A pulse
/// command that errored because it could not write a convenience file would
/// be trading the whole reading for one of its derivatives.
fn sample_unmerged_gauge(
    state_dir: &std::path::Path,
    contributions: &[ContributionInfo],
) -> UnmergedGauge {
    let unresolved: Vec<_> = contributions
        .iter()
        .filter(|contribution| contribution.is_unresolved())
        .collect();
    let branches = unresolved.len();
    let commits: usize = unresolved.iter().map(|c| c.commits_ahead).sum();
    let now = chrono::Utc::now();
    let path = state_dir.join(GAUGE_FILE);

    let previous: Option<GaugeSample> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());

    let (previous_branches, delta, since_seconds) = match &previous {
        Some(prev) => {
            let since = now.signed_duration_since(prev.sampled_at).num_seconds();
            let delta = i64::try_from(branches).unwrap_or(i64::MAX)
                - i64::try_from(prev.unmerged_branches).unwrap_or(i64::MAX);
            (Some(prev.unmerged_branches), Some(delta), Some(since))
        }
        None => (None, None, None),
    };

    let should_refresh = previous
        .as_ref()
        .is_none_or(|prev| now.signed_duration_since(prev.sampled_at) > gauge_window());
    if should_refresh {
        write_gauge_sample(
            &path,
            &GaugeSample {
                unmerged_branches: branches,
                unmerged_commits: commits,
                sampled_at: now,
            },
        );
    }

    UnmergedGauge {
        branches,
        commits,
        previous_branches,
        delta,
        since_seconds,
    }
}

/// Write the sample through a temp file, so a crash mid-write leaves the
/// previous sample intact rather than a truncated one that parses to nothing.
fn write_gauge_sample(path: &std::path::Path, sample: &GaugeSample) {
    let Ok(body) = serde_json::to_string_pretty(sample) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Compute SHA-256 hex digest of a string./// Compute SHA-256 hex digest of a string.
fn sha256_hex(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Format a chrono Duration as a human-readable age string (e.g. "2m ago", "3h ago").
fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Render a one-line breakdown of the per-kind galaxy counts.
///
/// Iterates in the canonical family order (infra → editorial → nascent)
/// so operators see the same shape across every invocation. Missing
/// kinds are skipped, not printed as `0`, to keep the line dense.
fn render_galaxies_line(galaxies: &GalaxiesSummary) -> String {
    let order = ["infra", "project", "social-hub", "editorial", "nascent"];
    let parts: Vec<String> = order
        .iter()
        .filter_map(|token| {
            galaxies
                .by_kind
                .get(*token)
                .filter(|&&n| n > 0)
                .map(|n| format!("{n} {token}"))
        })
        .collect();
    parts.join(", ")
}

/// Load the four-family summary from neurion's registry DB.
///
/// Read-only; a missing DB is modeled as an empty summary rather than
/// an error so `cs status` works in environments where neurion has
/// never booted.
///
/// Issue #97: `repos` accumulates one row per directory ever registered,
/// including throwaway test fixtures under a tempdir (`/private/var/
/// folders/**/T/...`) whose `git init` got picked up somewhere and never
/// cleaned up — measured on one live instance, 34656 of 35061 rows were
/// such fixtures, almost all already deleted from disk, printing an
/// implausible "34899 nascent". A row whose `local_path` no longer names a
/// directory on disk is not a galaxy, nascent or otherwise; it is dead
/// registry state, and this query does not count it.
fn load_galaxies_summary() -> anyhow::Result<GalaxiesSummary> {
    load_galaxies_summary_at(&neurion_db_path()?)
}

/// [`load_galaxies_summary`], parameterized on the DB path so a test can
/// point it at a throwaway `SQLite` file instead of the real neurion
/// registry.
fn load_galaxies_summary_at(db: &std::path::Path) -> anyhow::Result<GalaxiesSummary> {
    if !db.exists() {
        return Ok(GalaxiesSummary {
            by_kind: BTreeMap::new(),
            total: 0,
            nascent: 0,
        });
    }

    let conn = rusqlite::Connection::open(db)?;
    // If a column is absent (pre-migration DB) we fall through to an
    // empty summary. Pragmatic: the single row-reading error path also
    // catches "no such column" without a second probe.
    let sql = "SELECT galaxy_kind, local_path FROM repos";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Ok(GalaxiesSummary {
            by_kind: BTreeMap::new(),
            total: 0,
            nascent: 0,
        });
    };
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    let mut nascent = 0usize;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
        ))
    })?;
    for row in rows {
        let (kind_opt, local_path) = row?;
        let is_live = local_path.is_some_and(|p| std::path::Path::new(&p).is_dir());
        if !is_live {
            continue;
        }
        total += 1;
        let key = match kind_opt {
            Some(ref s) if !s.is_empty() && neurion_core::GalaxyKind::from_str(s).is_some() => {
                s.clone()
            }
            _ => {
                nascent += 1;
                "nascent".to_owned()
            }
        };
        *by_kind.entry(key).or_insert(0) += 1;
    }

    Ok(GalaxiesSummary {
        by_kind,
        total,
        nascent,
    })
}

/// Locate the neurion `SQLite` database. Mirrors the private `db_path`
/// in neurion-mcp — kept in lockstep by the same convention
/// (`<data_dir>/neurion/neurion.db`). No side effects.
fn neurion_db_path() -> anyhow::Result<std::path::PathBuf> {
    let dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine data directory"))?
        .join("neurion");
    Ok(dir.join("neurion.db"))
}

/// Workers whose session is gone while their molecule is still `Running`.
///
/// One probe per worker bound to a non-terminal molecule, classified by
/// [`super::ensemble::molecule_ghost`] — the function behind the ensemble's
/// ghost column — so `cs status` and `cs ensemble` agree by construction.
fn dead_workers(
    fleet: &cosmon_state::Fleet,
    molecules: &[cosmon_state::MoleculeData],
    backends: &[cosmon_transport::TmuxBackend],
) -> Vec<DeadWorker> {
    let mut dead: Vec<DeadWorker> = fleet
        .workers
        .values()
        .filter_map(|w| {
            let mol = molecules
                .iter()
                .find(|m| Some(&m.id) == w.current_molecule.as_ref())?;
            if !mol.status.is_alive() {
                return None;
            }
            let supervision = mol
                .process
                .as_ref()
                .and_then(|pr| pr.adapter_name.as_deref())
                .map_or(
                    cosmon_transport::registry::SupervisionMode::TmuxPane,
                    cosmon_transport::registry::supervision_mode_for,
                );
            let transport = super::ensemble::probe_transport(backends, &w.id);
            let ghost =
                super::ensemble::molecule_ghost(mol.status, mol.merged_at, supervision, transport);
            super::ensemble::is_dead_ghost(ghost).then(|| DeadWorker {
                worker: w.id.to_string(),
                molecule: mol.id.to_string(),
            })
        })
        .collect();
    dead.sort_by(|a, b| a.worker.cmp(&b.worker));
    dead
}

/// Discover all fleet-scoped tmux backends (same as ensemble.rs).
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

/// `cs status <id>` — the one-molecule read.
///
/// The four machine fields come from the one status verb the remote route
/// projects too, so the local and remote JSON answers cannot diverge. Human
/// output also names pending dependents held by this blocker. `--json` emits
/// exactly the
/// wire shape of `GET /v1/molecules/{id}/status` minus its envelope, which is
/// what makes a script portable between the two.
fn run_one(ctx: &Context, id: &str) -> anyhow::Result<()> {
    let molecule_id = cosmon_core::id::MoleculeId::new(id)
        .map_err(|e| anyhow::anyhow!("invalid molecule id: {e}"))?;
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    let view = cosmon_state::ops::molecule_status(store.as_ref(), &molecule_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let status = cosmon_state::ops::StatusJson::from_view(&view);
    let delivery = cosmon_state::events::input_injection::latest_briefing_delivery(
        &store.molecule_dir(&molecule_id),
    );

    if ctx.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("{} {}", view.status.emoji(), view.id);
        println!("  status:     {}", status.status);
        println!("  phase:      {}", status.phase);
        println!("  updated_at: {}", status.updated_at);
        println!("  terminal:   {}", status.terminal);
        if let Some(outcome) = delivery {
            println!("  briefing:   {outcome}");
            if let Some(recovery) =
                cosmon_state::events::input_injection::briefing_recovery(&molecule_id, outcome)
            {
                println!("  recovery:   {recovery}");
            }
        }
        if matches!(
            view.status,
            MoleculeStatus::Collapsed | MoleculeStatus::Frozen
        ) {
            let molecules = store.list_molecules(&MoleculeFilter::default())?;
            for (dependent, blocker, status) in held_dependents(&molecules)
                .into_iter()
                .filter(|(_, blocker, _)| blocker == &molecule_id)
            {
                println!("  blocked:    {dependent}");
                println!(
                    "  release:    {}",
                    release_guidance(status, &dependent, &blocker)
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use chrono::Utc;

    use cosmon_core::id::{FormulaId, MoleculeId};
    use cosmon_core::molecule::MoleculeStatus;

    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, MoleculeData, StateStore};
    use tempfile::TempDir;

    #[test]
    fn status_names_pending_dependent_held_by_collapsed_blocker() {
        let blocker = make_molecule("aaaa", MoleculeStatus::Collapsed, None);
        let mut dependent = make_molecule("bbbb", MoleculeStatus::Pending, None);
        dependent
            .typed_links
            .push(cosmon_core::interaction::MoleculeLink::BlockedBy {
                source: blocker.id.clone(),
            });
        let held = held_dependents(&[blocker.clone(), dependent.clone()]);
        assert_eq!(
            held,
            vec![(
                dependent.id.clone(),
                blocker.id.clone(),
                MoleculeStatus::Collapsed
            )]
        );
        let guidance = release_guidance(held[0].2, &held[0].0, &held[0].1);
        assert!(guidance.contains(&dependent.id.to_string()));
        assert!(guidance.contains("--blocked-by"));
    }

    #[test]
    fn frozen_blocker_guidance_uses_molecule_completion_commands() {
        let blocker = make_molecule("aaaa", MoleculeStatus::Frozen, None);
        let dependent = make_molecule("bbbb", MoleculeStatus::Pending, None);
        let guidance = release_guidance(MoleculeStatus::Frozen, &dependent.id, &blocker.id);
        assert!(guidance.contains(&format!("cs complete {}", blocker.id)));
        assert!(guidance.contains(&format!("cs done {}", blocker.id)));
    }

    #[test]
    fn kill_switch_line_is_silent_when_nothing_is_set() {
        assert_eq!(render_kill_switch_line(&[]), None);
    }

    #[test]
    fn kill_switch_line_names_every_active_file_and_the_global_scope() {
        let line = render_kill_switch_line(&[KillSwitch::StandDown, KillSwitch::Ask]).unwrap();
        assert_eq!(
            line,
            "kill-switch: ~/.cosmon/stand-down.lock, ~/.cosmon/ask.off — all autonomous activity stopped"
        );
        let line = render_kill_switch_line(&[KillSwitch::Health]).unwrap();
        assert_eq!(line, "kill-switch: ~/.cosmon/health.off — scoped stop");
    }

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        (tmp, store)
    }

    fn make_molecule(
        suffix: &str,
        status: MoleculeStatus,
        kind: Option<MoleculeKind>,
    ) -> MoleculeData {
        MoleculeData {
            harvest_reason: None,
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
            kind,
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
            base_branch: None,
            protected_paths: Vec::new(),
            pending_step: None,
            merged_at: None,
            non_integration: None,
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
    fn test_status_empty_fleet() {
        let (tmp, store) = make_store();
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args { molecule: None });
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_with_molecules() {
        let (tmp, store) = make_store();
        let fleet = Fleet::default();
        store.save_fleet(&fleet).unwrap();

        // Create molecules of various kinds and statuses
        let mols = vec![
            make_molecule("idea1", MoleculeStatus::Running, Some(MoleculeKind::Idea)),
            make_molecule("task1", MoleculeStatus::Running, Some(MoleculeKind::Task)),
            make_molecule("task2", MoleculeStatus::Pending, Some(MoleculeKind::Task)),
            make_molecule("bug1", MoleculeStatus::Running, Some(MoleculeKind::Issue)),
            make_molecule("done1", MoleculeStatus::Completed, None),
            make_molecule("fail1", MoleculeStatus::Collapsed, None),
        ];
        for mol in &mols {
            store.save_molecule(&mol.id, mol).unwrap();
        }

        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args { molecule: None });
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_json_output() {
        let (tmp, store) = make_store();
        let fleet = Fleet::default();
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("aaaa", MoleculeStatus::Running, Some(MoleculeKind::Task));
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args { molecule: None });
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_verbose() {
        let (tmp, store) = make_store();
        let fleet = Fleet::default();
        store.save_fleet(&fleet).unwrap();

        let mol = make_molecule("bbbb", MoleculeStatus::Pending, Some(MoleculeKind::Idea));
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: true,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args { molecule: None });
        assert!(result.is_ok());
    }

    #[test]
    fn test_render_bar() {
        let bar = render_bar(50.0, 10);
        // Should contain 5 full blocks and 5 light shade blocks (ignoring ANSI codes)
        assert!(bar.contains('\u{2588}'));
        assert!(bar.contains('\u{2591}'));
    }

    #[test]
    fn test_render_bar_boundaries() {
        let empty = render_bar(0.0, 10);
        assert!(empty.contains('\u{2591}'));

        let full = render_bar(100.0, 10);
        assert!(full.contains('\u{2588}'));

        // Over 100% clamps to 100%
        let over = render_bar(150.0, 10);
        assert!(over.contains('\u{2588}'));
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(chrono::Duration::seconds(30)), "30s ago");
        assert_eq!(format_duration(chrono::Duration::seconds(120)), "2m ago");
        assert_eq!(format_duration(chrono::Duration::seconds(7200)), "2h ago");
        assert_eq!(
            format_duration(chrono::Duration::seconds(172_800)),
            "2d ago"
        );
    }

    #[test]
    fn test_status_with_attention_budget() {
        let (tmp, store) = make_store();
        let mut fleet = Fleet::default();
        fleet.attention_budget = Some(50);
        store.save_fleet(&fleet).unwrap();

        // Create some alive molecules
        for i in 0..5 {
            let mol = make_molecule(
                &format!("attn{i}"),
                MoleculeStatus::Running,
                Some(MoleculeKind::Task),
            );
            store.save_molecule(&mol.id, &mol).unwrap();
        }

        // Compact mode
        let ctx = Context {
            verbose: false,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args { molecule: None });
        assert!(result.is_ok());

        // Verbose mode
        let ctx = Context {
            verbose: true,
            json: false,
            config: Some(tmp.path().to_path_buf()),
        };
        let result = run(&ctx, &Args { molecule: None });
        assert!(result.is_ok());
    }

    fn surfaces(up_to_date: bool, stale_count: usize, age_days: Option<i64>) -> SurfaceStatus {
        let age = age_days.map(chrono::Duration::days);
        SurfaceStatus {
            up_to_date,
            last_reconcile: age.map(format_duration),
            stale_count,
            reconcile_age_seconds: age.map(|d| d.num_seconds()),
            reconcile_stale: age.is_none_or(staleness::reconcile_is_stale),
            projected: age.is_some(),
        }
    }

    /// The tick must not stand for a fact it did not check. Three states, one
    /// glyph each — the defect was that all three rendered as `✅`.
    #[test]
    fn the_surface_tick_asserts_only_what_it_checked() {
        colored::control::set_override(false);

        let fresh = render_surfaces_token(&surfaces(true, 0, Some(2)));
        assert!(fresh.contains('\u{2705}'), "{fresh}");
        assert!(
            fresh.contains("2d ago"),
            "a tick must carry its age: {fresh}"
        );

        let old = render_surfaces_token(&surfaces(true, 0, Some(19)));
        assert!(
            !old.contains('\u{2705}'),
            "a 19-day-old reconcile must not read as a tick: {old}"
        );
        assert!(old.contains("19d ago"), "{old}");

        let never = render_surfaces_token(&surfaces(true, 0, None));
        assert!(
            !never.contains('\u{2705}'),
            "never reconciled is not clean: {never}"
        );

        let drifted = render_surfaces_token(&surfaces(false, 3, Some(1)));
        assert!(drifted.contains("3 stale"), "{drifted}");

        colored::control::unset_override();
    }

    /// The age token renders the oldest molecule and the count past the
    /// threshold; an empty backlog renders nothing at all.
    #[test]
    fn the_age_token_names_the_threshold_it_crossed() {
        colored::control::set_override(false);

        assert!(render_backlog_age(&BacklogAge::default()).is_none());

        let stale = BacklogAge {
            counted: 4,
            stale: 2,
            oldest: Some(chrono::Duration::days(39)),
            oldest_id: cosmon_core::id::MoleculeId::new("task-20260811-a7f0").ok(),
            leases_excluded: 0,
        };
        let token = render_backlog_age(&stale).expect("a token");
        assert!(token.contains("oldest 39d"), "{token}");
        assert!(token.contains("2 >48h"), "{token}");

        let fresh = BacklogAge {
            counted: 1,
            stale: 0,
            oldest: Some(chrono::Duration::hours(3)),
            oldest_id: cosmon_core::id::MoleculeId::new("task-20260811-a7f0").ok(),
            leases_excluded: 0,
        };
        let token = render_backlog_age(&fresh).expect("a token");
        assert_eq!(
            token, "oldest 3h",
            "nothing is past the threshold to report"
        );

        colored::control::unset_override();
    }

    /// The gauge keeps its sample rather than overwriting it on every run —
    /// otherwise a second `cs status` a minute later erases exactly the
    /// movement the gauge exists to show.
    #[test]
    fn the_gauge_holds_its_sample_for_a_window() {
        let tmp = TempDir::new().unwrap();
        let state_dir = tmp.path();
        let three = vec![
            ContributionInfo {
                branch: "a".to_owned(),
                commits_ahead: 2,
                molecule: "task-20260101-aaaa".to_owned(),
                disposition: BranchDisposition::Unresolved,
                detail: "completed with no recorded harvest".to_owned(),
            },
            ContributionInfo {
                branch: "b".to_owned(),
                commits_ahead: 1,
                molecule: "task-20260101-bbbb".to_owned(),
                disposition: BranchDisposition::Unresolved,
                detail: "completed with no recorded harvest".to_owned(),
            },
            ContributionInfo {
                branch: "c".to_owned(),
                commits_ahead: 5,
                molecule: "task-20260101-cccc".to_owned(),
                disposition: BranchDisposition::Unresolved,
                detail: "completed with no recorded harvest".to_owned(),
            },
        ];

        let first = sample_unmerged_gauge(state_dir, &three);
        assert_eq!(first.branches, 3);
        assert_eq!(first.commits, 8);
        assert!(
            first.delta.is_none(),
            "no baseline means no delta, not a delta of zero"
        );

        // A second run inside the window compares against the first sample
        // instead of replacing it.
        let second = sample_unmerged_gauge(state_dir, &three[..1]);
        assert_eq!(second.previous_branches, Some(3));
        assert_eq!(second.delta, Some(-2));

        let third = sample_unmerged_gauge(state_dir, &three[..1]);
        assert_eq!(
            third.previous_branches,
            Some(3),
            "the sample must survive the second run, or the window is one invocation wide"
        );
    }

    /// The whole point of the lease exclusion, at the seam that reads it: a
    /// molecule is a lease because the ledger names it, never because of its
    /// id.
    #[test]
    fn lease_missions_come_from_the_ledger() {
        let tmp = TempDir::new().unwrap();
        assert!(lease_missions(tmp.path()).is_empty());

        let dir = tmp.path().join("pilot-lease");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("task-20260811-a7f0.grants.jsonl"), "").unwrap();

        let found = lease_missions(tmp.path());
        assert_eq!(found.len(), 1);
        assert!(found.contains(&cosmon_core::id::MoleculeId::new("task-20260811-a7f0").unwrap()));
    }

    /// Only `feat/<valid-molecule-id>` is molecule work; every operator
    /// namespace observed in the wild (`backup/*`, `spore/*`, a bare
    /// release-bake name) is not.
    #[test]
    fn only_feat_prefixed_valid_ids_are_molecule_branches() {
        assert!(is_molecule_branch("feat/task-20260101-aaaa"));
        assert!(!is_molecule_branch("backup/before-migration"));
        assert!(!is_molecule_branch("spore/math-attack"));
        assert!(!is_molecule_branch("release-2026.09"));
        assert!(!is_molecule_branch("feat/not-a-molecule-id"));
    }

    /// Issue #97: a `repos` row whose `local_path` no longer names a
    /// directory on disk is dead registry state, not a galaxy — it must be
    /// excluded from both `total` and `nascent` rather than inflating the
    /// nascent count the way 34656 stale tempdir fixtures did on a live
    /// instance.
    #[test]
    fn dead_repo_rows_do_not_inflate_the_galaxy_count() {
        let tmp = TempDir::new().unwrap();
        let live_dir = tmp.path().join("live-galaxy");
        std::fs::create_dir_all(&live_dir).unwrap();
        let dead_dir = tmp.path().join("this-was-deleted");

        let db = tmp.path().join("neurion.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE repos (name TEXT PRIMARY KEY, local_path TEXT, galaxy_kind TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (name, local_path, galaxy_kind) VALUES (?1, ?2, ?3)",
            rusqlite::params!["live", live_dir.to_str().unwrap(), "infra"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repos (name, local_path, galaxy_kind) VALUES (?1, ?2, NULL)",
            rusqlite::params!["dead", dead_dir.to_str().unwrap()],
        )
        .unwrap();
        drop(conn);

        let summary = load_galaxies_summary_at(&db).unwrap();
        assert_eq!(summary.total, 1, "the deleted-fixture row must not count");
        assert_eq!(summary.nascent, 0);
        assert_eq!(summary.by_kind.get("infra"), Some(&1));
    }

    /// `clean` is `true` only when there is nothing for the pilot to have
    /// left behind — a zombie, an un-harvested molecule, or an unmerged
    /// branch each independently make it `false` (issue #97).
    #[test]
    fn hygiene_is_clean_only_with_no_residue() {
        assert!(HygieneInfo::new(0, 0, 0, 0, 2, 3).clean);
        assert!(
            !HygieneInfo::new(1, 0, 0, 0, 0, 0).clean,
            "a zombie session is residue"
        );
        assert!(
            !HygieneInfo::new(0, 0, 1, 0, 0, 0).clean,
            "an un-harvested molecule is residue"
        );
        assert!(
            !HygieneInfo::new(0, 0, 0, 1, 0, 0).clean,
            "an unmerged molecule branch is residue"
        );
        assert!(
            !HygieneInfo::new(0, 1, 0, 0, 0, 0).clean,
            "a dead worker is residue (issue #172)"
        );
    }

    /// Issue #172 — the classification `cs status` counts from is the one
    /// `cs ensemble` renders: a `Running` molecule whose session is gone is a
    /// dead ghost, while the same molecule under a live session is not.
    #[test]
    fn dead_ghost_needs_a_running_molecule_without_a_session() {
        use super::super::ensemble::{is_dead_ghost, molecule_ghost};
        use cosmon_core::reconcile::TransportState;
        use cosmon_transport::registry::SupervisionMode;

        let tmux = SupervisionMode::TmuxPane;
        let ghost = |t| molecule_ghost(MoleculeStatus::Running, None, tmux, t);
        assert!(is_dead_ghost(ghost(TransportState::Dead)));
        assert!(!is_dead_ghost(ghost(TransportState::Alive)));
        assert!(
            !is_dead_ghost(molecule_ghost(
                MoleculeStatus::Running,
                None,
                SupervisionMode::InProcess,
                TransportState::Dead
            )),
            "an in-process loop has no pane by design"
        );
    }

    /// Branch classification consumes the lifecycle record that already owns
    /// the audit facts: live work is active, collapse carries its disposition,
    /// and a completed branch with no harvest remains actionable.
    #[test]
    fn branch_disposition_uses_existing_lifecycle_evidence() {
        let active = make_molecule("live", MoleculeStatus::Running, None);
        assert_eq!(classify_branch(Some(&active)).0, BranchDisposition::Active);

        let mut retained = make_molecule("kept", MoleculeStatus::Collapsed, None);
        retained.collapse_reason = Some("superseded after review".to_owned());
        let (disposition, detail) = classify_branch(Some(&retained));
        assert_eq!(disposition, BranchDisposition::RetainedAudited);
        assert!(detail.contains("superseded after review"), "{detail}");

        let unresolved = make_molecule("done", MoleculeStatus::Completed, None);
        assert_eq!(
            classify_branch(Some(&unresolved)).0,
            BranchDisposition::Unresolved
        );
        assert_eq!(
            classify_branch(None).0,
            BranchDisposition::Unresolved,
            "a branch with no molecule record must never be guessed clean"
        );
    }

    /// Human status must name the branch in every class and expose the
    /// retained verdict, rather than reducing all three to one count.
    #[test]
    fn branch_rows_name_unresolved_active_and_retained_work() {
        colored::control::set_override(false);
        let rows = render_contribution_lines(&[
            ContributionInfo {
                branch: "feat/task-20260101-aaaa".to_owned(),
                commits_ahead: 1,
                molecule: "task-20260101-aaaa".to_owned(),
                disposition: BranchDisposition::Active,
                detail: "running".to_owned(),
            },
            ContributionInfo {
                branch: "feat/task-20260101-bbbb".to_owned(),
                commits_ahead: 2,
                molecule: "task-20260101-bbbb".to_owned(),
                disposition: BranchDisposition::RetainedAudited,
                detail: "collapsed — duplicate".to_owned(),
            },
            ContributionInfo {
                branch: "feat/task-20260101-cccc".to_owned(),
                commits_ahead: 3,
                molecule: "task-20260101-cccc".to_owned(),
                disposition: BranchDisposition::Unresolved,
                detail: "completed with no recorded harvest".to_owned(),
            },
        ]);
        assert!(
            rows[0].contains("active (running) · not residue"),
            "{:?}",
            rows
        );
        assert!(
            rows[1].contains("audit disposition: collapsed — duplicate"),
            "{:?}",
            rows
        );
        assert!(rows[2].contains("cs done task-20260101-cccc"), "{:?}", rows);
        colored::control::unset_override();
    }
}
