// SPDX-License-Identifier: AGPL-3.0-only

//! `cs purge` — infrastructure teardown for workers.
//!
//! Two modes, one verb (ADR-052 §D3):
//!
//! 1. **Sweep** (no positional arg) — remove workers whose fleet entry is
//!    no longer load-bearing. Three populations qualify:
//!    * `desired = Stopped` workers (the pre-existing terminal case),
//!    * `desired = Running` / `desired = Paused` workers whose tmux session
//!      no longer exists — reclassified to [`WorkerStatus::Stale`] on the
//!      way out (the surface-lie bug where fleet read `Running` while the
//!      pane had been dead for hours, and `cs purge` reported "nothing to
//!      purge"), and
//!    * workers bound to a `Completed` / `Collapsed` molecule — the
//!      merge-without-done case. The tmux session may
//!      still be alive (the worker is idling at `❯` after `cs complete`);
//!      we only remove the fleet entry so `cs ensemble` stops reporting
//!      the worker as in flight. Tmux is left untouched — killing it is
//!      policy that belongs to `cs done` or `cs purge <worker> --force`.
//!
//!    The sweep touches no Active/Paused/Unresponsive/Starting/Stopping
//!    workers whose tmux session is still alive AND whose molecule is
//!    still alive — only truly orphaned ones.
//!
//! 2. **Targeted** (`cs purge <worker>`) — purge one specific worker. With
//!    `--force` the tmux session is SIGKILL'd before the fleet entry is
//!    removed, subsuming the former `cs kill` verb. Without `--force` the
//!    worker is expected to already be in a terminal state (graceful path).
//!
//! Both modes fail **closed** on unharvested work (incident 2026-08-02): a
//! worker whose pane is gone but whose molecule still carries commits ahead
//! of base — or a dirty worktree — is not purged and its molecule is not
//! collapsed. A missing tmux session is evidence about the pane, not about
//! the work; the reboot that removed the tmux server that day took four
//! healthy molecules with it. `--allow-unharvested` is the explicit gesture
//! that accepts the loss.
//!
//! ADR-052 §D3 collapses `cs kill` + `cs purge` into this one command:
//! both are infrastructure teardown; the difference was always the force
//! flag, not the perimeter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use cosmon_core::event_v2::{CollapseReason, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::molecule::{CollapseCause, MoleculeStatus};
use cosmon_core::transport::TransportBackend;
use cosmon_core::worker::{DesiredState, WorkerRole, WorkerStatus};
use cosmon_state::StateStore;
use cosmon_transport::TmuxBackend;

use super::Context;

/// Evidence that a molecule's deliverable is still only on its own branch or
/// in its own worktree — i.e. the work has **not** been harvested.
///
/// Recorded as data rather than a bare bool because the operator-facing
/// alert has to name what is at stake: an incident is only actionable if it
/// says *three commits on `feat/task-…-0c2d`* rather than "unharvested work".
/// The 2026-08-02 incident is precisely the case where nothing was named:
/// four molecules went `running → collapsed` after a machine reboot removed
/// the tmux server, and the commits they had already made were discovered by
/// hand, afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnharvestedWork {
    /// The molecule's work branch (`feat/<mol-id>`).
    pub branch: String,
    /// Commits on `branch` not reachable from the molecule's base branch.
    pub commits_ahead: usize,
    /// `git status --porcelain` entries in the molecule's worktree.
    pub dirty_files: Vec<String>,
    /// Set when the branch exists but its ahead-count could not be probed.
    ///
    /// A probe that cannot answer is not evidence of a harvested branch, so
    /// it counts as unharvested: the whole point of the guard is that
    /// "I could not check" must not read the same as "there is nothing there".
    pub probe_error: Option<String>,
}

impl UnharvestedWork {
    /// One line naming the commits and files at stake, for the alert.
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.commits_ahead > 0 {
            parts.push(format!(
                "{} commit(s) ahead of base on {}",
                self.commits_ahead, self.branch
            ));
        }
        if !self.dirty_files.is_empty() {
            parts.push(format!(
                "{} uncommitted file(s) in the worktree: {}",
                self.dirty_files.len(),
                self.dirty_files.join(", ")
            ));
        }
        if let Some(err) = &self.probe_error {
            parts.push(format!(
                "branch {} present but unprobeable: {err}",
                self.branch
            ));
        }
        parts.join("; ")
    }
}

/// Ask whether a molecule still holds work that no merge has taken.
///
/// A trait rather than a free function because the git probe is I/O at the
/// edge: the sweep's policy (withhold or collapse) is what deserves a test,
/// and a test that has to build a real repository with a diverged branch for
/// every case tests git instead of the policy.
pub(crate) trait HarvestProbe {
    /// `Some(evidence)` when the molecule's work is demonstrably unharvested,
    /// `None` when there is nothing at stake (no repo, no branch, branch
    /// fully merged and worktree clean).
    fn unharvested(&self, mol_id: &MoleculeId, base: Option<&str>) -> Option<UnharvestedWork>;
}

/// The production [`HarvestProbe`]: `git rev-list` + `git status` against the
/// galaxy repository.
struct GitHarvestProbe {
    /// Repository root, or `None` when the command is not run inside one —
    /// in which case there are no worktrees and no branches to lose, and the
    /// probe answers `None` for every molecule.
    repo_root: Option<PathBuf>,
    /// The galaxy's `[project] trunk_branch`, for base-branch resolution.
    configured_trunk: Option<String>,
}

impl GitHarvestProbe {
    /// Build the probe from the invocation context (repo discovery + config).
    fn discover(ctx: &Context) -> Self {
        let repo_root = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
            .filter(|p| !p.as_os_str().is_empty());
        let configured_trunk =
            cosmon_filestore::load_project_config(&super::resolve_config_from_context(ctx))
                .ok()
                .and_then(|cfg| cfg.project.trunk_branch);
        Self {
            repo_root,
            configured_trunk,
        }
    }
}

/// `true` when `refs/heads/<branch>` resolves in `repo_root`.
fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Commits reachable from `branch` but not from `base`; `None` when the
/// probe itself failed (which the caller must not read as zero).
fn commits_ahead_of(repo_root: &Path, base: &str, branch: &str) -> Option<usize> {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-list",
            "--count",
            &format!("{base}..{branch}"),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Paths reported by `git status --porcelain` in a molecule's worktree.
///
/// An absent worktree is not dirty. A `git status` that fails is reported as
/// clean here on purpose: the branch probe is the load-bearing half, and a
/// failing status in a directory that may not even be a worktree would
/// otherwise withhold every purge on the host.
fn dirty_paths(worktree: &Path) -> Vec<String> {
    if !worktree.is_dir() {
        return Vec::new();
    }
    let Ok(out) = Command::new("git")
        .args(["-C", &worktree.to_string_lossy(), "status", "--porcelain"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.get(3..).map(str::trim).filter(|p| !p.is_empty()))
        .map(str::to_owned)
        .collect()
}

impl HarvestProbe for GitHarvestProbe {
    fn unharvested(&self, mol_id: &MoleculeId, base: Option<&str>) -> Option<UnharvestedWork> {
        let repo_root = self.repo_root.as_ref()?;
        let branch = format!("feat/{mol_id}");
        let worktree = repo_root.join(".worktrees").join(mol_id.as_str());
        let dirty_files = dirty_paths(&worktree);

        let (commits_ahead, probe_error) = if branch_exists(repo_root, &branch) {
            let base =
                cosmon_cli::base_branch::resolve(repo_root, base, self.configured_trunk.as_deref());
            match commits_ahead_of(repo_root, &base, &branch) {
                Some(n) => (n, None),
                None => (
                    0,
                    Some(format!("`git rev-list --count {base}..{branch}` failed")),
                ),
            }
        } else {
            (0, None)
        };

        if commits_ahead == 0 && dirty_files.is_empty() && probe_error.is_none() {
            return None;
        }
        Some(UnharvestedWork {
            branch,
            commits_ahead,
            dirty_files,
            probe_error,
        })
    }
}

/// A worker the sweep refused to purge, with the evidence that stopped it.
struct Withheld {
    /// The worker whose fleet entry was left in place.
    worker: WorkerId,
    /// The still-`Running` molecule that would have been collapsed.
    molecule: MoleculeId,
    /// What is at stake.
    work: UnharvestedWork,
}

/// Fail-closed split of the stale population (incident 2026-08-02).
///
/// A missing tmux session proves that the *pane* is gone. It proves nothing
/// about the work: when the machine rebooted on 2026-08-02 the tmux server
/// disappeared under four healthy workers, and `cs purge` read every one of
/// them as stale and flipped its molecule `running → collapsed` — while
/// `task-…-16bf` was one commit ahead of main, `task-…-0c2d` two, and
/// `task-…-7582` had three uncommitted files in its worktree. Nothing in the
/// output named any of it.
///
/// So the sweep now separates two questions. *Is the pane dead?* decides the
/// stale classification. *Is the work harvested?* decides whether the
/// molecule may be collapsed. When the second answer is no, the worker is
/// withheld entirely — neither the molecule flipped nor the fleet entry
/// removed, so `cs ensemble` keeps showing it and the branch keeps its only
/// witness. `--allow-unharvested` is the operator's explicit statement that
/// the loss is acceptable.
///
/// Only `Running` molecules are guarded: a terminal molecule is not going to
/// be collapsed by [`collapse_zombie_molecule`], so there is nothing to fail
/// closed about.
fn withhold_unharvested(
    fleet: &cosmon_state::Fleet,
    store: &dyn StateStore,
    probe: &dyn HarvestProbe,
    stale: Vec<WorkerId>,
) -> (Vec<WorkerId>, Vec<Withheld>) {
    let mut keep = Vec::new();
    let mut withheld = Vec::new();
    for wid in stale {
        let mol = fleet
            .workers
            .get(&wid)
            .and_then(|w| w.current_molecule.clone())
            .and_then(|mid| store.load_molecule(&mid).ok())
            .filter(|m| m.status == MoleculeStatus::Running);
        let Some(mol) = mol else {
            keep.push(wid);
            continue;
        };
        match probe.unharvested(&mol.id, mol.base_branch.as_deref()) {
            Some(work) => withheld.push(Withheld {
                worker: wid,
                molecule: mol.id.clone(),
                work,
            }),
            None => keep.push(wid),
        }
    }
    (keep, withheld)
}

/// Flip a zombie molecule's `state.json` from `Running` to `Collapsed` when
/// the worker bound to it is being purged because the worker process is gone
/// — dead tmux on the sweep `stale` path, or an explicit
/// `cs purge <worker> --force`.
///
/// This closes the machine-crash zombie window. Before this fix, `cs purge`
/// removed the worker's fleet entry but left `state.json` at
/// `status = running`, so the board read undrained on a raw read and the
/// operator had to `cs collapse` each zombie by hand. The exact pathology
/// hit grace (verify-20260620-7e7b / verify-20260621-2b67) and cosmon (four
/// cosmon-ward molecules left `running` after their workers 401-died; purge
/// removed the workers but left the molecules running).
///
/// Defensive, in the spirit of the briefing seal (CLAUDE.md §briefing
/// seals): only a `Running` molecule is touched — terminal, frozen, pending,
/// and starved molecules are left exactly as they are, so an intentionally
/// suspended molecule is never collapsed out from under the operator. The
/// cause is recorded as [`CollapseCause::ProcessDeath`] and the reason-kind
/// as `worker_crashed` so `cs errors` aggregates it correctly. Any I/O
/// failure is swallowed so the purge hot path never blocks. Returns the
/// molecule id when a flip happened, so the caller can report it.
fn collapse_zombie_molecule(
    store: &dyn StateStore,
    events_path: &Path,
    mol_id: &MoleculeId,
    worker_id: &WorkerId,
) -> Option<MoleculeId> {
    let mut mol = store.load_molecule(mol_id).ok()?;
    if mol.status != MoleculeStatus::Running {
        return None;
    }
    let prev = mol.status;
    let reason = format!(
        "worker {worker_id} gone (purged); molecule was left running — \
         auto-collapsed by cs purge"
    );
    let kind = CollapseReason::from("worker_crashed".to_owned());

    mol.status = MoleculeStatus::Collapsed;
    mol.collapse_cause = Some(CollapseCause::ProcessDeath);
    mol.collapse_reason = Some(reason.clone());
    mol.collapse_reason_kind = Some(kind.clone());
    mol.collapsed_step = Some(mol.current_step);
    // Terminal transition: clear any inline live-process record so a
    // collapsed molecule never carries a phantom worker pointer (mirrors
    // `cs collapse`).
    if mol.process.is_some() {
        mol.release_process();
    }
    mol.updated_at = Utc::now();
    store.save_molecule(&mol.id.clone(), &mol).ok()?;

    let status_seq = cosmon_state::event_log::emit_one(
        events_path,
        EventV2::MoleculeStatusChanged {
            molecule_id: mol_id.clone(),
            from: prev.to_string(),
            to: "collapsed".to_owned(),
        },
        None,
    )
    .ok();
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        EventV2::MoleculeCollapsed {
            molecule_id: mol_id.clone(),
            reason,
            kind: Some(kind),
        },
        status_seq,
    );
    Some(mol_id.clone())
}

/// Flip every zombie molecule pinned to a `stale` worker (dead tmux = the
/// worker process is gone). Reads each stale worker's `current_molecule`
/// from `fleet` BEFORE the caller reclassifies them (which nulls the
/// binding), and returns the ids of the molecules actually collapsed.
///
/// Orphan workers are excluded by construction: the classifier only files a
/// worker as `orphan` when its molecule is already terminal, so there is no
/// zombie to flip there. The per-molecule `is_running` guard inside
/// [`collapse_zombie_molecule`] makes a double call a no-op.
fn collapse_stale_zombies(
    fleet: &cosmon_state::Fleet,
    store: &dyn StateStore,
    events_path: &Path,
    stale: &[WorkerId],
) -> Vec<String> {
    let mut collapsed = Vec::new();
    for wid in stale {
        if let Some(mid) = fleet
            .workers
            .get(wid)
            .and_then(|w| w.current_molecule.clone())
        {
            if let Some(flipped) = collapse_zombie_molecule(store, events_path, &mid, wid) {
                collapsed.push(flipped.as_str().to_owned());
            }
        }
    }
    collapsed
}

/// Arguments for the `purge` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Optional worker ID — when given, targeted purge of that worker only.
    ///
    /// Without a worker the command sweeps every terminal-state worker
    /// from fleet state (the pre-ADR-052 behaviour). With a worker, only
    /// that worker is removed; pair with `--force` to SIGKILL its tmux
    /// session first (formerly `cs kill`).
    pub worker: Option<String>,

    /// In targeted mode, SIGKILL the tmux session before removing the
    /// fleet entry. Ignored in sweep mode. Supersedes the stand-alone
    /// `cs kill` verb (ADR-052 §D3).
    #[arg(long)]
    pub force: bool,

    /// Only purge workers matching this desired state (default: sweep all
    /// workers — Stopped ones and Running/Paused ones whose tmux session
    /// is gone).
    #[arg(long)]
    pub status: Option<String>,

    /// Restrict the purge to workers matching this role discriminator —
    /// either `cognition` or `runtime` (see `WorkerRole`). Without this
    /// flag `cs purge` removes both runtime and cognition workers that
    /// meet the status predicate; with it, operators can clean up one
    /// half of a runtime+cognition pair without collapsing the other.
    #[arg(long, value_parser = parse_worker_role)]
    pub role: Option<WorkerRole>,

    /// Collapse molecules whose work is still unharvested (commits ahead of
    /// base, or an unclean worktree).
    ///
    /// Without this flag `cs purge` fails closed: a worker whose pane is
    /// gone but whose branch still carries commits — or whose worktree still
    /// has uncommitted files — is left in the fleet, its molecule left
    /// `running`, and the commits and files at stake are named in an alert.
    /// A dead tmux session is evidence about the pane, not about the work
    /// (incident 2026-08-02, where four molecules were silently collapsed
    /// after a reboot with up to three commits each still unmerged).
    #[arg(long)]
    pub allow_unharvested: bool,
}

fn parse_worker_role(s: &str) -> Result<WorkerRole, String> {
    s.parse::<WorkerRole>().map_err(|e| e.to_string())
}

/// Execute the `purge` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.state_dir();
    let store = ctx.store();

    let probe = GitHarvestProbe::discover(ctx);

    // Targeted mode — `cs purge <worker> [--force]` (supersedes `cs kill`).
    if let Some(ref worker_name) = args.worker {
        return run_targeted(
            ctx,
            store.as_ref(),
            &state_dir,
            worker_name,
            args.force,
            args.allow_unharvested,
            &probe,
        );
    }

    let socket = super::tmux_socket_name(ctx);
    let backend = TmuxBackend::new(&socket);
    run_sweep(ctx, store.as_ref(), &state_dir, &backend, &probe, args)
}

/// Populations produced by [`classify_sweep`] — one vec per reason code
/// so each can carry a distinct `WorkerKilled` event message and the
/// operator output can report "3 stale + 2 orphan" rather than a single
/// opaque total.
struct SweepBuckets {
    /// `desired = Stopped` workers — clean terminal state, purge as-is.
    terminal: Vec<WorkerId>,
    /// `desired = Running|Paused` with dead tmux — surface-lie population.
    /// Reclassified to `Stale` on the way out.
    stale: Vec<WorkerId>,
    /// Tmux alive but `current_molecule` is `Completed` / `Collapsed`.
    /// Fleet entry is removed; tmux untouched.
    orphan: Vec<WorkerId>,
}

/// Classify fleet workers into the three sweep populations.
///
/// Split out of [`run_sweep`] both to keep the outer function readable
/// and because the decision is policy (desired-state + molecule
/// terminality + tmux liveness) while the outer function is mechanism
/// (reclassify, remove, emit events).
fn classify_sweep<B: TransportBackend>(
    fleet: &cosmon_state::Fleet,
    store: &dyn StateStore,
    backend: &B,
    filter_desired: Option<DesiredState>,
    filter_role: Option<WorkerRole>,
) -> SweepBuckets {
    // Pre-load molecule terminality for every worker's current_molecule.
    // A miss (unreadable molecule file, unknown id) is treated as
    // non-terminal — the conservative default matching the stale-tmux
    // branch below, since a false-positive orphan reclassify would
    // silently destroy a live worker's fleet entry.
    let mol_terminal: HashMap<MoleculeId, bool> = fleet
        .workers
        .values()
        .filter_map(|w| w.current_molecule.clone())
        .map(|mid| {
            let terminal = store
                .load_molecule(&mid)
                .is_ok_and(|m| m.status.is_terminal());
            (mid, terminal)
        })
        .collect();

    let mut buckets = SweepBuckets {
        terminal: Vec::new(),
        stale: Vec::new(),
        orphan: Vec::new(),
    };

    for worker in fleet.workers.values() {
        if filter_role.is_some_and(|r| worker.worker_role != r) {
            continue;
        }
        if let Some(f) = filter_desired {
            if worker.desired != f {
                continue;
            }
        }
        let mol_is_terminal = worker
            .current_molecule
            .as_ref()
            .and_then(|mid| mol_terminal.get(mid))
            .copied()
            .unwrap_or(false);

        match worker.desired {
            DesiredState::Stopped => buckets.terminal.push(worker.id.clone()),
            DesiredState::Running | DesiredState::Paused => {
                // Probe the transport. An Err here (e.g. tmux not
                // installed on this host, socket permission error) is
                // treated as "alive" — only a definitive `Ok(false)`
                // counts as a stale-tmux verdict.
                let alive = backend.is_alive(&worker.id).unwrap_or(true);
                if !alive {
                    buckets.stale.push(worker.id.clone());
                } else if mol_is_terminal {
                    // tmux alive but molecule Completed/Collapsed — the
                    // fleet entry is the only thing keeping `cs ensemble`
                    // convinced the worker is in flight. Remove the
                    // entry; leave tmux alone (the agent may still be
                    // sitting at `❯` — the operator decides whether to
                    // kill the session).
                    buckets.orphan.push(worker.id.clone());
                }
            }
        }
    }
    buckets
}

/// Sweep-mode purge, parameterised over the transport backend so tests
/// can inject `MockBackend` without spinning up a real tmux server.
#[allow(clippy::too_many_lines)]
fn run_sweep<B: TransportBackend>(
    ctx: &Context,
    store: &dyn StateStore,
    state_dir: &Path,
    backend: &B,
    probe: &dyn HarvestProbe,
    args: &Args,
) -> anyhow::Result<()> {
    let mut fleet = store.load_fleet()?;

    let filter_desired: Option<DesiredState> = args
        .status
        .as_ref()
        .map(|s| s.parse())
        .transpose()
        .map_err(|e| anyhow::anyhow!("invalid status filter: {e}"))?;
    let filter_role = args.role;

    let SweepBuckets {
        terminal,
        mut stale,
        orphan,
    } = classify_sweep(&fleet, store, backend, filter_desired, filter_role);

    // Fail closed before anything is written: a stale pane whose molecule
    // still holds unharvested work is withheld from the sweep entirely.
    let withheld = if args.allow_unharvested {
        Vec::new()
    } else {
        let (keep, withheld) =
            withhold_unharvested(&fleet, store, probe, std::mem::take(&mut stale));
        stale = keep;
        withheld
    };

    let total = terminal.len() + stale.len() + orphan.len();
    if total == 0 && withheld.is_empty() {
        if ctx.json {
            println!(
                r#"{{"command":"purge","purged":0,"workers":[],"terminal":[],"stale":[],"orphan":[],"withheld":[]}}"#
            );
        } else {
            println!("Nothing to purge.");
        }
        return Ok(());
    }

    // Before clearing `current_molecule` below, collapse any zombie
    // molecule still pinned to a stale worker (machine crash / 401-death).
    let events_path = state_dir.join("events.jsonl");
    let zombies_collapsed = collapse_stale_zombies(&fleet, store, &events_path, &stale);

    // Reclassify stale + orphan workers' status so the fleet.json
    // snapshot on disk carries an accurate reason before the entry is
    // removed — any audit tooling that reads the pre-purge projection
    // (e.g. `cs reconcile`) sees `Stale`, not `Running`.
    let now = Utc::now();
    for wid in stale.iter().chain(orphan.iter()) {
        if let Some(w) = fleet.workers.get_mut(wid) {
            w.status = WorkerStatus::Stale;
            w.desired = DesiredState::Stopped;
            w.updated_at = now;
            w.current_molecule = None;
        }
    }

    let mut purged: Vec<String> = Vec::new();
    for wid in terminal.iter().chain(stale.iter()).chain(orphan.iter()) {
        fleet.workers.remove(wid);
        purged.push(wid.as_str().to_owned());
    }

    store.save_fleet(&fleet)?;

    // Emit WorkerKilled events. Distinct `reason` strings let downstream
    // consumers (the overseer, the chronicle sweep, `cs events`) tell
    // the two populations apart without cross-referencing fleet state.
    for wid in &terminal {
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::WorkerKilled {
                worker_id: wid.clone(),
                reason: "purged".to_owned(),
            },
            None,
        );
    }
    for wid in &stale {
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::WorkerKilled {
                worker_id: wid.clone(),
                reason: "purged: stale tmux (session missing)".to_owned(),
            },
            None,
        );
    }
    for wid in &orphan {
        let _ = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::WorkerKilled {
                worker_id: wid.clone(),
                reason: "purged: orphan (molecule terminal, fleet entry stale)".to_owned(),
            },
            None,
        );
    }

    if ctx.json {
        let out = serde_json::json!({
            "command": "purge",
            "purged": total,
            "workers": purged,
            "terminal": terminal.iter().map(|w| w.as_str().to_owned()).collect::<Vec<_>>(),
            "stale": stale.iter().map(|w| w.as_str().to_owned()).collect::<Vec<_>>(),
            "orphan": orphan.iter().map(|w| w.as_str().to_owned()).collect::<Vec<_>>(),
            "zombies_collapsed": zombies_collapsed,
            "withheld": withheld.iter().map(|w| serde_json::json!({
                "worker": w.worker.as_str(),
                "molecule": w.molecule.as_str(),
                "branch": w.work.branch,
                "commits_ahead": w.work.commits_ahead,
                "dirty_files": w.work.dirty_files,
                "probe_error": w.work.probe_error,
            })).collect::<Vec<_>>(),
        });
        println!("{out}");
    } else {
        if !zombies_collapsed.is_empty() {
            println!(
                "Collapsed {} zombie molecule(s) (running → collapsed, cause=process_death):",
                zombies_collapsed.len()
            );
            for mid in &zombies_collapsed {
                println!("  - {mid}");
            }
        }
        if !stale.is_empty() {
            println!(
                "Reclassified {} worker(s) to Stale (tmux session missing).",
                stale.len()
            );
        }
        if !orphan.is_empty() {
            println!(
                "Reclassified {} worker(s) to Stale (molecule terminal, fleet entry orphaned).",
                orphan.len()
            );
        }
        println!("Purged {total} worker(s):");
        for name in &purged {
            println!("  - {name}");
        }
        if !withheld.is_empty() {
            println!(
                "\nWITHHELD — {} worker(s) NOT purged: their pane is gone but their work is not \
                 harvested.",
                withheld.len()
            );
            for w in &withheld {
                println!(
                    "  - {} → molecule {} still running: {}",
                    w.worker,
                    w.molecule,
                    w.work.describe()
                );
            }
            println!(
                "  Harvest first (`cs done <molecule>`), or repeat with --allow-unharvested to \
                 collapse them and accept the loss."
            );
        }
    }

    if withheld.is_empty() {
        return Ok(());
    }

    // Non-zero exit, after the safe half of the sweep has been persisted.
    // The withheld population is exactly the shape the 2026-08-02 incident
    // took, and it went unnoticed because purge exited 0 and said nothing an
    // operator would stop for. Re-running the sweep is idempotent, so the
    // error repeats until the work is harvested or the loss is accepted.
    let detail = withheld
        .iter()
        .map(|w| format!("{} ({}): {}", w.molecule, w.worker, w.work.describe()))
        .collect::<Vec<_>>()
        .join("\n  ");
    anyhow::bail!(
        "refusing to collapse {} molecule(s) with unharvested work:\n  {detail}\n  \
         a missing tmux session is not evidence that the work failed. Harvest with \
         `cs done <molecule>`, or pass --allow-unharvested to collapse anyway.",
        withheld.len()
    )
}

/// The targeted-mode half of the fail-closed guard (see
/// [`withhold_unharvested`] for the sweep half and the incident it comes
/// from).
///
/// A targeted purge also flips a still-`Running` molecule to `Collapsed`, so
/// it can discard unmerged commits just as silently as the sweep. `--force`
/// is a statement about the tmux session, not about the work — only
/// `--allow-unharvested` accepts the loss, otherwise the guard would be one
/// `--force` away from useless. Called before any mutation, so a refusal
/// leaves fleet and molecule state untouched.
fn refuse_if_unharvested(
    fleet: &cosmon_state::Fleet,
    store: &dyn StateStore,
    probe: &dyn HarvestProbe,
    worker_id: &WorkerId,
) -> anyhow::Result<()> {
    let bound = fleet
        .workers
        .get(worker_id)
        .and_then(|w| w.current_molecule.clone())
        .and_then(|mid| store.load_molecule(&mid).ok())
        .filter(|m| m.status == MoleculeStatus::Running);
    let Some(mol) = bound else { return Ok(()) };
    let Some(work) = probe.unharvested(&mol.id, mol.base_branch.as_deref()) else {
        return Ok(());
    };
    anyhow::bail!(
        "refusing to purge {worker_id}: molecule {} is still running with unharvested work — \
         {}\n  a missing or killed pane is not evidence that the work failed. Harvest with \
         `cs done {}`, or pass --allow-unharvested to purge anyway.",
        mol.id,
        work.describe(),
        mol.id,
    )
}

/// Targeted purge — remove a single worker, optionally SIGKILL'ing tmux first.
///
/// Supersedes the legacy `cs kill` verb. With `force = true`, attempts a
/// best-effort graceful exit (short timeout) before force-terminating the
/// tmux session; with `force = false`, only the fleet record is cleaned up
/// (the worker is expected to already have exited). The fleet entry is
/// removed on success, emitting a `WorkerKilled` audit event.
fn run_targeted(
    ctx: &Context,
    store: &dyn StateStore,
    state_dir: &std::path::Path,
    worker_name: &str,
    force: bool,
    allow_unharvested: bool,
    probe: &dyn HarvestProbe,
) -> anyhow::Result<()> {
    let worker_id = WorkerId::new(worker_name)?;

    let mut fleet = store.load_fleet()?;

    // task-20260719-fedf — a bare "worker not found" is a dead end. During
    // the 2026-07-19 incident `cs purge` said exactly that for worker ids
    // `cs ensemble` was rendering at that very moment, and the operator had
    // no way to tell whether the id was wrong or the two verbs were reading
    // different stores. `cs ensemble` aggregates across every deployed fleet
    // (and, under `--all` / `--cluster`, across sibling galaxies); `cs purge`
    // only ever reads the current galaxy's fleet. Naming the store searched —
    // and what it does hold — turns the disagreement into a diagnosis.
    if !fleet.workers.contains_key(&worker_id) {
        let mut known: Vec<&str> = fleet.workers.keys().map(WorkerId::as_str).collect();
        known.sort_unstable();
        let known = if known.is_empty() {
            "none".to_owned()
        } else {
            known.join(", ")
        };
        anyhow::bail!(
            "worker not found: {worker_id}\n  searched: {}\n  this fleet holds: {known}\n  \
             note: `cs ensemble` may be showing workers from another fleet or galaxy \
             (`--all` / `--cluster`); purge only reads the current one",
            state_dir.display(),
        );
    }
    if !allow_unharvested {
        refuse_if_unharvested(&fleet, store, probe, &worker_id)?;
    }

    let Some(worker) = fleet.workers.get_mut(&worker_id) else {
        // Unreachable: presence was just established above.
        anyhow::bail!("worker not found: {worker_id}");
    };

    let previous_status = worker.status.to_string();
    // Capture the molecule binding before we null it — a targeted purge of
    // a worker whose molecule is still `running` leaves a crash zombie just
    // like the sweep stale path, so flip it below (the `is_running` guard
    // inside the helper leaves terminal/frozen molecules alone).
    let bound_molecule = worker.current_molecule.clone();
    worker.desired = DesiredState::Stopped;
    worker.status = WorkerStatus::Stopped;
    worker.updated_at = Utc::now();
    worker.current_molecule = None;

    // Force mode: try a quick graceful exit (triggers SessionEnd hooks,
    // memory flush), then terminate what survives.
    let tmux_killed = if force {
        let backend = TmuxBackend::new(super::tmux_socket_name(ctx));
        backend
            .graceful_exit(&worker_id, std::time::Duration::from_secs(5))
            .is_ok()
    } else {
        false
    };

    // Remove the worker from fleet state.
    fleet.workers.remove(&worker_id);
    store.save_fleet(&fleet)?;

    // Emit both legacy and V2 events so the audit trail is identical to
    // the old `cs kill` path (backward-compatible for consumers).
    let events_path = state_dir.join("events.jsonl");

    // Flip a zombie molecule the purged worker left running. Mirrors the
    // sweep stale path so `cs purge <worker> --force` no longer leaves the
    // board reading undrained.
    let zombie_collapsed = bound_molecule
        .as_ref()
        .and_then(|mid| collapse_zombie_molecule(store, &events_path, mid, &worker_id))
        .map(|mid| mid.as_str().to_owned());

    let _ = cosmon_filestore::event::append(
        &events_path,
        &cosmon_core::event::Envelope::now(cosmon_core::event::Event::WorkerKilled {
            worker_id: worker_id.clone(),
        }),
    );
    let reason = if force {
        format!("purged --force (was {previous_status})")
    } else {
        format!("purged (was {previous_status})")
    };
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        cosmon_core::event_v2::EventV2::WorkerKilled {
            worker_id: worker_id.clone(),
            reason,
        },
        None,
    );

    if ctx.json {
        let out = serde_json::json!({
            "command": "purge",
            "worker_id": worker_id.as_str(),
            "previous_status": previous_status,
            "status": "stopped",
            "force": force,
            "tmux_killed": tmux_killed,
            "purged": 1,
            "workers": [worker_id.as_str()],
            "zombie_collapsed": zombie_collapsed,
        });
        println!("{out}");
    } else {
        let verb = if force { "Force-purged" } else { "Purged" };
        println!("{verb} worker {worker_id} ({previous_status} -> removed)");
        if let Some(mid) = &zombie_collapsed {
            println!(
                "  • collapsed zombie molecule {mid} (running → collapsed, cause=process_death)"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cosmon_core::agent::AgentRole;
    use cosmon_core::clearance::Clearance;
    use cosmon_core::id::{AgentId, FleetId, FormulaId, MoleculeId, WorkerId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_core::transport::{AgentDefinition, RuntimeConfig, TransportBackend};
    use cosmon_core::worker::{DesiredState, WorkerStatus};
    use cosmon_filestore::FileStore;
    use cosmon_state::{Fleet, MoleculeData, StateStore, WorkerData};
    use cosmon_transport::MockBackend;
    use tempfile::TempDir;

    use super::*;

    /// Build a minimal [`MoleculeData`] for purge-sweep tests.
    fn sample_mol(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
            variables: std::collections::HashMap::new(),
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
            base_branch: None,
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

    /// Register `worker_id` as an alive tmux session in `backend`.
    ///
    /// Mirrors what `TmuxBackend::is_alive` would return `true` for — the
    /// mock is keyed by worker id, so once this is called the sweep's
    /// liveness probe sees the session as alive and leaves the entry.
    fn register_alive(backend: &MockBackend, worker_id: &str) {
        let agent = AgentDefinition {
            id: AgentId::new(worker_id).unwrap(),
            role: AgentRole::Implementation,
            command: "true".to_owned(),
            args: vec![],
        };
        let _ = backend.spawn(&agent, &RuntimeConfig::default());
    }

    /// Probe that reports every molecule as harvested — the ambient case for
    /// the pre-existing sweep tests, whose fixtures have no git repository.
    struct NoWork;
    impl HarvestProbe for NoWork {
        fn unharvested(&self, _mol: &MoleculeId, _base: Option<&str>) -> Option<UnharvestedWork> {
            None
        }
    }

    /// Probe that reports the given evidence for every molecule — the
    /// crashed-machine case, where the branch really does hold commits the
    /// merge never took.
    struct Unharvested(UnharvestedWork);
    impl HarvestProbe for Unharvested {
        fn unharvested(&self, _mol: &MoleculeId, _base: Option<&str>) -> Option<UnharvestedWork> {
            Some(self.0.clone())
        }
    }

    /// The 2026-08-02 shape: commits on the branch, nothing merged.
    fn commits_ahead(n: usize) -> Unharvested {
        Unharvested(UnharvestedWork {
            branch: "feat/task-20260802-16bf".to_owned(),
            commits_ahead: n,
            dirty_files: Vec::new(),
            probe_error: None,
        })
    }

    fn ctx_for(tmp: &TempDir, json: bool) -> Context {
        Context {
            verbose: false,
            json,
            config: Some(tmp.path().to_path_buf()),
        }
    }

    fn worker(name: &str, status: WorkerStatus, desired: DesiredState) -> WorkerData {
        let mut w = WorkerData::new(
            WorkerId::new(name).unwrap(),
            AgentId::new("a").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            status,
        );
        w.desired = desired;
        w
    }

    #[test]
    fn test_purge_removes_terminal_workers() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();

        // Active (desired=Running) AND tmux alive — should NOT be purged.
        let w1 = worker("alive", WorkerStatus::Active, DesiredState::Running);
        // Stopped — should be purged.
        let w2 = worker("dead", WorkerStatus::Stopped, DesiredState::Stopped);
        // Error + desired=Stopped — should be purged.
        let w3 = worker(
            "errored",
            WorkerStatus::Error("crash".to_owned()),
            DesiredState::Stopped,
        );

        fleet.workers.insert(w1.id.clone(), w1);
        fleet.workers.insert(w2.id.clone(), w2);
        fleet.workers.insert(w3.id.clone(), w3);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "alive");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
        assert!(fleet.workers.contains_key(&WorkerId::new("alive").unwrap()));
    }

    #[test]
    fn test_purge_reclassifies_dead_tmux_to_stale() {
        // desired=Running but tmux session is gone → reclassify to Stale
        // and purge. This is the surface-lie fix (task-20260419-5982):
        // before the probe, fleet reported Running + "nothing to purge"
        // while the pane had been dead for hours.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();

        let ghost = worker("ghost", WorkerStatus::Active, DesiredState::Running);
        let live = worker("live", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(ghost.id.clone(), ghost);
        fleet.workers.insert(live.id.clone(), live);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "live");
        // "ghost" deliberately not registered → is_alive returns false.

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1, "ghost worker must be purged");
        assert!(
            fleet.workers.contains_key(&WorkerId::new("live").unwrap()),
            "live worker must survive the sweep"
        );

        // Stale reclassification is traced in the event log.
        let events_path = tmp.path().join("events.jsonl");
        let events = std::fs::read_to_string(&events_path).unwrap();
        assert!(
            events.contains("stale tmux"),
            "WorkerKilled reason must flag the stale-tmux population; got: {events}"
        );
        assert!(
            events.contains("\"worker_id\":\"ghost\""),
            "events.jsonl must name the ghost worker; got: {events}"
        );
    }

    #[test]
    fn test_purge_paused_with_dead_tmux_is_also_reclassified() {
        // desired=Paused matters too — a paused worker whose pane has
        // been SIGKILL'd cannot be resumed. Same treatment as Running.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("paused-ghost", WorkerStatus::Paused, DesiredState::Paused);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.is_empty());
    }

    #[test]
    fn test_purge_status_filter_running_only_probes_running() {
        // --status=running should only purge running-with-dead-tmux; a
        // Stopped worker must be left alone even though it would
        // normally qualify in the default sweep.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let ghost = worker("ghost", WorkerStatus::Active, DesiredState::Running);
        let stopped = worker("retired", WorkerStatus::Stopped, DesiredState::Stopped);
        fleet.workers.insert(ghost.id.clone(), ghost);
        fleet.workers.insert(stopped.id.clone(), stopped);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: Some("running".to_owned()),
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
        assert!(
            fleet
                .workers
                .contains_key(&WorkerId::new("retired").unwrap()),
            "stopped worker must survive --status=running"
        );
    }

    #[test]
    fn test_purge_role_filter_keeps_opposite_half() {
        use cosmon_core::worker::WorkerRole;
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();

        // Stopped Cognition worker — should be purged when --role=cognition.
        let cog = worker("cog-dead", WorkerStatus::Stopped, DesiredState::Stopped);
        // Stopped Runtime worker — should NOT be purged when --role=cognition.
        let mut rt = WorkerData::new(
            WorkerId::new("runtime-dead").unwrap(),
            AgentId::new("runtime").unwrap(),
            AgentRole::Runtime,
            Clearance::Write,
            WorkerStatus::Stopped,
        );
        rt.desired = DesiredState::Stopped;

        fleet.workers.insert(cog.id.clone(), cog);
        fleet.workers.insert(rt.id.clone(), rt);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: Some(WorkerRole::Cognition),
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
        assert!(fleet
            .workers
            .contains_key(&WorkerId::new("runtime-dead").unwrap()));
    }

    #[test]
    fn test_purge_targeted_removes_single_worker() {
        // `cs purge <worker>` removes the named worker regardless of desired
        // state — it is the explicit operator intent, not a sweep predicate.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("wire", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("wire".to_owned()),
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run(&ctx, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(!fleet.workers.contains_key(&WorkerId::new("wire").unwrap()));
    }

    #[test]
    fn test_purge_targeted_nonexistent_errors() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&Fleet::default()).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("ghost".to_owned()),
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };

        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_purge_nothing_to_purge() {
        // All workers alive and running → nothing to sweep.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("active", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "active");

        let ctx = ctx_for(&tmp, true);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(fleet.workers.len(), 1);
    }

    #[test]
    #[allow(clippy::items_after_statements)]
    fn test_purge_transport_error_is_conservative() {
        // If the transport can't answer (returns Err), treat the worker
        // as alive — a false-reclassify would silently destroy a real
        // worker's fleet entry, the worst-case failure mode. The test
        // uses a canned-error backend to force the Err path.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mut fleet = Fleet::new();
        let w = worker("maybe-alive", WorkerStatus::Active, DesiredState::Running);
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        // ErrBackend: always returns Err from is_alive.
        struct ErrBackend;
        impl TransportBackend for ErrBackend {
            fn spawn(
                &self,
                _agent: &AgentDefinition,
                _config: &RuntimeConfig,
            ) -> Result<cosmon_core::transport::SpawnHandle, cosmon_core::transport::TransportError>
            {
                unreachable!()
            }
            fn terminate(
                &self,
                _id: &WorkerId,
            ) -> Result<(), cosmon_core::transport::TransportError> {
                unreachable!()
            }
            fn is_alive(
                &self,
                _id: &WorkerId,
            ) -> Result<bool, cosmon_core::transport::TransportError> {
                Err(cosmon_core::transport::TransportError::Io(
                    "simulated".to_owned(),
                ))
            }
            fn send_input(
                &self,
                _id: &WorkerId,
                _input: &str,
            ) -> Result<(), cosmon_core::transport::TransportError> {
                unreachable!()
            }
            fn capture_output(
                &self,
                _id: &WorkerId,
                _lines: usize,
            ) -> Result<String, cosmon_core::transport::TransportError> {
                unreachable!()
            }
            fn list_sessions(
                &self,
            ) -> Result<
                Vec<cosmon_core::transport::SessionInfo>,
                cosmon_core::transport::TransportError,
            > {
                unreachable!()
            }
            fn graceful_exit(
                &self,
                _id: &WorkerId,
                _timeout: std::time::Duration,
            ) -> Result<bool, cosmon_core::transport::TransportError> {
                unreachable!()
            }
        }

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &ErrBackend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(
            fleet.workers.len(),
            1,
            "transport error must NOT trigger a stale reclassify"
        );
    }

    /// Bind a worker to a molecule via `current_molecule`, parallel to what
    /// `cs tackle` does at spawn time.
    fn worker_with_mol(name: &str, mol: &str) -> WorkerData {
        let mut w = WorkerData::new(
            WorkerId::new(name).unwrap(),
            AgentId::new("a").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        w.desired = DesiredState::Running;
        w.current_molecule = Some(MoleculeId::new(mol).unwrap());
        w
    }

    #[test]
    fn test_purge_orphans_worker_when_molecule_completed_even_if_tmux_alive() {
        // bead ae83: a worker whose molecule was merged via manual
        // cherry-pick (bypassing `cs done`) stays in fleet.json forever
        // — tmux is still alive (agent idling at ❯ after `cs complete`)
        // but the molecule is `Completed`. `cs ensemble` then displays
        // it as `running/active`, noise that drowns the real signal.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        // Persist a Completed molecule and bind a worker to it.
        let mol = sample_mol("task-20260422-ae83", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("orphan-worker-ae83", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        // Tmux is alive for the orphan — the agent is still idling.
        register_alive(&backend, "orphan-worker-ae83");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(
            fleet.workers.is_empty(),
            "orphan worker bound to Completed molecule must be purged"
        );

        // Event reason must flag the new population so the chronicle
        // sweep can distinguish orphan from stale-tmux.
        let events = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
        assert!(
            events.contains("orphan (molecule terminal"),
            "WorkerKilled reason must flag the orphan population; got: {events}"
        );
    }

    #[test]
    fn test_purge_orphans_worker_when_molecule_collapsed() {
        // Symmetric case: collapsed molecule is also terminal. Worker
        // bound to a collapsed molecule is equally orphaned.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260422-c0ld", MoleculeStatus::Collapsed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("orphan-worker-c0ld", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "orphan-worker-c0ld");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.is_empty());
    }

    #[test]
    fn test_purge_keeps_worker_when_molecule_still_running() {
        // Guard: a worker bound to a Running molecule with live tmux is
        // the healthy case — the sweep MUST NOT touch it.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260422-live", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("live-worker", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "live-worker");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(
            fleet.workers.len(),
            1,
            "healthy worker must survive the sweep"
        );
    }

    #[test]
    fn test_purge_orphan_missing_molecule_file_is_conservative() {
        // If the molecule file cannot be loaded (deleted, permission
        // error, partial checkout), the terminality check defaults to
        // `false` — we leave the fleet entry alone rather than risk a
        // false-positive orphan reclassify. This mirrors the
        // `transport_error_is_conservative` guarantee on the stale path.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        // Note: no `save_molecule` call — the molecule file is absent.
        let mut fleet = Fleet::new();
        let w = worker_with_mol("bound-to-ghost", "task-20260422-ghst");
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        register_alive(&backend, "bound-to-ghost");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert_eq!(
            fleet.workers.len(),
            1,
            "missing molecule file must NOT trigger an orphan reclassify"
        );
    }

    // -----------------------------------------------------------------
    // Zombie-molecule flip (task-20260622-29e3, cosmon-ward from grace)
    // -----------------------------------------------------------------

    #[test]
    fn test_purge_sweep_collapses_running_molecule_of_stale_worker() {
        // The exact grace / cosmon zombie pathology: a worker's tmux dies
        // (machine crash, 401-death), the molecule is left at status
        // `running`, and the sweep removes the worker. Before the fix the
        // molecule stayed `running` forever — the board read undrained and
        // the operator had to `cs collapse` it by hand. The sweep must now
        // flip it to `Collapsed` with cause `process_death`.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("verify-20260620-7e7b", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("zombie-worker", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        // Deliberately NOT registered → is_alive == false → stale population.

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        // Worker removed.
        let fleet = store.load_fleet().unwrap();
        assert!(fleet.workers.is_empty(), "stale worker must be purged");

        // Molecule flipped to Collapsed / ProcessDeath.
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(
            reloaded.status,
            MoleculeStatus::Collapsed,
            "zombie running molecule must be collapsed by the sweep"
        );
        assert_eq!(
            reloaded.collapse_cause,
            Some(cosmon_core::molecule::CollapseCause::ProcessDeath),
            "cause must be process_death"
        );
        assert_eq!(reloaded.collapsed_step, Some(reloaded.current_step));

        // The flip is traced in the event log.
        let events = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
        assert!(
            events.contains("\"verify-20260620-7e7b\"") && events.contains("collapsed"),
            "events.jsonl must record the molecule collapse; got: {events}"
        );
    }

    #[test]
    fn test_purge_sweep_leaves_completed_molecule_of_stale_worker_untouched() {
        // Guard: a stale worker bound to an already-terminal molecule must
        // NOT have its molecule rewritten — only `Running` molecules are
        // zombies. A Completed molecule stays Completed.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260620-done", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("stale-but-done", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let backend = MockBackend::new();
        // Not registered → stale.

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(&ctx, &store, tmp.path(), &backend, &NoWork, &args).unwrap();

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(
            reloaded.status,
            MoleculeStatus::Completed,
            "terminal molecule must never be rewritten by purge"
        );
        assert!(reloaded.collapse_cause.is_none());
    }

    #[test]
    fn test_purge_targeted_force_collapses_running_molecule() {
        // `cs purge <worker> --force` on a worker whose molecule is still
        // running leaves the same crash zombie as the sweep stale path —
        // flip it to Collapsed / ProcessDeath.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("verify-20260621-2b67", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("force-target", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("force-target".to_owned()),
            force: true,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run(&ctx, &args).unwrap();

        let fleet = store.load_fleet().unwrap();
        assert!(!fleet
            .workers
            .contains_key(&WorkerId::new("force-target").unwrap()));

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Collapsed);
        assert_eq!(
            reloaded.collapse_cause,
            Some(cosmon_core::molecule::CollapseCause::ProcessDeath)
        );
    }

    #[test]
    fn test_purge_targeted_leaves_completed_molecule_untouched() {
        // Symmetric guard for the targeted path: a Completed molecule is
        // not a zombie and must survive a targeted purge unchanged.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());

        let mol = sample_mol("task-20260621-keep", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let mut fleet = Fleet::new();
        let w = worker_with_mol("target-done", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: Some("target-done".to_owned()),
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run(&ctx, &args).unwrap();

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Completed);
        assert!(reloaded.collapse_cause.is_none());
    }

    // -----------------------------------------------------------------
    // Fail-closed on unharvested work (incident 2026-08-02, task-…-7c43)
    // -----------------------------------------------------------------

    /// A stale worker (dead pane) bound to a running molecule whose branch is
    /// ahead of base. This is the whole incident in one fixture.
    fn stale_worker_with_running_molecule(
        store: &dyn StateStore,
        mol_id: &str,
        worker_id: &str,
    ) -> MoleculeId {
        let mol = sample_mol(mol_id, MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();
        let mut fleet = Fleet::new();
        let w = worker_with_mol(worker_id, mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();
        mol.id
    }

    #[test]
    fn test_sweep_refuses_to_collapse_molecule_with_commits_ahead() {
        // THE regression test for 2026-08-02 09:19: the tmux server vanished
        // with the machine reboot, so every pane read dead, and `cs purge`
        // turned four running molecules into `collapsed` — while their
        // branches were 1, 2 and 3 commits ahead of main. A purge over a dead
        // roster whose branch is ahead must NEVER produce `collapsed` without
        // an explicit flag.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol_id = stale_worker_with_running_molecule(&store, "task-20260802-16bf", "w-16bf");

        let backend = MockBackend::new(); // not registered → dead pane → stale
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        let err = run_sweep(&ctx, &store, tmp.path(), &backend, &commits_ahead(1), &args)
            .expect_err("purge must fail closed on unharvested work");

        // The molecule survives, still running.
        let reloaded = store.load_molecule(&mol_id).unwrap();
        assert_eq!(
            reloaded.status,
            MoleculeStatus::Running,
            "a dead pane is not evidence that the work failed"
        );
        assert!(reloaded.collapse_cause.is_none());

        // The worker entry survives, so the board keeps showing the molecule.
        let fleet = store.load_fleet().unwrap();
        assert!(
            fleet
                .workers
                .contains_key(&WorkerId::new("w-16bf").unwrap()),
            "the withheld worker must stay in the fleet"
        );

        // The alert names what is at stake.
        let msg = err.to_string();
        assert!(
            msg.contains("task-20260802-16bf"),
            "alert must name the molecule; got: {msg}"
        );
        assert!(
            msg.contains("1 commit"),
            "alert must name the commits; got: {msg}"
        );
        assert!(
            msg.contains("--allow-unharvested"),
            "alert must name the remedy; got: {msg}"
        );
    }

    #[test]
    fn test_sweep_refuses_when_worktree_is_dirty() {
        // task-…-7582's shape: nothing committed, three modified files still
        // in the worktree. Equally unharvested, equally not collapsible.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol_id = stale_worker_with_running_molecule(&store, "task-20260802-7582", "w-7582");

        let probe = Unharvested(UnharvestedWork {
            branch: "feat/task-20260802-7582".to_owned(),
            commits_ahead: 0,
            dirty_files: vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()],
            probe_error: None,
        });
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        let err = run_sweep(&ctx, &store, tmp.path(), &MockBackend::new(), &probe, &args)
            .expect_err("a dirty worktree must fail closed too");

        assert_eq!(
            store.load_molecule(&mol_id).unwrap().status,
            MoleculeStatus::Running
        );
        let msg = err.to_string();
        assert!(
            msg.contains("src/a.rs"),
            "alert must name the files; got: {msg}"
        );
    }

    #[test]
    fn test_sweep_with_allow_unharvested_collapses_as_before() {
        // The explicit gesture: the operator states the loss is acceptable,
        // and the pre-fix behaviour is restored exactly.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol_id = stale_worker_with_running_molecule(&store, "task-20260802-0c2d", "w-0c2d");

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: true,
        };
        run_sweep(
            &ctx,
            &store,
            tmp.path(),
            &MockBackend::new(),
            &commits_ahead(2),
            &args,
        )
        .unwrap();

        assert_eq!(
            store.load_molecule(&mol_id).unwrap().status,
            MoleculeStatus::Collapsed,
            "--allow-unharvested must restore the collapsing sweep"
        );
        assert!(store.load_fleet().unwrap().workers.is_empty());
    }

    #[test]
    fn test_sweep_still_purges_the_harvested_workers_alongside_a_withheld_one() {
        // The guard is per-worker, not per-sweep: withholding one molecule
        // must not strand the rest of the roster. The probe answers for the
        // molecule-bound worker only; the terminal worker has no molecule.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol_id = stale_worker_with_running_molecule(&store, "task-20260802-16bf", "w-16bf");

        let mut fleet = store.load_fleet().unwrap();
        let retired = worker("retired", WorkerStatus::Stopped, DesiredState::Stopped);
        fleet.workers.insert(retired.id.clone(), retired);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        let _ = run_sweep(
            &ctx,
            &store,
            tmp.path(),
            &MockBackend::new(),
            &commits_ahead(1),
            &args,
        );

        let fleet = store.load_fleet().unwrap();
        assert!(
            !fleet
                .workers
                .contains_key(&WorkerId::new("retired").unwrap()),
            "the clean terminal worker must still be purged"
        );
        assert!(fleet
            .workers
            .contains_key(&WorkerId::new("w-16bf").unwrap()));
        assert_eq!(
            store.load_molecule(&mol_id).unwrap().status,
            MoleculeStatus::Running
        );
    }

    #[test]
    fn test_targeted_purge_refuses_unharvested_work_even_with_force() {
        // `--force` is a statement about the tmux session, not about the
        // work. Without `--allow-unharvested` the targeted path must refuse
        // too — otherwise the guard is one `--force` away from useless.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol_id = stale_worker_with_running_molecule(&store, "task-20260802-0c2d", "target");

        let ctx = ctx_for(&tmp, false);
        let err = run_targeted(
            &ctx,
            &store,
            tmp.path(),
            "target",
            true,
            false,
            &commits_ahead(2),
        )
        .expect_err("targeted --force must not discard unharvested work");

        assert_eq!(
            store.load_molecule(&mol_id).unwrap().status,
            MoleculeStatus::Running
        );
        assert!(
            store
                .load_fleet()
                .unwrap()
                .workers
                .contains_key(&WorkerId::new("target").unwrap()),
            "a refused targeted purge must leave state untouched"
        );
        assert!(err.to_string().contains("--allow-unharvested"));
    }

    #[test]
    fn test_targeted_purge_proceeds_when_work_is_harvested() {
        // Control: the guard is silent when there is nothing to lose.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol_id = stale_worker_with_running_molecule(&store, "task-20260802-clea", "clean");

        let ctx = ctx_for(&tmp, false);
        run_targeted(&ctx, &store, tmp.path(), "clean", false, false, &NoWork).unwrap();

        assert!(store.load_fleet().unwrap().workers.is_empty());
        assert_eq!(
            store.load_molecule(&mol_id).unwrap().status,
            MoleculeStatus::Collapsed,
            "a harvested molecule is still zombie-flipped as before"
        );
    }

    #[test]
    fn test_terminal_molecule_is_not_withheld() {
        // The guard exists to protect a collapse that would lose work. A
        // molecule that is already terminal is not going to be collapsed, so
        // its worker must still be swept even if a branch lingers.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol = sample_mol("task-20260802-done", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();
        let mut fleet = Fleet::new();
        let w = worker_with_mol("done-worker", mol.id.as_str());
        fleet.workers.insert(w.id.clone(), w);
        store.save_fleet(&fleet).unwrap();

        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        run_sweep(
            &ctx,
            &store,
            tmp.path(),
            &MockBackend::new(),
            &commits_ahead(3),
            &args,
        )
        .unwrap();

        assert!(store.load_fleet().unwrap().workers.is_empty());
    }

    #[test]
    fn test_unprobeable_branch_is_treated_as_unharvested() {
        // Fail-closed applies to the probe itself: "I could not check" must
        // not read the same as "there is nothing there".
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        let mol_id = stale_worker_with_running_molecule(&store, "task-20260802-dark", "w-dark");

        let probe = Unharvested(UnharvestedWork {
            branch: "feat/task-20260802-dark".to_owned(),
            commits_ahead: 0,
            dirty_files: Vec::new(),
            probe_error: Some("git rev-list failed".to_owned()),
        });
        let ctx = ctx_for(&tmp, false);
        let args = Args {
            worker: None,
            force: false,
            status: None,
            role: None,
            allow_unharvested: false,
        };
        assert!(run_sweep(&ctx, &store, tmp.path(), &MockBackend::new(), &probe, &args).is_err());
        assert_eq!(
            store.load_molecule(&mol_id).unwrap().status,
            MoleculeStatus::Running
        );
    }

    #[test]
    fn test_git_probe_sees_commits_ahead_and_dirty_worktree() {
        // The probe half, against a real repository: everything above trusts
        // `HarvestProbe`, so the git implementation needs its own witness.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-C", &repo.to_string_lossy()])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.invalid"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("seed"), "seed").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "seed"]);

        let mol = MoleculeId::new("task-20260802-16bf").unwrap();
        let wt = repo.join(".worktrees").join(mol.as_str());
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/task-20260802-16bf",
            &wt.to_string_lossy(),
        ]);

        let probe = GitHarvestProbe {
            repo_root: Some(repo.clone()),
            configured_trunk: Some("main".to_owned()),
        };
        assert_eq!(
            probe.unharvested(&mol, Some("main")),
            None,
            "a fresh branch with a clean worktree holds nothing"
        );

        // One uncommitted file → unharvested.
        std::fs::write(wt.join("deliverable.md"), "work").unwrap();
        let dirty = probe.unharvested(&mol, Some("main")).unwrap();
        assert_eq!(dirty.dirty_files, vec!["deliverable.md".to_owned()]);
        assert_eq!(dirty.commits_ahead, 0);

        // Commit it → still unharvested, now as a commit ahead of main.
        let wt_git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(["-C", &wt.to_string_lossy()])
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        };
        wt_git(&["add", "-A"]);
        wt_git(&["commit", "-qm", "deliverable"]);
        let ahead = probe.unharvested(&mol, Some("main")).unwrap();
        assert_eq!(ahead.commits_ahead, 1);
        assert!(ahead.dirty_files.is_empty());
        assert_eq!(ahead.branch, "feat/task-20260802-16bf");
    }
}
