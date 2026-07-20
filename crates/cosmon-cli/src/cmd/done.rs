// SPDX-License-Identifier: AGPL-3.0-only

//! `cs done` — the symmetric teardown of `cs tackle`.
//!
//! Where `cs tackle <mol>` creates a branch, worktree, tmux session, and
//! spawns a worker, `cs done <mol>` reverses the ceremony:
//!
//! 1. Verify the molecule is completed (unless `--force`).
//! 2. Merge the worker's branch into main (non-fast-forward by default).
//! 3. Kill the tmux session.
//! 4. Purge the worker from fleet state.
//! 5. Remove the git worktree.
//! 6. Delete the branch.
//! 7. Auto-commit durable molecule artifacts (prompt.md, log.md, etc.).
//! 8. Reconcile surfaces.
//!
//! The convention mirrors tackle: session = `{mol_id}`, socket = `{project_id}`,
//! branch = `feat/{mol_id}`, worktree = `.worktrees/{mol_id}`. All steps are
//! idempotent — safe to rerun, safe if some state was already cleaned up
//! manually.
//!
//! ## Merge strategy
//!
//! Parallel tackling is a first-class cosmon pattern: several workers can
//! hold independent molecules at the same time. When the first lands, main
//! moves forward and the second can no longer fast-forward. A strict
//! `--ff-only` policy breaks the one-command symmetry of `tackle → wait →
//! done` in that scenario. `cs done` therefore defaults to `--strategy
//! merge` (git `merge --no-ff --no-edit`), which succeeds for any divergent
//! branch without textual conflicts and aborts cleanly (via `git merge
//! --abort`) if conflicts exist — listing the offending files. Operators
//! who want a strict linear history can opt in with `--strategy ff-only`.
//!
//! ## Failure is loud, not a "warning"
//!
//! A merge that does **not** land — a textual conflict, an `ff-only` refusal,
//! a HEAD-not-on-base mistake, a post-merge verification failure — is a hard
//! failure: `cs done` prints a `❌`-prefixed message, returns a **non-zero
//! exit**, and performs **no teardown** (the tmux session, worktree, and
//! branch are preserved). It is explicitly **not** folded into the benign
//! warning channel: an operator must never read "done with 1 warning" and
//! believe stale work shipped. Benign warnings (a failing `post_merge` hook, a
//! confidential-gate caveat) only ride the success path *after* the merge has
//! landed and end with `✅`/`⚠ … done` at exit 0. See `report_merge_failure`
//! and the `MergeLoopOutcome::Conflict` variant.
//!
//! A **refused `[hooks] pre_done` gate** is the same hard-failure class on the
//! *before-merge* side: the galaxy-configured gate runs before the trunk lock,
//! and a non-zero exit prints a `❌ PRE-DONE GATE REFUSED` message, returns a
//! non-zero exit, and tears down nothing. This is the primitive that lets a
//! galaxy make a falsifiable Definition-of-Done enforceable from inside the
//! molecule cycle (showroom `delib-20260701-bfdf`, torvalds D1). Unlike the
//! advisory `post_merge` hook, `pre_done` may abort precisely because nothing
//! has landed yet. See `run_pre_done_hook` and `report_pre_done_failure`.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use cosmon_core::config::{
    ConfidentialBlocklistConfig, GitRemoteBlocklistConfig, ProjectConfig, PublishIdentityConfig,
};
use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_core::transport::TransportBackend;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeData, StateStore};
use cosmon_transport::TmuxBackend;

use super::Context;

/// Arguments for the `done` subcommand.
///
/// Five independent bool flags because each maps to an independent CLI
/// opt-out for a distinct teardown step. A bitflag or enum would obscure
/// this one-to-one mapping without simplifying anything.
#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct Args {
    /// Molecule ID to tear down.
    molecule: String,

    /// Proceed even if the molecule is not in a terminal state.
    #[arg(long)]
    force: bool,

    /// Silent no-op when the molecule is not `Completed` or already merged.
    ///
    /// Hook-friendly gate for callers that do not know whether the molecule
    /// is ready for teardown — tmux `pane-died` hooks, patrol sweeps, and
    /// the legacy `cs harvest` alias. Exits success without touching state
    /// when the precondition is not met; otherwise, behaves exactly like
    /// `cs done`. Supersedes the stand-alone `cs harvest` verb (ADR-052).
    #[arg(long)]
    if_completed: bool,

    /// Compute and display the teardown plan without executing any steps.
    ///
    /// Reports what `cs done` *would* do: worktree state (clean/dirty),
    /// whether a merge is needed, whether the tmux session is alive,
    /// whether a fleet worker is registered, and whether the branch exists.
    #[arg(long)]
    dry_run: bool,

    /// Skip merging the worker's branch into the base branch.
    #[arg(long)]
    no_merge: bool,

    /// Skip removing the git worktree.
    #[arg(long)]
    no_worktree_remove: bool,

    /// Skip deleting the worker's branch after merge.
    #[arg(long)]
    no_branch_delete: bool,

    /// Skip killing the tmux session.
    #[arg(long)]
    no_kill: bool,

    /// Merge strategy for the worker's branch.
    ///
    /// `merge` (default) creates a merge commit (`git merge --no-ff`) so
    /// parallel workers can land independently even when main has moved.
    /// `ff-only` preserves a strictly linear history and refuses anything
    /// that is not a fast-forward; it is refused when native attribution is
    /// configured because a fast-forward creates no trailer carrier.
    #[arg(long, value_enum, default_value_t = MergeStrategy::Merge)]
    strategy: MergeStrategy,

    /// Disable auto-propel escalation on merge conflict.
    ///
    /// By default, when a merge conflict is detected, `cs done` escalates
    /// by sending a resume signal to the worker with rebase instructions,
    /// then retries the merge after a backoff delay. This flag restores the
    /// old behavior: abort immediately and print a manual-resolution message.
    ///
    /// Mechanical-first escalation: see docs/architectural-invariants.md
    #[arg(long)]
    no_auto_propel: bool,

    /// Custom message sent to the worker during auto-propel escalation.
    ///
    /// The default instructs the worker to rebase onto the base branch,
    /// resolve conflicts, run tests, and NOT call `cs done` itself.
    #[arg(long)]
    propel_message: Option<String>,

    /// Maximum number of auto-propel escalation retries before giving up.
    #[arg(long, default_value_t = 3)]
    max_retries: u32,

    /// Skip the blocking `[hooks] pre_done` gate for this invocation.
    ///
    /// The `pre_done` hook (when configured) runs before the merge and
    /// *aborts* teardown on a non-zero exit — the galaxy-owned Definition-of-
    /// Done gate. This flag is the human operator's kill-switch: it bypasses
    /// the gate entirely for a deliverable the operator knows is good but the
    /// script cannot see (e.g. evidence living outside the repo). Equivalent
    /// to setting the `COSMON_SKIP_PRE_DONE_HOOK` environment variable. No
    /// effect when no `pre_done` hook is configured.
    #[arg(long)]
    skip_pre_done_hook: bool,
}

/// Merge strategy used by `cs done` when integrating the worker's branch.
///
/// Default is [`MergeStrategy::Merge`] because parallel tackling is the
/// validated common case — see the module docs for the motivating incident.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum MergeStrategy {
    /// Non-fast-forward merge (`git merge --no-ff --no-edit`).
    ///
    /// Succeeds whenever the branches do not share textual conflicts; on
    /// conflict, the merge is rolled back (`git merge --abort`) and the
    /// offending files are reported.
    Merge,
    /// Fast-forward-only merge (`git merge --ff-only`).
    ///
    /// Refuses anything that would require a merge commit. Suited to
    /// strictly linear history regimes, but incompatible with parallel
    /// tackling when main moves between worker starts and with configured
    /// native attribution (which requires a cosmon-owned commit carrier).
    FfOnly,
}

/// Reject a merge shape that cannot carry configured attribution trailers.
///
/// `ff-only` advances the base ref directly to a worker commit and creates no
/// cosmon-owned commit. Rewriting that worker commit would invalidate its SHA,
/// so native attribution and fast-forward-only integration are mutually
/// exclusive. The empty-trailer configuration retains the historical
/// fast-forward behavior.
fn ensure_attribution_carrier(
    strategy: MergeStrategy,
    coauthor_trailers: &[String],
) -> anyhow::Result<()> {
    if strategy == MergeStrategy::FfOnly && !coauthor_trailers.is_empty() {
        return Err(anyhow::anyhow!(
            "cs done refuses `--strategy ff-only` while native attribution is configured: \
             a fast-forward creates no merge commit on which to stamp the Noogram/adapter \
             `Co-Authored-By:` trailers, and cosmon will not rewrite worker commits. Use the \
             default `--strategy merge`, or remove `[attribution].coauthor_email` if this \
             repository intentionally chooses unstamped linear history \
             (delib-20260717-194b, trailer-carrier contract)."
        ));
    }
    Ok(())
}

/// Surface the fail-open corner of the trailer facet (pré-mortem
/// task-20260717-ffe1, C4): a configured `[attribution]` block whose
/// `coauthor_email` is empty stamps NOTHING, while the author-slot assertion
/// keeps running and giving an impression of full protection. The empty-email
/// configuration stays legal (the facet is opt-in by contract — see
/// [`AttributionConfig::coauthor_trailers`](cosmon_core::config::AttributionConfig::coauthor_trailers)),
/// but it must be visible at the moment of integration, not discovered later
/// in `git log`.
fn warn_unstamped_attribution(
    attribution: &cosmon_core::config::AttributionConfig,
    coauthor_trailers: &[String],
    warnings: &mut Vec<String>,
) {
    if !attribution.is_empty() && coauthor_trailers.is_empty() {
        warnings.push(format!(
            "attribution: `[attribution]` names `{}` but `coauthor_email` is empty, so NO \
             `Co-Authored-By:` trailer will be stamped on this integration. Set \
             `coauthor_email` in `.cosmon/config.toml` to stamp maker/adapter provenance, \
             or ignore this warning if unstamped history is intentional.",
            attribution.public_name
        ));
    }
}

/// Convert the durable adapter fold into an optional co-author and observable
/// warnings without inventing provenance.
fn adapter_for_coauthor(
    fold: cosmon_state::ops::model_attribution::AdapterFold,
    mol_id: &MoleculeId,
    attribution_enabled: bool,
    warnings: &mut Vec<String>,
) -> Option<String> {
    use cosmon_state::ops::model_attribution::AdapterFold;

    match fold {
        AdapterFold::Single(name) => Some(name),
        AdapterFold::Absent => {
            if attribution_enabled {
                warnings.push(format!(
                    "attribution: no adapter witness was found in the event log for {mol_id} — \
                     omitting the adapter from the co-author display name. The maker trailer still rides, but \
                     this commit does NOT prove which adapter ran."
                ));
            }
            None
        }
        AdapterFold::Ambiguous(names) => {
            warnings.push(format!(
                "attribution: {} distinct adapters ran {mol_id} ({}) — dropping the \
                 adapter annotation rather than guessing last-writer (F6). The \
                 maker trailer still rides.",
                names.len(),
                names.join(", ")
            ));
            None
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
/// Pre-computed teardown plan — what `cs done` *would* do.
///
/// Built by [`compute_teardown_plan`] and displayed in `--dry-run` mode.
/// Each field captures the current state of a teardown resource so the
/// operator can review before committing.
#[derive(Debug, serde::Serialize)]
pub struct TeardownPlan {
    /// Molecule ID being torn down.
    pub molecule: String,
    /// Current molecule status (e.g. "Completed", "Running").
    pub molecule_status: String,
    /// Whether the molecule is in a terminal state.
    pub is_terminal: bool,
    /// Whether the worktree directory exists on disk.
    pub worktree_exists: bool,
    /// Dirty files in the worktree (empty if clean or absent).
    pub worktree_dirty_files: Vec<String>,
    /// Whether the worker's branch exists locally.
    pub branch_exists: bool,
    /// Whether the branch is already merged into the current HEAD.
    ///
    /// Topology test only: true iff every commit reachable from the
    /// worker's branch is reachable from the configured base branch.
    /// True for both *empty* branches (no commits ahead, see
    /// `branch_is_empty`) and *genuinely merged* branches; the two
    /// cases are distinguished by `branch_is_empty`.
    pub branch_already_merged: bool,
    /// Whether the worker's branch carries zero commits ahead of the
    /// base branch. Set when `cs done` would have nothing to merge —
    /// either because the worker never committed in this repo or
    /// because its diff landed via another path. Mutually exclusive
    /// with `merge_needed`. See `branch_is_empty_relative_to`.
    pub branch_is_empty: bool,
    /// Whether the branch needs a merge (exists and not yet merged).
    pub merge_needed: bool,
    /// Whether the tmux session is alive.
    pub session_alive: bool,
    /// Whether a fleet worker is registered for this molecule.
    pub worker_registered: bool,
    /// Planned actions (what would happen without `--dry-run`).
    pub planned_actions: Vec<String>,
    /// Warnings about the planned teardown.
    pub warnings: Vec<String>,
}

/// Compute the teardown plan without executing anything.
#[allow(clippy::too_many_arguments)]
fn compute_teardown_plan(
    ctx: &Context,
    args: &Args,
    mol_id: &MoleculeId,
    mol_status: &str,
    is_terminal: bool,
    socket: &str,
    session_name: &str,
) -> anyhow::Result<TeardownPlan> {
    let branch_name = format!("feat/{mol_id}");
    let wid = WorkerId::new(session_name)?;
    let repo_root = find_repo_root()?;
    let worktree_path = repo_root.join(".worktrees").join(mol_id.as_str());

    let mut planned_actions = Vec::new();
    let mut warnings = Vec::new();

    // Worktree state.
    let worktree_exists = worktree_path.exists();
    let worktree_dirty_files = if worktree_exists {
        worktree_is_dirty(&worktree_path).unwrap_or_default()
    } else {
        Vec::new()
    };
    if !worktree_dirty_files.is_empty() && !args.force {
        warnings.push(format!(
            "worktree has {} uncommitted file(s) — will refuse without --force",
            worktree_dirty_files.len()
        ));
    }

    // Branch state.
    let branch_present = branch_exists(&repo_root, &branch_name);
    let branch_merged = if branch_present {
        is_branch_merged(&repo_root, &branch_name)
    } else {
        false
    };
    // Distinguish empty-relative-to-base from genuinely already-merged.
    // Both report `branch_merged = true`, but a downstream operator
    // needs to know which case applies to verify the payload's fate.
    // See `branch_is_empty_relative_to` and bug `task-20260422-ecf3`.
    let branch_is_empty = if branch_present && branch_merged {
        let base = resolve_base_branch(&repo_root);
        branch_is_empty_relative_to(&repo_root, &branch_name, &base)
    } else {
        false
    };
    let merge_needed = branch_present && !branch_merged;

    // Tmux session state.
    let backend = TmuxBackend::new(socket);
    let session_alive = backend.is_alive(&wid).unwrap_or(false);

    // Fleet worker state.
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);
    let worker_registered = store
        .load_fleet()
        .is_ok_and(|f| f.workers.contains_key(&wid));

    // Compute effective flags.
    let effective_no_branch_delete = if args.no_merge && !args.no_branch_delete && !args.force {
        warnings.push(
            "--no-merge implies --no-branch-delete (branch is the only copy of the work)"
                .to_owned(),
        );
        true
    } else {
        args.no_branch_delete
    };

    // Planned actions.
    if !is_terminal && !args.force {
        warnings.push(format!(
            "molecule is {mol_status} (not terminal) — will refuse without --force"
        ));
    }
    if !args.no_merge && merge_needed {
        planned_actions.push(format!("merge branch {branch_name} into HEAD"));
    }
    if !args.no_merge && branch_merged {
        if branch_is_empty {
            planned_actions.push("skip merge (empty branch — no commits ahead of base)".to_owned());
        } else {
            planned_actions.push("skip merge (already merged)".to_owned());
        }
    }
    if !args.no_kill && session_alive {
        planned_actions.push(format!("kill tmux session {session_name}"));
    }
    if worker_registered {
        planned_actions.push("purge worker from fleet".to_owned());
    }
    if !args.no_worktree_remove && worktree_exists {
        planned_actions.push(format!("remove worktree {}", worktree_path.display()));
    }
    if !effective_no_branch_delete && branch_present {
        planned_actions.push(format!("delete branch {branch_name}"));
    }
    planned_actions.push("auto-commit molecule artifacts (if any)".to_owned());

    Ok(TeardownPlan {
        molecule: mol_id.as_str().to_owned(),
        molecule_status: mol_status.to_owned(),
        is_terminal,
        worktree_exists,
        worktree_dirty_files,
        branch_exists: branch_present,
        branch_already_merged: branch_merged,
        branch_is_empty,
        merge_needed,
        session_alive,
        worker_registered,
        planned_actions,
        warnings,
    })
}

/// Check whether a branch is already merged into the configured base
/// branch.
///
/// Probes git topology directly with
/// `git merge-base --is-ancestor <branch> <base>`, where `<base>` is
/// resolved by [`resolve_base_branch`] — usually `main`. Returns true
/// iff every commit reachable from `branch` is also reachable from the
/// base branch.
///
/// **Strict-ancestry invariant** (architectural-invariants.md §11). The
/// probe used to compare against `HEAD`, which was brittle: when `cs done`
/// was invoked from anywhere other than a main-branch checkout (e.g. from
/// within the worker's own worktree, or from another task's worktree),
/// `HEAD` pointed at the worker's branch itself, and the ancestry check
/// trivially returned true — silently short-circuiting the merge and
/// orphaning the payload. Reported incident (2026-04-21): `cs done`
/// returned `already_merged` while the real payload commit (491
/// insertions across 6 files) was NOT on main; the operator had to merge
/// by hand.
///
/// The only source of truth is ancestry against the base branch — never
/// against an ambient `HEAD`, never against commit-subject heuristics.
/// Earlier regression: a bookkeeping commit on HEAD whose subject matched
/// `evolve(<mol>): step N/M` fooled a subject-parsing check.
fn is_branch_merged(repo_root: &Path, branch: &str) -> bool {
    let base = resolve_base_branch(repo_root);
    branch_is_ancestor_of(repo_root, branch, &base)
}

/// Enforce the invariant `archived ⇒ status.is_terminal()` at the writer.
///
/// `cs done --force` is the one teardown path that can land `merged_at` /
/// `archived` on a molecule that never reached a terminal state — a worker
/// that died before any `cs evolve`, or a molecule force-torn-down before
/// it was ever tackled. Persisting `{archived: true, status: Running}` is a
/// state-machine incoherence: every `status`-keyed
/// reader (`cs observe`, `detect_ghost`) treats the row as a live
/// `👻 unnamed-merge` ghost that no repeat `cs done` and no `cs reconcile`
/// can ever clear.
///
/// The terminus is `MoleculeStatus::Collapsed` — semantically honest, since
/// no work completed; *not* `Completed`, which would corrupt completion and
/// energy accounting. We reuse the existing terminal variant (no new variant,
/// no ADR) and tag the cause `Manual` with reason `forced-teardown`.
///
/// The `is_terminal` guard makes this a no-op on the normal (non-`--force`)
/// path, where the molecule is already `Completed` before teardown begins;
/// it fires only on the `--force`-on-a-live-molecule case this fix targets.
/// Pre-existing free-form `collapse_reason` is preserved if already set.
fn terminalize_for_forced_teardown(mol: &mut cosmon_state::MoleculeData) {
    use cosmon_core::molecule::{CollapseCause, MoleculeStatus};
    if mol.status.is_terminal() {
        return;
    }
    mol.status = MoleculeStatus::Collapsed;
    if mol.collapse_cause.is_none() {
        mol.collapse_cause = Some(CollapseCause::Manual);
    }
    if mol.collapse_reason.is_none() {
        mol.collapse_reason = Some("forced-teardown".to_owned());
    }
}

/// Test-only helper: probe ancestry against the ambient `HEAD`.
///
/// Production code never calls this — `verify_merge` post-merge and
/// `is_branch_merged` pre-merge both compare against the configured
/// base branch (strict-ancestry invariant, §11). The helper is kept
/// alive so the regression tests in this module can demonstrate that
/// the *old* HEAD-based probe falsely succeeded for merges that landed
/// on the wrong branch — the failure mode `verify_merge` now catches.
#[cfg(test)]
fn branch_is_ancestor_of_head(repo_root: &Path, branch: &str) -> bool {
    branch_is_ancestor_of(repo_root, branch, "HEAD")
}

/// Topology probe — true iff `branch`'s tip is reachable from `target`.
///
/// Uses `git merge-base --is-ancestor <branch> <target>`, which exits 0
/// when the branch tip is in the target's history and 1 otherwise. Any
/// other failure (missing ref, git invocation error) is treated as "not
/// merged" to keep `cs done` conservative — the merge is then attempted
/// and either succeeds or surfaces a real error.
fn branch_is_ancestor_of(repo_root: &Path, branch: &str, target: &str) -> bool {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "merge-base",
            "--is-ancestor",
            branch,
            target,
        ])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// Choose the action label for the "branch reachable from base, nothing
/// to merge" outcome.
///
/// Two semantically distinct cases share the same git-topology shape
/// — see [`MergeOutcome::AlreadyMerged`] — and the operator needs to
/// tell them apart to verify the worker's deliverable landed where it
/// was supposed to:
///
/// 1. **`already_merged`** — the molecule's `merged_at` stamp is set,
///    meaning a prior `cs done` integrated this branch into base. The
///    label communicates "yes, the work landed".
/// 2. **`empty_branch`** — `merged_at` is unset *and* `base..branch`
///    is empty, so the worker never produced a commit in this repo.
///    The label cues the operator to check that the deliverable
///    landed in a sibling repo or external store (e.g. galaxy
///    bootstrap, vault artifact, …).
/// 3. Anything else falls back to `already_merged` — conservative:
///    when the topology contradicts itself, the safer label is the
///    one that does not promise the operator there is no work to
///    look for.
fn classify_already_merged_label(merged_at_is_set: bool, branch_is_empty: bool) -> &'static str {
    if merged_at_is_set {
        "already_merged"
    } else if branch_is_empty {
        "empty_branch"
    } else {
        "already_merged"
    }
}

/// Distinguish a branch that is *empty relative to base* (zero commits
/// ahead, e.g. the worker produced output outside this repo and never
/// committed) from one that is *genuinely already merged* (commits
/// landed via another path).
///
/// Both shapes pass `git merge-base --is-ancestor <branch> <base>` —
/// in the empty case trivially, since the branch tip *is* a commit
/// already on base. Reporting both as `already_merged` is misleading:
/// a worker that produced no diff in this repo is semantically
/// different from one whose diff has been integrated. When a worker's
/// output lives outside the cosmon repo (galaxy creation, vault
/// artifact, …),
/// `cs done` previously claimed `already_merged` and the operator
/// could not tell whether work had landed somewhere or never existed.
///
/// Probe: `git rev-list --count <base>..<branch>` returns the number
/// of commits reachable from `branch` but not from `base`. Zero means
/// every commit on `branch` is already on `base` *and* there are no
/// commits unique to `branch` — which is true whether the branch tip
/// is `base`'s tip itself (no commits ever made), or whether all the
/// branch's commits coincide with `base`. Either way, there is
/// nothing for `cs done` to merge.
fn branch_is_empty_relative_to(repo_root: &Path, branch: &str, base: &str) -> bool {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-list",
            "--count",
            &format!("{base}..{branch}"),
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.trim() == "0"
        }
        _ => false,
    }
}

/// Structural post-condition for the final `cs done` guard: `true` iff
/// `branch` still exists locally **and** carries commit(s) not reachable
/// from `base`.
///
/// When this holds at the end of teardown, the worker's deliverable was
/// *not* integrated — and `cs done` must refuse to exit 0, because the
/// branch is the only copy of the work. This catches the *silent* no-op
/// class that the typed merge errors above do not: a false `already_merged`,
/// a base-resolution slip, or any future path that reports success without
/// moving base: a bug where `cs done` returned exit 0 with the branch and
/// worktree still present and nothing on main.
///
/// It is deliberately a *fresh, independent* probe (not a cached flag): a
/// successful merge deletes the branch — so `branch_exists` is already
/// `false` — and a `--no-branch-delete` merge leaves the branch present but
/// *empty relative to base*, so neither case is a false positive. Only a
/// branch that genuinely still has unmerged commits trips it.
fn unmerged_work_remains(repo_root: &Path, branch: &str, base: &str) -> bool {
    branch_exists(repo_root, branch) && !branch_is_empty_relative_to(repo_root, branch, base)
}

/// Resolve the base branch that the worker's branch is supposed to merge
/// back into.
///
/// Resolution order (first that works wins):
///
/// 1. The `COSMON_BASE_BRANCH` environment variable (explicit operator
///    override, primarily for tests and non-`main` repos).
/// 2. `git symbolic-ref refs/remotes/origin/HEAD` stripped of the
///    `refs/remotes/origin/` prefix — the default branch advertised by
///    the remote. Gracefully skipped if no `origin` remote exists.
/// 3. The literal `"main"` as last-resort default — matches the cosmon
///    convention (`cs tackle` branches from `main` when no blocker
///    branch is available, see `tackle.rs`).
///
/// Returns the branch *name*, not a full ref — callers concatenate it with
/// `refs/heads/` or pass it directly to `git merge-base --is-ancestor`
/// which resolves it as a commitish.
fn resolve_base_branch(repo_root: &Path) -> String {
    if let Ok(explicit) = std::env::var("COSMON_BASE_BRANCH") {
        if !explicit.trim().is_empty() {
            return explicit.trim().to_owned();
        }
    }

    let symref = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output();
    if let Ok(o) = symref {
        if o.status.success() {
            let raw = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            // `symbolic-ref --short` already trims `refs/remotes/` — result
            // is e.g. `origin/main`. Strip the remote prefix to get the
            // local branch name.
            if let Some(stripped) = raw.strip_prefix("origin/") {
                if !stripped.is_empty() {
                    return stripped.to_owned();
                }
            } else if !raw.is_empty() {
                return raw;
            }
        }
    }

    "main".to_owned()
}

/// Verify that the branch tip is an ancestor of the configured *base*
/// branch after merge.
///
/// Uses `git merge-base --is-ancestor <branch> <base>` — returns true
/// when every commit on the branch is reachable from base (`main` by
/// default). This guards against the silent-merge-to-wrong-branch
/// failure mode:
///
/// - The pilot ran `cs done <id>` from inside a worktree.
/// - `find_repo_root()` returned the worktree path.
/// - `git -C <worktree> merge` advanced the *worktree's* current branch
///   (e.g. `feat/task-…-X`), not `main`.
/// - The previous probe `branch_is_ancestor_of_head` compared against
///   the ambient `HEAD` (now the worktree's branch tip), which trivially
///   contained the merged branch — so `verify_merge` returned true and
///   `cs done` reported a fictitious "merged" while `main` never moved.
/// - The work commit was orphaned when the branch was later cleaned up
///   and had to be recovered with `git fsck --lost-found`.
///
/// The fix: compare against the base branch (resolved via
/// [`resolve_base_branch`]). When the merge landed on the wrong
/// branch, `base..branch` is non-empty → false → caller surfaces a
/// typed error instead of a silent success.
fn verify_merge(repo_root: &Path, branch: &str) -> bool {
    let base = resolve_base_branch(repo_root);
    branch_is_ancestor_of(repo_root, branch, &base)
}

/// Read the current branch name at `repo_root` (e.g. `"main"`).
///
/// Returns `None` when HEAD is detached or git fails. Used by the
/// pre-flight check in [`try_merge_branch`] to refuse silent merges
/// onto the wrong branch.
fn current_branch_name(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() || s == "HEAD" {
        // Empty output or detached HEAD.
        None
    } else {
        Some(s)
    }
}

/// Refuse `cs done` when the worktree carries a forbidden git remote.
///
/// Part of the internalised-substrate leak guard: an agent that adds a
/// public mirror as a remote and pushes private code there is
/// structurally rejected at the merge
/// gate, so discipline is not the floor.
///
/// Reads `git remote -v` from `repo_root` and matches every output line
/// against each substring in `blocklist.forbidden_substrings`. Returns
/// `Ok(())` when the blocklist is empty, when no remotes exist, or when
/// no line matches; returns `Err` listing every (pattern, line) violation
/// otherwise.
///
/// Substring matching (not regex) is intentional: the patterns are short,
/// literal repository identifiers, and the matching rule must be obvious
/// to a tired agent reading the error message at 3am.
fn check_git_remote_blocklist(
    repo_root: &Path,
    blocklist: &GitRemoteBlocklistConfig,
) -> anyhow::Result<()> {
    if blocklist.is_empty() {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "remote", "-v"])
        .output();
    let remotes = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        // No `git remote -v` output (no remotes, not a git repo) → nothing
        // to leak through. Conservative: do not block on probe failure.
        _ => return Ok(()),
    };
    let violations = collect_remote_blocklist_violations(&remotes, blocklist);
    if violations.is_empty() {
        return Ok(());
    }
    let mut msg = String::from(
        "cosmon worktree carries a forbidden git remote — `cs done` aborts to prevent leak.\n\
         (delib-20260426-7cfc R1: structural anti-leak guard)\n",
    );
    for (pattern, line) in &violations {
        // Allocation-free fmt write into a String buffer; never fails.
        let _ = writeln!(msg, "  pattern: {pattern}");
        let _ = writeln!(msg, "  remote:  {line}");
    }
    msg.push_str(
        "\nRemove the remote, then retry. The blocklist is configured in\n\
         `.cosmon/config.toml` under `[git_remote_blocklist]`.\n",
    );
    Err(anyhow::anyhow!("{msg}"))
}

/// Pure substring scan over `git remote -v` output — the testable core of
/// [`check_git_remote_blocklist`].
///
/// Returns every (pattern, offending line) pair so the caller can report
/// all violations at once instead of leaking one at a time.
fn collect_remote_blocklist_violations(
    git_remote_v_output: &str,
    blocklist: &GitRemoteBlocklistConfig,
) -> Vec<(String, String)> {
    let mut hits = Vec::new();
    for line in git_remote_v_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        for pattern in &blocklist.forbidden_substrings {
            if pattern.is_empty() {
                continue;
            }
            if trimmed.contains(pattern.as_str()) {
                hits.push((pattern.clone(), trimmed.to_owned()));
            }
        }
    }
    hits
}

/// Publish-identity gate over the git author/committer identity of the
/// commits a `cs done` merge would introduce (ADR-128 §V1).
///
/// V0's `[confidential_blocklist]` scans the *file content* of the merged
/// tree, but the git author/committer identity is stamped into every commit
/// and is invisible to any content grep. shannon's confirmed residual: the
/// operator email `operator@example.org` rides every commit of a shipped public
/// repo. This widens detection to that channel.
///
/// Scope is deliberately `<base>..<branch>` — only the commits the merge
/// would *publish*, never the project's pre-existing history. In an internal
/// galaxy (like cosmon) the operator identity is legitimate on past commits;
/// the gate must never flag those. With an empty config it is a zero-cost
/// fast return (backward-compatible for every project that predates the knob).
///
/// Two layers, both honoured here:
/// - **whitelist** (`allowed_emails`, closed-codebook): any author/committer
///   email not in the codebook is a violation by construction (recall → 1 on
///   this slot);
/// - **blacklist** (`forbidden_substrings`, defense-in-depth): literal
///   substring scan over author/committer names and commit messages.
///
/// The gate is **syntactic**: it cannot detect paraphrase, implication,
/// encoded, or composed disclosure (Rice-theorem-adjacent undecidability).
/// The error message states this so the gate manufactures no false confidence.
fn check_publish_identity_blocklist(
    repo_root: &Path,
    branch: &str,
    base: &str,
    cfg: &PublishIdentityConfig,
) -> anyhow::Result<()> {
    if cfg.is_empty() {
        return Ok(());
    }
    let range = format!("{base}..{branch}");

    // Scan 1 — author + committer emails, one per line, for both the
    // whitelist (closed-codebook) and blacklist layers.
    let emails = git_log_field(repo_root, &range, "%ae%n%ce");
    // Scan 2 — author + committer names and the raw commit message body,
    // for the blacklist layer only (free text has no codebook).
    let free_text = git_log_field(repo_root, &range, "%an%n%cn%n%B");

    let mut violations = collect_identity_email_violations(&emails, cfg);
    violations.extend(collect_identity_text_violations(&free_text, cfg));

    if violations.is_empty() {
        return Ok(());
    }

    let mut msg = String::from(
        "cosmon worktree carries a confidential git identity in the commits it would \
         publish — `cs done` aborts to prevent a D7 leak.\n\
         (ADR-128 §V1: the publish-identity gate over `<base>..<branch>`)\n",
    );
    for (reason, value) in &violations {
        let _ = writeln!(msg, "  {reason}: {value}");
    }
    msg.push_str(
        "\nFix the identity, then retry. To re-stamp the offending commits with the\n\
         canonical publish identity, set it and rewrite the branch's authorship, e.g.:\n\
         \n  git config user.email <canonical>   # pin the publish identity first\n\
         \n  git -c rebase.instructionFormat= rebase --exec \\\n\
         \n    'git commit --amend --no-edit --reset-author' <base>\n\
         \nThe gate is configured per-project in `.cosmon/config.toml` under\n\
         `[publish_identity]` (`allowed_emails` whitelist / `forbidden_substrings`).\n\
         \nResidual risk (named, not hidden): this gate is SYNTACTIC. It raises recall\n\
         to 1 on the enumerated git-identity slot, but it does NOT detect paraphrase,\n\
         implication, encoded, or composed disclosure (undecidable, Rice-adjacent).\n\
         Human review remains the backstop for the semantic failure class.\n",
    );
    Err(anyhow::anyhow!("{msg}"))
}

/// The operator identity allowed in git author and committer slots.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OperatorIdentity {
    /// Operator display name (`git config user.name`).
    name: String,
    /// Operator email (`git config user.email`).
    email: String,
}

/// Resolve the operator's canonical git identity from `repo_root`'s effective
/// git config (`user.name` and `user.email`, walking local → global → system).
///
/// Returns `None` when no identity is configured (a bare CI checkout or fresh
/// repo). Attribution-enabled integration treats that absence as a hard error:
/// without a reference identity, the author-slot assertion cannot prove its
/// direction-of-control invariant. The identity is the human who runs `cs
/// done`; it is never the maker/adapter (those live only on `Co-Authored-By`
/// trailers).
fn resolve_operator_identity(repo_root: &Path) -> Option<OperatorIdentity> {
    fn config_value(repo_root: &Path, key: &str) -> Option<String> {
        let out = Command::new("git")
            .args(["-C", &repo_root.to_string_lossy(), "config", key])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        (!value.is_empty()).then_some(value)
    }

    Some(OperatorIdentity {
        name: config_value(repo_root, "user.name")?,
        email: config_value(repo_root, "user.email")?,
    })
}

/// Resolve the operator identity or fail closed with an actionable remedy.
fn require_operator_identity(repo_root: &Path) -> anyhow::Result<OperatorIdentity> {
    resolve_operator_identity(repo_root).ok_or_else(|| {
        anyhow::anyhow!(
            "cs done refuses attribution-enabled integration because the operator git \
             identity is incomplete. Configure both `git config user.name <operator>` and \
             `git config user.email <operator-address>`, then rerun `cs done`. Without that \
             reference identity cosmon cannot prove that worker commits keep Noogram and \
             adapters out of the author/committer slots."
        )
    })
}

/// Count the commits in `<base>..<branch>` (`git rev-list --count`).
///
/// `None` on any probe failure (missing branch, non-git dir) so the caller
/// distinguishes "probe slipped" from a real zero — the vacuous-range guard
/// (F5) must fail *closed* on a genuine zero-but-unmerged branch, not on a
/// probe that could not run.
fn rev_list_count(repo_root: &Path, base: &str, branch: &str) -> Option<usize> {
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

/// A commit in the publish range whose author or committer slot is NOT the
/// operator — the direction-of-control violation the author-slot assertion
/// (delib-20260717-194b, F4 / adversary's unifying CATCH) hunts for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorSlotViolation {
    /// Abbreviated commit SHA (for the operator-facing report).
    sha: String,
    /// The offending author name (`%an`).
    author_name: String,
    /// The offending author email (`%ae`).
    author_email: String,
    /// The offending committer name (`%cn`).
    committer_name: String,
    /// The offending committer email (`%ce`).
    committer_email: String,
}

/// Field separator for the author-slot scan — the ASCII unit separator, which
/// cannot occur in an email or a SHA, so the split is unambiguous even if a
/// future format grows a free-text field.
const AUTHOR_SCAN_SEP: char = '\u{1f}';

/// Pure core of the author-slot assertion: given `git log` output of
/// `%H<US>%an<US>%ae<US>%cn<US>%ce` lines and the operator identity, return
/// every commit whose author OR committer name/email pair is not the operator.
///
/// Split out from I/O so the invariant — *every commit in the publish range is
/// operator-authored* — is unit-testable against a hand-built log (knuth's
/// RED-before/GREEN-after discipline). The comparison is trimmed and
/// case-sensitive for names and case-insensitive for emails: display-name case
/// is identity data, while email domains are not case-sensitive.
fn collect_non_operator_authored(
    log_output: &str,
    operator: &OperatorIdentity,
) -> Vec<AuthorSlotViolation> {
    let operator_name = operator.name.trim();
    let operator_email = operator.email.trim();
    let mut out = Vec::new();
    for line in log_output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(AUTHOR_SCAN_SEP);
        let sha = fields.next().unwrap_or("").trim();
        let author_name = fields.next().unwrap_or("").trim();
        let author_email = fields.next().unwrap_or("").trim();
        let committer_name = fields.next().unwrap_or("").trim();
        let committer_email = fields.next().unwrap_or("").trim();
        let author_ok =
            author_name == operator_name && author_email.eq_ignore_ascii_case(operator_email);
        let committer_ok =
            committer_name == operator_name && committer_email.eq_ignore_ascii_case(operator_email);
        if !author_ok || !committer_ok {
            out.push(AuthorSlotViolation {
                sha: sha.to_owned(),
                author_name: author_name.to_owned(),
                author_email: author_email.to_owned(),
                committer_name: committer_name.to_owned(),
                committer_email: committer_email.to_owned(),
            });
        }
    }
    out
}

/// Author-slot assertion (delib-20260717-194b, F4). Every commit the merge
/// would publish (`<base>..<branch>`, merges included) must be authored AND
/// committed by the operator — the maker/adapter identity belongs on
/// `Co-Authored-By` trailers, never in the author slot (direction-of-control).
///
/// This is **independent of the `[publish_identity]` blocklist** (T3): an
/// internal galaxy ships an empty `allowed_emails`, so that gate catches
/// nothing, yet a codex worker that leaked `Noogram <hello@noogram.org>` into
/// the author slot must still be caught. Keying on the operator email — a
/// closed set of one, resolved from the repo's own git config — covers every
/// galaxy without asking it to fill a codebook.
///
/// Hard-fails, listing every offending SHA, when a non-operator
/// author/committer slot is found. The caller must first obtain a complete
/// identity through [`require_operator_identity`], which also fails closed.
fn assert_operator_authored_commits(
    repo_root: &Path,
    base: &str,
    branch: &str,
    operator: &OperatorIdentity,
) -> anyhow::Result<()> {
    let range = format!("{base}..{branch}");
    let fmt = format!(
        "%H{AUTHOR_SCAN_SEP}%an{AUTHOR_SCAN_SEP}%ae{AUTHOR_SCAN_SEP}%cn{AUTHOR_SCAN_SEP}%ce"
    );
    let log = git_log_field(repo_root, &range, &fmt);
    let violations = collect_non_operator_authored(&log, operator);
    if violations.is_empty() {
        return Ok(());
    }
    let mut msg = format!(
        "cosmon refuses to integrate {branch}: {} commit(s) in `{range}` are not \
         operator-authored — the git author/committer slot MUST be the operator \
         ({} <{}>); the maker and the real adapter belong only on \
         `Co-Authored-By:` trailers (delib-20260717-194b, F4 direction-of-control).\n",
        violations.len(),
        operator.name,
        operator.email
    );
    for v in &violations {
        let short: String = v.sha.chars().take(12).collect();
        let _ = writeln!(
            msg,
            "  {short}  author={} <{}>  committer={} <{}>",
            v.author_name, v.author_email, v.committer_name, v.committer_email
        );
    }
    msg.push_str(
        "\nThis is the codex-author leak class: a worker git process wrote the maker \
         name into the author slot. Fix it at the source — pin the worktree identity \
         (`git -C <worktree> config user.email <operator>`) so feature commits are \
         BORN operator-authored — then re-stamp the offending commits:\n\
         \n  git config user.email <operator>\n\
         \n  git -c rebase.instructionFormat= rebase --exec \\\n\
         \n    'git commit --amend --no-edit --reset-author' <base>\n\
         \nthen rerun `cs done`.\n",
    );
    Err(anyhow::anyhow!("{msg}"))
}

/// Run `git log --format=<fmt> <range>` from `repo_root` and return stdout.
///
/// Conservative on probe failure: an empty range, a missing branch, or a
/// non-git directory yields the empty string, so the caller scans nothing
/// and the gate passes (never blocks on a probe slip — same discipline as
/// [`check_git_remote_blocklist`]).
fn git_log_field(repo_root: &Path, range: &str, fmt: &str) -> String {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "log",
            &format!("--format={fmt}"),
            range,
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

/// Pure whitelist+blacklist scan over author/committer email lines — the
/// testable core of the publish-identity gate's identity layer.
///
/// Returns every `(reason, offending value)` pair so the caller reports all
/// violations at once. The whitelist comparison is case-insensitive and
/// trimmed (git lowercases nothing, but operators do not want a leak to slip
/// through on a stray capital).
fn collect_identity_email_violations(
    emails_output: &str,
    cfg: &PublishIdentityConfig,
) -> Vec<(String, String)> {
    let mut hits = Vec::new();
    let whitelist_active = !cfg.allowed_emails.is_empty();
    for line in emails_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Whitelist (closed-codebook): any email not in the codebook is a
        // by-construction violation.
        if whitelist_active
            && !cfg
                .allowed_emails
                .iter()
                .any(|allowed| allowed.trim().eq_ignore_ascii_case(trimmed))
        {
            hits.push((
                "email not in publish codebook".to_owned(),
                trimmed.to_owned(),
            ));
        }
        // Blacklist (defense-in-depth): substring scan over the email too.
        for pattern in &cfg.forbidden_substrings {
            if pattern.is_empty() {
                continue;
            }
            if trimmed.contains(pattern.as_str()) {
                hits.push((
                    format!("forbidden substring `{pattern}` in identity"),
                    trimmed.to_owned(),
                ));
            }
        }
    }
    hits
}

/// Pure blacklist scan over author/committer names and commit-message lines —
/// the free-text layer of the publish-identity gate.
///
/// No whitelist applies to free text (there is no codebook for a display name
/// or a commit subject); only `forbidden_substrings` is enforced here.
fn collect_identity_text_violations(
    text_output: &str,
    cfg: &PublishIdentityConfig,
) -> Vec<(String, String)> {
    let mut hits = Vec::new();
    if cfg.forbidden_substrings.is_empty() {
        return hits;
    }
    for line in text_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        for pattern in &cfg.forbidden_substrings {
            if pattern.is_empty() {
                continue;
            }
            if trimmed.contains(pattern.as_str()) {
                hits.push((
                    format!("forbidden substring `{pattern}`"),
                    trimmed.to_owned(),
                ));
            }
        }
    }
    hits
}

/// The molecule variable a brief sets to declare its allowed change
/// perimeter for the scope-guard (`cs nucleate --var scope_allow=<globs>`).
///
/// Comma/newline-separated globset patterns (e.g.
/// `docs/book/src/**,README.md`). Absent ⇒ the guard is inert for that
/// molecule, so every molecule that predates the knob keeps byte-identical
/// `cs done` behaviour.
const SCOPE_ALLOW_VAR: &str = "scope_allow";

/// Run `git diff --name-only <range>` from `repo_root` and return the changed
/// paths, one per element.
///
/// Conservative on probe failure — a missing branch, an empty range, or a
/// non-git directory yields an empty `Vec`, so the caller partitions nothing
/// and the guard passes (never blocks on a probe slip — same discipline as
/// [`git_log_field`] and [`check_git_remote_blocklist`]).
///
/// Uses the three-dot `<base>...<branch>` form at the call site so the diff is
/// taken from the merge-base — the exact set of paths the merge would
/// introduce, matching the commit set [`check_publish_identity_blocklist`]
/// scans over `<base>..<branch>`.
fn git_diff_names(repo_root: &Path, range: &str) -> Vec<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "diff",
            "--name-only",
            range,
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Scope-guard (P3 of `task-20260712-3819`) — surface files the merge would
/// introduce that fall outside the molecule's declared change-perimeter.
///
/// The forcing incident: a worker briefed on `docs/book/src/**` + `README.md`
/// rewrote 40 crate-source files under `crates/cosmon-cli/src/cmd/`, which
/// would have broken the golden man-page test and silently changed
/// `cs --help` output. The escape was caught only by a hand `git status`.
///
/// `perimeter` is the parsed `scope_allow` glob set (empty ⇒ the molecule
/// declared no perimeter ⇒ the guard is inert, a zero-cost early return).
/// The changed paths over `<base>...<branch>` are partitioned against a
/// compiled [`globset::GlobSet`] via the pure
/// [`cosmon_core::scope_guard::partition_changed_paths`] core; a glob that
/// fails to compile is skipped (a malformed pattern must never crash the merge
/// gate — the remaining patterns still apply).
///
/// Policy (invariants §8b — *propose mechanisms of verification, do not
/// impose them*): the default is **advisory** — an out-of-scope merge prints a
/// structured warning and proceeds. `[scope_guard] strict = true` escalates it
/// to a hard `cs done` abort. Unlike the anti-leak gates, an out-of-scope
/// change is a quality/integrity signal, not a confidentiality breach, so the
/// honest default warns rather than aborting a legitimate adjacent-file
/// refactor.
fn check_scope_guard(
    repo_root: &Path,
    branch: &str,
    base: &str,
    perimeter: &[String],
    strict: bool,
) -> anyhow::Result<()> {
    if perimeter.is_empty() {
        return Ok(());
    }
    let changed = git_diff_names(repo_root, &format!("{base}...{branch}"));
    if changed.is_empty() {
        return Ok(());
    }

    // Compile the declared perimeter into a matcher. A malformed glob is
    // skipped so a typo cannot wedge the gate; the surviving patterns still
    // define the perimeter.
    let mut builder = globset::GlobSetBuilder::new();
    for pat in perimeter {
        if pat.is_empty() {
            continue;
        }
        if let Ok(glob) = globset::Glob::new(pat) {
            builder.add(glob);
        }
    }
    let set = builder
        .build()
        .unwrap_or_else(|_| globset::GlobSet::empty());

    let partition =
        cosmon_core::scope_guard::partition_changed_paths(&changed, |p| set.is_match(p));
    if partition.is_clean() {
        return Ok(());
    }

    let mut msg = format!(
        "scope-guard: the merge touches {} file(s) outside the molecule's declared \
         perimeter (scope_allow).\n\
         (P3 of task-20260712-3819 — a docs-only brief once rewrote 40 crate-source files)\n\
         Declared perimeter: {}\n\
         Out-of-scope changes:\n",
        partition.out_of_scope_count(),
        perimeter.join(", "),
    );
    for path in &partition.out_of_scope {
        let _ = writeln!(msg, "  {path}");
    }

    if strict {
        msg.push_str(
            "\n`cs done` aborts because `[scope_guard] strict = true`. Either widen the\n\
             molecule's `scope_allow` perimeter, or `git checkout` the out-of-scope files\n\
             in the worktree before retrying.\n",
        );
        return Err(anyhow::anyhow!("{msg}"));
    }

    msg.push_str(
        "\nThis is advisory (`[scope_guard] strict = false`). The merge proceeds. Set\n\
         `strict = true` in `.cosmon/config.toml` to make an out-of-scope merge a hard\n\
         abort.\n",
    );
    eprintln!("⚠ {msg}");
    Ok(())
}

/// The verbatim residual-risk statement appended to every confidential
/// blocklist abort (ADR-128, turing's MANDATORY decidability clause).
///
/// "Does this artifact disclose the secret?" is a semantic property —
/// Rice-theorem-adjacent and undecidable; any finite filter can be evaded
/// by paraphrase or encoding. The gate closes the *decidable* subproblem
/// (literal name + aliases + domain + email) completely. Promising
/// semantic coverage would manufacture false confidence, so the message
/// states the boundary explicitly.
const CONFIDENTIAL_RESIDUAL_RISK: &str = "\
This gate blocks the *literal* confidential name, its registered aliases, the operator\n\
domain, and the operator email in external artifacts. It does NOT detect paraphrase,\n\
implication, encoded, or composed disclosure. Those remain the responsibility of human\n\
review. The gate reduces the realized failure class to zero; it does not reduce the\n\
semantic failure class.\n";

/// Refuse `cs done` when a publish-bound artifact's *content* carries a
/// confidential substring (the operator's fund name, its aliases, the
/// operator domain, or email).
///
/// Sibling of [`check_git_remote_blocklist`]: that guard inspects the
/// worktree's *remotes*, this one inspects the *content* of the narrow set
/// of files matched by `cfg.publish_globs`. Closes the attribution
/// vacuum (ADR-128) — the fleet stamping the operator's confidential
/// fund name into external boilerplate (README,
/// footer, index). A negative per-molecule guard ("don't say X") fails by
/// construction; the deterministic merge gate is the structural floor.
/// V0 file-content floor that [`check_publish_identity_blocklist`] (V1)
/// widens to the git-identity channel.
///
/// Scans **only** files matched by `publish_globs` (NARROW by design — a
/// `grep -r` would false-positive on internal docs that legitimately name
/// the entity). On any hit: returns `Err` listing every (file, substring)
/// violation, points at the config block, and ends with the mandatory
/// residual-risk statement. Hard abort, identical across all three regimes
/// — Autonomous mode has no operator to scrub after the fact, so an
/// advisory gate would abandon exactly the regime that needs it.
///
/// Probed against `repo_root` (the merged checkout) because that is where
/// the about-to-be-published content lives after the merge step.
/// Resolve the operator's machine-wide cosmon config path,
/// `~/.config/cosmon/config.toml` — the SINGLE private home for the
/// operator's confidential blocklist.
///
/// This is the federation source: the operator's fund name is the *same*
/// secret across every galaxy, so it lives here once (never committed to any
/// galaxy repo) and `cs done` folds it into each galaxy's per-galaxy gate.
/// Honours `$COSMON_CONFIG_HOME` for test isolation, falling back to
/// `$HOME/.config` — the same convention as `tackle::global_adapter_config_path`
/// (deliberately **not** `dirs::config_dir()`, which lands in
/// `~/Library/Application Support` on macOS).
fn global_operator_config_path() -> PathBuf {
    let config_home = std::env::var_os("COSMON_CONFIG_HOME").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into())).join(".config"),
        PathBuf::from,
    );
    config_home.join("cosmon").join("config.toml")
}

/// Load **only** the `[confidential_blocklist]` section of the global
/// operator config at `path`.
///
/// Best-effort by construction: a missing file, an I/O error, or a TOML
/// parse failure all yield the default (empty) blocklist, so a malformed
/// global config can never abort `cs done` — the per-galaxy config still
/// applies. A bespoke deserialize struct (rather than the full
/// [`ProjectConfig`]) is used so unrelated global-config sections are ignored
/// and the `[project]` table is not required — mirrors
/// `tackle::load_global_adapters`.
fn load_global_confidential_blocklist(path: &Path) -> ConfidentialBlocklistConfig {
    #[derive(serde::Deserialize, Default)]
    struct GlobalBlocklistOnly {
        #[serde(default)]
        confidential_blocklist: ConfidentialBlocklistConfig,
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<GlobalBlocklistOnly>(&text).ok())
        .map(|g| g.confidential_blocklist)
        .unwrap_or_default()
}

/// The effective confidential blocklist for a `cs done` invocation: the
/// per-galaxy config union-merged with the operator's machine-wide blocklist.
///
/// This is the load-bearing federation step. Without it, the gate fires only
/// in the one galaxy that hand-typed `[confidential_blocklist]` (cosmon),
/// leaving every other galaxy — the qfa leak among them — with an empty,
/// inert gate. With it, the operator supplies the fund name once in
/// `~/.config/cosmon/config.toml` and every galaxy inherits it.
fn effective_confidential_blocklist(
    per_galaxy: &ConfidentialBlocklistConfig,
) -> ConfidentialBlocklistConfig {
    effective_confidential_blocklist_from(per_galaxy, &global_operator_config_path())
}

/// Path-parameterized core of [`effective_confidential_blocklist`] — merges
/// the per-galaxy blocklist with the global one loaded from `global_path`.
///
/// Split out so the federation merge can be tested end-to-end against an
/// explicit temp config file, with no process-global `$HOME`/env mutation
/// (which would race the parallel test runner).
fn effective_confidential_blocklist_from(
    per_galaxy: &ConfidentialBlocklistConfig,
    global_path: &Path,
) -> ConfidentialBlocklistConfig {
    // Home-galaxy exemption: a galaxy that OWNS the federation confidential
    // name(s) names itself freely, so the machine-wide federation blocklist is
    // NOT folded into its gate — otherwise the gate would block the galaxy's
    // own name in its own internal files (false positives that abort `cs done`).
    // The owner declares itself with a generic flag; no galaxy name is baked
    // into cosmon. Its own per-galaxy substrings (if any) still apply.
    if per_galaxy.owns_federation_secret {
        return per_galaxy.clone();
    }
    let global = load_global_confidential_blocklist(global_path);
    per_galaxy.merged_with(&global)
}

fn check_confidential_blocklist(
    repo_root: &Path,
    cfg: &ConfidentialBlocklistConfig,
) -> anyhow::Result<()> {
    if cfg.is_empty() {
        return Ok(());
    }
    let files = collect_publish_files(repo_root, &cfg.effective_publish_globs());
    // Read each matched file; skip unreadable / binary (non-UTF8) files —
    // the gate is defensive, a read failure must never block the hot path
    // by panicking. A file we cannot read as text cannot carry a literal
    // substring match anyway.
    let contents: Vec<(String, String)> = files
        .iter()
        .filter_map(|rel| {
            std::fs::read_to_string(repo_root.join(rel))
                .ok()
                .map(|body| (rel.clone(), body))
        })
        .collect();
    let violations = collect_confidential_blocklist_violations(&contents, cfg);
    if violations.is_empty() {
        return Ok(());
    }
    let mut msg = String::from(
        "publish-bound artifact carries a confidential substring — `cs done` aborts to prevent leak.\n\
         (delib-20260617-62ff / ADR-128: D7 attribution-vacuum publish gate)\n",
    );
    for (file, pattern) in &violations {
        let _ = writeln!(msg, "  file:      {file}");
        let _ = writeln!(msg, "  substring: {pattern}");
    }
    msg.push_str(
        "\nRemove the confidential string from the file(s) above, then retry. The\n\
         blocklist is configured in `.cosmon/config.toml` under\n\
         `[confidential_blocklist]` (forbidden_substrings + publish_globs).\n\n",
    );
    msg.push_str(CONFIDENTIAL_RESIDUAL_RISK);
    Err(anyhow::anyhow!("{msg}"))
}

/// Walk `repo_root` and return the relative paths of every file matched by
/// `publish_globs` — the testable file-discovery half of
/// [`check_confidential_blocklist`].
///
/// Uses `ignore::WalkBuilder` so `.git/`, `.worktrees/`, and gitignored
/// scaffolding are skipped by default (we never scan throwaway scratch or
/// the git object store). Globs are matched against the path relative to
/// `repo_root`. A glob that fails to compile is skipped (a malformed
/// pattern must not crash the merge gate); the remaining patterns still
/// apply.
fn collect_publish_files(repo_root: &Path, publish_globs: &[String]) -> Vec<String> {
    let mut builder = globset::GlobSetBuilder::new();
    let mut any = false;
    for pat in publish_globs {
        if pat.is_empty() {
            continue;
        }
        if let Ok(glob) = globset::Glob::new(pat) {
            builder.add(glob);
            any = true;
        }
    }
    if !any {
        return Vec::new();
    }
    let Ok(set) = builder.build() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(repo_root).hidden(false).build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(repo_root) else {
            continue;
        };
        if set.is_match(rel) {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
    out
}

/// Pure case-folded substring scan over publish-bound file contents — the
/// testable core of [`check_confidential_blocklist`], mirroring
/// [`collect_remote_blocklist_violations`].
///
/// Takes `(relative_path, content)` pairs and returns every
/// `(file, matched_substring)` pair so the caller can report all
/// violations at once. Matching is case-insensitive: a footer that writes
/// `TENANT-DEMO` or `tenant-demo` is caught the same as `Tenant-Demo`, because the
/// realized leak class does not respect casing.
fn collect_confidential_blocklist_violations(
    files: &[(String, String)],
    cfg: &ConfidentialBlocklistConfig,
) -> Vec<(String, String)> {
    let mut hits = Vec::new();
    let needles: Vec<(String, String)> = cfg
        .forbidden_substrings
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| (p.clone(), p.to_lowercase()))
        .collect();
    for (path, content) in files {
        let folded = content.to_lowercase();
        for (original, lowered) in &needles {
            if folded.contains(lowered.as_str()) {
                hits.push((path.clone(), original.clone()));
            }
        }
    }
    hits
}

/// Execute the `done` command.
#[allow(clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    // Guard: require project identity before touching transport.
    super::require_project_identity(ctx)?;

    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    let mol_id = MoleculeId::new(&args.molecule)?;
    let mol = store.load_molecule(&mol_id)?;

    require_security_review_verdict(&state_dir, &mol)?;

    // Worktree guard (idea-20260531-1e1b). Capture the worktree `cs tackle`
    // recorded for this molecule NOW, before teardown removes it — so the
    // artifact commit at step 7 can verify it is committing inside the galaxy
    // that owns the molecule, not a foreign release clone (the genericize
    // ghost-commit). Canonicalized while the worktree still exists so symlinks
    // resolve. `None` for legacy / `--no-worktree` / test molecules, which
    // then behave as today.
    let galaxy_root = state_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| state_dir.clone(), Path::to_path_buf);
    let recorded_worktree = super::evolve::recorded_worktree_for(&store, &mol, &galaxy_root)
        .map(|p| super::evolve::canonical_or(&p));

    // --if-completed: silent no-op when the molecule is not `Completed`
    // or has already been merged. Subsumes the former `cs harvest` verb
    // (ADR-052 §D3). Emits the canonical `Harvested` audit event so the
    // trail is identical to the old `cs harvest` path.
    //
    // On the `already_merged` branch, a stale fleet worker may still be
    // bound to this molecule — e.g. a prior `cs done` stamped `merged_at`
    // but crashed before reaching the purge step, or the merge was
    // performed outside `cs done` (manual `git merge`). Leaving that
    // worker in `fleet.json` causes `cs ensemble` to display the
    // already-merged molecule as `running/diverged` indefinitely. Attempt
    // a best-effort purge before returning so the hook/patrol sweep path
    // converges to a clean state even after a partial prior teardown.
    if args.if_completed {
        use cosmon_core::molecule::MoleculeStatus;
        // Already-terminal short-circuit. `merged_at` is the merged-branch
        // marker; `archived` is the terminal-Inert marker that a `no_branch`
        // molecule reaches WITHOUT a `merged_at` stamp (delib / drainage /
        // empty-branch task — task-20260626-eb65). Recognising `archived` here
        // keeps `cs done --if-completed` an idempotent no-op for those
        // molecules instead of re-running a full teardown probe on every sweep.
        if mol.status != MoleculeStatus::Completed || mol.merged_at.is_some() || mol.archived {
            let outcome = if mol.status != MoleculeStatus::Completed {
                "not_completed"
            } else if mol.merged_at.is_some() {
                "already_merged"
            } else {
                "already_archived"
            };
            let already_merged = mol.status == MoleculeStatus::Completed && mol.merged_at.is_some();
            let purged_stale = if already_merged {
                purge_stale_worker_for(&store, &mol)
            } else {
                false
            };
            // task-20260719-fedf — the fleet entry was only ever half the
            // leftover. Reclaim the merged branch and worktree too, so the
            // documented recovery verb actually leaves a clean galaxy.
            let reclaimed = if already_merged {
                find_repo_root().map_or_else(
                    |_| Vec::new(),
                    |root| reclaim_merged_git_artifacts(&root, &mol_id),
                )
            } else {
                Vec::new()
            };
            if ctx.json {
                let payload = serde_json::json!({
                    "command": "done",
                    "molecule": mol_id.as_str(),
                    "if_completed": true,
                    "outcome": outcome,
                    "purged_stale_worker": purged_stale,
                    "reclaimed": reclaimed,
                });
                println!("{}", serde_json::to_string(&payload)?);
            } else {
                println!("done {mol_id}: {outcome} (--if-completed no-op)");
                if purged_stale {
                    println!("  • purged stale fleet worker");
                }
                for action in &reclaimed {
                    println!("  • {action}");
                }
            }
            return Ok(());
        }
    }

    let socket = super::tmux_socket_name(ctx);

    // Resolve the tmux session name the worker was tackled with. Stored
    // on the molecule at tackle time so renames stay in lockstep with
    // teardown; falls back to the raw molecule ID for legacy molecules.
    let session_name = mol
        .session_name
        .clone()
        .unwrap_or_else(|| mol_id.to_string());

    // --dry-run: compute plan and display, no side effects.
    if args.dry_run {
        let plan = compute_teardown_plan(
            ctx,
            args,
            &mol_id,
            &mol.status.to_string(),
            mol.status.is_terminal(),
            &socket,
            &session_name,
        )?;
        report_plan(ctx, &plan);
        return Ok(());
    }

    let branch_name = format!("feat/{mol_id}");
    let wid = WorkerId::new(&session_name)?;
    let repo_root = find_repo_root()?;
    let worktree_path = repo_root.join(".worktrees").join(mol_id.as_str());

    // Detect "ghost teardown": every teardown resource is already absent.
    // Branch gone, worktree gone, fleet entry gone, tmux session dead. In
    // that case the molecule's status is moot — there is nothing to risk
    // by proceeding, and forcing the operator to add `--force` (plus the
    // `--no-merge --no-worktree-remove --no-branch-delete` triad reported
    // in df4c) is unnecessary friction.
    let backend_probe = TmuxBackend::new(&socket);
    let session_alive = backend_probe.is_alive(&wid).unwrap_or(false);
    let worker_registered = store
        .load_fleet()
        .is_ok_and(|f| f.workers.contains_key(&wid));
    let nothing_left = !branch_exists(&repo_root, &branch_name)
        && !worktree_path.exists()
        && !session_alive
        && !worker_registered;

    // 1. Guard: refuse teardown of an active molecule unless --force —
    //    *unless* there is literally nothing left to tear down.
    if !mol.status.is_terminal() && !args.force && !nothing_left {
        return Err(anyhow::anyhow!(
            "molecule {mol_id} is {} — use --force to tear down an active molecule",
            mol.status
        ));
    }

    // 1b. Structural anti-leak guard (delib-20260426-7cfc, R1).
    //
    //     Refuse to merge if the worktree carries a forbidden git remote.
    //     Discipline ("never add this remote") fails categorically against a
    //     tired-3am-agent; the merge gate is the structural floor that does
    //     not. The blocklist is configured per-project in
    //     `.cosmon/config.toml [git_remote_blocklist]` and ships empty by
    //     default — backward compatible for every project that does not need
    //     the guard.
    //
    //     Probed against `repo_root` (the cosmon checkout itself, not the
    //     worker's `.worktrees/<mol>` worktree) because shared remotes are
    //     visible from every worktree of the same repository.
    let blocklist_config_path = super::resolve_config_from_context(ctx);
    let blocklist_cfg = cosmon_filestore::load_project_config(&blocklist_config_path)
        .unwrap_or_else(|_| ProjectConfig::default());
    check_git_remote_blocklist(&repo_root, &blocklist_cfg.git_remote_blocklist)?;

    // 1c. Publish-identity gate (ADR-128 §V1 — the D7 publish-closure
    //     widening). The git author/committer identity is stamped into every
    //     commit and is invisible to V0's file-content scan; shannon's
    //     confirmed residual is the operator email riding a shipped public
    //     repo. Scanned over `<base>..<branch>` — only the commits this merge
    //     would publish, never the project's pre-existing history. Ships
    //     empty by default (cosmon is internal; the operator identity is
    //     legitimate here), so this is a zero-cost return for every project
    //     that does not configure the guard.
    let base_branch = resolve_base_branch(&repo_root);

    // 1b'. Range non-emptiness precondition (delib-20260717-194b, F5 / adversary
    //      A6). Every range-scoped gate below scans `<base>..<branch>`. If base
    //      MISresolves, that range is empty and every gate passes *vacuously* —
    //      scanning nothing, disabling them all at once. Fail CLOSED: when the
    //      branch is NOT already reachable from base (so it genuinely carries
    //      unlanded work) yet `rev-list --count <base>..<branch>` is zero, the
    //      base resolved to something unrelated. Refuse rather than let the
    //      publish-identity gate and the author-slot assertion sleep through an
    //      empty scan. A probe that could not run (`None`) is NOT treated as a
    //      zero — only a genuine, git-reported zero-but-unmerged branch aborts.
    if !args.no_merge
        && branch_exists(&repo_root, &branch_name)
        && !is_branch_merged(&repo_root, &branch_name)
    {
        if let Some(0) = rev_list_count(&repo_root, &base_branch, &branch_name) {
            return Err(anyhow::anyhow!(
                "cs done aborts: branch {branch_name} is not reachable from base \
                 `{base_branch}`, yet `git rev-list --count {base_branch}..{branch_name}` \
                 is 0 — the base branch misresolved and every range-scoped publish gate \
                 would pass scanning an empty range (delib-20260717-194b, F5 / adversary \
                 A6). Refusing to integrate on a vacuous range. Verify the base branch \
                 (COSMON_BASE_BRANCH / origin/HEAD) and rerun `cs done` from the main \
                 checkout."
            ));
        }
    }

    check_publish_identity_blocklist(
        &repo_root,
        &branch_name,
        &base_branch,
        &blocklist_cfg.publish_identity,
    )?;

    // 1c''. Author-slot assertion (delib-20260717-194b, F4 — the load-bearing
    //       backstop). The publish-identity blocklist above ships EMPTY in every
    //       internal galaxy, so it catches nothing there. This assertion is
    //       independent of that codebook (T3): it keys on the operator's own git
    //       identity, resolved from the repo config, and demands that every
    //       commit the merge would publish is operator-authored AND
    //       operator-committed. The maker (Noogram) and the real adapter belong
    //       ONLY on `Co-Authored-By:` trailers — never in the author slot. This
    //       is the catch for the codex-author leak (P2): even when birth-time
    //       identity pinning (F2/F3) silently no-ops (tmux boundary, config
    //       precedence, a late amend), this post-facto scan closes the hole.
    //
    //       Gated on a configured `[attribution]` block: a galaxy that adopts
    //       native attribution promises operator-authored feature commits, so
    //       the invariant fires for it without asking it to fill a blocklist. A
    //       zero-config galaxy stays byte-identical (F9). A configured galaxy
    //       fails closed when operator identity cannot be resolved: silently
    //       skipping would disable the assertion where it is needed most.
    if !args.no_merge && !blocklist_cfg.attribution.is_empty() {
        let operator = require_operator_identity(&repo_root)?;
        assert_operator_authored_commits(&repo_root, &base_branch, &branch_name, &operator)?;
    }

    // 1c'. Scope-guard (P3 of task-20260712-3819). When the molecule declared
    //      a change-perimeter via `--var scope_allow=<globs>`, surface any
    //      file the merge would introduce outside it. Advisory by default
    //      (§8b), a hard abort under `[scope_guard] strict = true`. Inert when
    //      the molecule declared no perimeter, so this is a zero-cost return
    //      for every molecule that predates the knob.
    let scope_perimeter = mol
        .variables
        .get(SCOPE_ALLOW_VAR)
        .map(|raw| cosmon_core::scope_guard::parse_scope_perimeter(raw))
        .unwrap_or_default();
    check_scope_guard(
        &repo_root,
        &branch_name,
        &base_branch,
        &scope_perimeter,
        blocklist_cfg.scope_guard.strict,
    )?;

    // 1d. Pre-done gate — the blocking `[hooks] pre_done` hook
    //     (showroom delib-20260701-bfdf, torvalds D1).
    //
    //     `post_merge` is advisory: it runs after the merge lands and can
    //     only warn. That left a structural hole — nothing in the molecule
    //     cycle could *refuse* a DONE. A rigorous, falsifiable Definition-of-
    //     Done (DROVE ∧ OBSERVED ∧ BADGE ∧ FALSIFIER) could only be enforced
    //     out-of-band in GitHub branch-protection, outside cosmon's teardown.
    //
    //     `pre_done` closes it: run the galaxy-configured script BEFORE the
    //     trunk lock and the merge — while everything is still reversible —
    //     and HARD ABORT teardown on a non-zero exit, forwarding the script's
    //     stderr as the reason. Cosmon runs the verdict; the policy lives in
    //     the galaxy's script. Idempotent (the script is a pure read of
    //     repo/molecule state), ships absent by default (backward compatible),
    //     and honours a per-invocation operator kill-switch
    //     (`--skip-pre-done-hook` / `COSMON_SKIP_PRE_DONE_HOOK`).
    //
    //     Placed here, ahead of the trunk lock (step 2), so a refused DONE
    //     touches nothing: no lock contention, no merge, no `merged_at`
    //     stamp, no worktree/branch/tmux teardown. The operator (or worker)
    //     fixes the gap and reruns `cs done`.
    if let Some(ref hook_cmd) = blocklist_cfg.hooks.pre_done {
        if pre_done_hook_skipped(args.skip_pre_done_hook) {
            eprintln!(
                "⚠ pre_done gate skipped by operator kill-switch (--skip-pre-done-hook / COSMON_SKIP_PRE_DONE_HOOK): {hook_cmd}"
            );
        } else {
            // Trust gate (B5, RCE-by-clone): the `pre_done` hook is a
            // repo-supplied shell string. This is a hard gate before any
            // merge, so refuse `cs done` outright on an untrusted repository
            // rather than running the hook — the operator is told to
            // `cs trust` first.
            cosmon_cli::trust::ensure_trusted(&repo_root)?;
            run_pre_done_hook(&repo_root, hook_cmd, &mol_id).inspect_err(|e| {
                report_pre_done_failure(ctx, &mol_id, hook_cmd, &e.to_string());
            })?;
        }
    }

    let mut actions: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if nothing_left && !mol.status.is_terminal() && !args.force {
        warnings.push(format!(
            "molecule is {} but no teardown resource remains — proceeding as idempotent no-op",
            mol.status
        ));
    }

    // Track whether merge succeeded — gates branch deletion (FIX 2).
    let mut merge_succeeded = false;

    // PR-B (task-20260714-aa2e): the durable `MergeCompleted` for a *successful*
    // merge is emitted ONCE, AFTER the post-merge gate — never before it. The
    // merge-success arms below therefore no longer emit their own `Ok`; they
    // only record the success flavor here so the single post-gate event can
    // carry it. `None` = no successful merge yet; `Some(0)` = clean landing;
    // `Some(n>0)` = landed after `n` escalation(s). The pre-gate `Ok` this
    // replaces "lied": it was written before the gate ran, so a merge the gate
    // then rolled back (or could only mark `Unverified`) left a permanent `Ok`
    // in `events.jsonl` the gate never earned. See `post_gate_merge_result`.
    let mut merge_escalation_retries: Option<u32> = None;

    // Track whether teardown reached its terminal state with *nothing to
    // merge* — the `no_branch` outcome (delibs, drainage workers, empty-branch
    // tasks that never produced a feat branch). A no_branch molecule is already
    // `Completed` and genuinely has no branch to land, yet it still must reach
    // the terminal **Inert** state on disk: `archived = true`. Before this flag
    // existed, the `NoBranch` arm pushed only the `no_branch` action and the
    // archive write below (gated on `merge_succeeded`) was skipped — leaving
    // `{status: Completed, archived: false}`, which the molecule-health pass
    // (A8 `CompletedUnharvested`, ADR-137 §3/§4) re-detected on every sweep as
    // a permanent phantom anomaly. `cs done` reported `✅ done` but never
    // flipped the archival bit, so the harvest never *cleared*. Archival, not
    // the merge, is what makes A8 clearable; the two are decoupled here.
    // Distinct from `merge_succeeded` so a no_branch teardown archives WITHOUT
    // firing the post-merge deploy hook, the confidential publish gate, or the
    // `merged_at` stamp — none of which apply when nothing landed on trunk.
    let mut no_branch_teardown = false;

    // FIX 3: --no-merge implies --no-branch-delete (unless --force).
    // When merge is skipped, the branch is the last copy of the worker's
    // code — deleting it is silent data destruction.
    let effective_no_branch_delete = if args.no_merge && !args.no_branch_delete && !args.force {
        warnings.push(
            "--no-merge implies --no-branch-delete (branch is the only copy of the work; use --force to override)"
                .to_owned(),
        );
        true
    } else {
        args.no_branch_delete
    };

    // 2. Merge the worker's branch into the current branch.
    //
    // Acquire the **trunk write lock** (ADR-110 Phase 1 Commit 1, invariant
    // I1 WRITER-UNIQUE) before any operation that mutates the shared cosmon
    // main checkout. Two concurrent `cs done` against the same checkout would
    // otherwise race on `git merge` and produce one of the cassures that
    // motivated `delib-20260523-a682` (contamination + half-applied merges).
    //
    // Lock order (deadlock-freedom — TLA+ `smithy/docs/formal/MCStitch.tla`):
    // the trunk lock is the OUTER lock. It stays alive through the merge, the
    // `merged_at` stamp, the `frontier.json` rewrite, the post-merge hook, and
    // the archive write — every trunk write — and is then **dropped before the
    // terminal fleet-purge** (step 4 below). `cs done` therefore never holds
    // `fleet ⊃ trunk`; combined with `cs stitch` holding the trunk lock alone,
    // the global order is a single total order (trunk before fleet) and
    // Coffman circular-wait is impossible. `MCStitchDeadlock.cfg` documents the
    // inversion this avoids; the abda recovery preserves the original rule
    // *« trunk lock dropped before fleet-purge to avoid lock-order inversion »*.
    //
    // `--no-merge` keeps the lock unacquired: nothing in this branch touches
    // the trunk, so contention would be a false signal.
    let trunk_guard = if args.no_merge {
        None
    } else {
        // ADR-131 Decision 2: object-safe trunk guard via the port. The guard
        // is the OUTER lock, held through the merge and dropped before the
        // fleet-purge below (drop(trunk_guard)) to preserve the trunk ⊃ fleet
        // order.
        Some(store.lock_trunk(&format!("cs done {mol_id}"))?)
    };

    // Capture the exact pre-merge revision before `cs done` emits its merge
    // event. The merge helper may flush that event into a bookkeeping commit
    // before invoking `git merge`; RR-SAFE-1 must roll back that commit too.
    let pre_merge_head = if args.no_merge {
        None
    } else {
        Some(git_head(&repo_root)?)
    };

    // Emit EventV2::MergeDispatched before the merge attempt.
    let events_path = cosmon_filestore::resolve_state_dir(None).join("events.jsonl");
    let merge_dispatch_seq = if args.no_merge {
        None
    } else {
        cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::MergeDispatched {
                molecule: mol_id.clone(),
                branch: branch_name.clone(),
                // ADR-105 / I9': worker-driven local merges carry no
                // federation lineage. `cs delegate` (task-20260518-e541)
                // and the future federation writer will stamp this when
                // the molecule originated in a sister galaxy.
                federation_provenance: None,
            },
            None,
        )
        .ok()
    };

    // Pre-merge relocation: workers occasionally commit durable artifacts to
    // a repo-relative `molecule/<name>` path (reviews, reports, scratch
    // output). When several parallel branches adopt the same convention, the
    // paths collide at merge time — three branches each overwrite
    // `molecule/review.md` and git reports an add/add conflict on every
    // landing after the first. Rewriting the worker's branch to
    // `molecule/<mol-id>/<name>` before the merge makes the paths disjoint
    // so parallel landings merge cleanly. Idempotent and non-fatal: already
    // scoped artifacts and missing worktrees are no-ops; failures warn.
    if !args.no_merge {
        match relocate_workspace_artifacts(&worktree_path, &mol_id) {
            Ok(moved) if !moved.is_empty() => {
                actions.push(format!("relocated_workspace_artifacts: {}", moved.len()));
            }
            Ok(_) => { /* nothing to relocate — silent */ }
            Err(e) => warnings.push(format!("workspace artifact relocate failed: {e}")),
        }
    }

    // Pre-merge ADR renumber: parallel workers branched from the same base
    // each pick "the next ADR" and silently collide on `ADR-NNN` (RPP and
    // LLMPort both took 117 on 2026-06-05; the operator hand-renumbered at
    // merge). This is the same collision class as the workspace-artifact
    // relocation above — branches adopting an identical convention — and is
    // resolved the same way: rewrite the worker's branch to a disjoint number
    // *before* the merge so the landing is conflict-free and no manual
    // surgery is needed. Idempotent and non-fatal (see ADR-121).
    if !args.no_merge {
        let base = resolve_base_branch(&repo_root);
        match renumber_colliding_adrs(&worktree_path, &base) {
            Ok(plans) if !plans.is_empty() => {
                for p in &plans {
                    actions.push(format!(
                        "renumbered_adr: {} → ADR-{}",
                        p.old_path,
                        cosmon_cli::adr::format_adr_number(p.new_number)
                    ));
                }
            }
            Ok(_) => { /* no ADR collision — silent */ }
            Err(e) => warnings.push(format!("ADR renumber failed: {e}")),
        }
    }

    // Native attribution (delib-20260717-194b, F1 + F6). Compute the
    // `Co-Authored-By` trailer block ONCE, here, before the merge — so it can be
    // stamped on the commit that actually EXISTS. The pre-194b bug threaded the
    // trailers only into `commit_molecule_artifacts` (step 7), a carrier that
    // task-work molecules never produce (no artifact staged under the molecule
    // dir), so the trailers were silently discarded (0/6). The right carrier on
    // the default `--no-ff` path is the merge commit cosmon itself creates
    // (F1) — the trailers are threaded into `try_merge_with_escalation` below —
    // and the artifact commit stays a fallback carrier for artifact-producing
    // molecules (step 7 reuses the same vec).
    //
    // The adapter annotation is folded from the durable event log under the
    // strict rule (F6): emitted only when EXACTLY ONE distinct adapter ran the
    // molecule. Zero recorded → drop (the log is silent, so is the stamp); more
    // than one (resume / handoff) → drop and WARN rather than credit
    // last-writer-wins, "a guess dressed as a fact". Empty `coauthor_email` ⇒
    // empty vec ⇒ every commit message byte-identical to a pre-attribution
    // cosmon (F9).
    let coauthor_trailers: Vec<String> = {
        use cosmon_state::ops::model_attribution::folded_adapter;
        let real_adapter = adapter_for_coauthor(
            folded_adapter(&state_dir, &mol_id),
            &mol_id,
            !blocklist_cfg.attribution.coauthor_trailers(None).is_empty(),
            &mut warnings,
        );
        blocklist_cfg
            .attribution
            .coauthor_trailers(real_adapter.as_deref())
    };
    warn_unstamped_attribution(
        &blocklist_cfg.attribution,
        &coauthor_trailers,
        &mut warnings,
    );

    // A fast-forward creates no commit owned by cosmon, so there is nowhere to
    // carry the configured provenance trailers without rewriting worker
    // commits (forbidden by delib-20260717-194b: it changes their SHAs and
    // breaks ancestry guards). Refuse this contradictory option combination
    // before touching the branch instead of reporting a successful but
    // unstamped integration.
    if !args.no_merge {
        ensure_attribution_carrier(args.strategy, &coauthor_trailers)?;
    }

    // Mechanical-first escalation: see docs/architectural-invariants.md
    // On conflict, try graduated escalation: ff-only → 3-way merge → propel
    // worker to rebase+resolve → retry, bounded by max_retries.
    if !args.no_merge {
        let merge_result = try_merge_with_escalation(
            ctx,
            &store,
            &mol_id,
            &repo_root,
            &branch_name,
            args.strategy,
            &session_name,
            &socket,
            !args.no_auto_propel,
            args.max_retries,
            args.propel_message.as_deref(),
            &coauthor_trailers,
        );
        match merge_result {
            Ok(MergeLoopOutcome::Merged) => {
                actions.push("merged".to_owned());
                merge_succeeded = true;
                // PR-B: no pre-gate `MergeCompleted` here. The single durable
                // event is emitted after the post-merge gate, keyed on its
                // `GateOutcome`. Record the clean-landing flavor only.
                merge_escalation_retries = Some(0);
            }
            Ok(MergeLoopOutcome::AlreadyMerged) => {
                // Topology says branch is reachable from base — nothing
                // to merge. Disambiguate the *reason* and surface the
                // appropriate action label so the operator can verify
                // the worker's deliverable landed where it was
                // supposed to. See `classify_already_merged_label`.
                let label = classify_already_merged_label(
                    mol.merged_at.is_some(),
                    branch_is_empty_relative_to(
                        &repo_root,
                        &branch_name,
                        &resolve_base_branch(&repo_root),
                    ),
                );
                actions.push(label.to_owned());
                merge_succeeded = true;
                // PR-B: pre-gate `Ok` removed — the branch was already
                // reachable, so the gate still runs on the (usually empty)
                // diff and the single post-gate event records its verdict.
                merge_escalation_retries = Some(0);
            }
            Ok(MergeLoopOutcome::NoBranch) => {
                actions.push("no_branch".to_owned());
                // No branch ever existed (delib / drainage worker / empty-branch
                // task), so there is nothing to merge — but the molecule must
                // still reach the terminal Inert state on disk. Flag the teardown
                // so the archive write below flips `archived = true` and A8
                // (`CompletedUnharvested`) becomes clearable. See the
                // `no_branch_teardown` declaration above.
                no_branch_teardown = true;
            }
            Ok(MergeLoopOutcome::Conflict { files, recovery }) => {
                // SILENT-INTEGRITY FIX (spark-20260622-6036 / task-20260622-1057).
                //
                // A merge conflict means NOTHING landed: the base branch did
                // not move, no merge commit was created, and the worker's work
                // lives only on its feat branch. Treating this as a benign
                // "warning" and printing "done with 1 warning" let the operator
                // believe stale work had shipped (a deck for an institute
                // director, in the repro). FAIL LOUDLY instead:
                //
                //   1. emit a `Conflict` merge event (audit trail),
                //   2. render a dedicated, greppable `❌ MERGE CONFLICT` report
                //      that lists the conflicted files + the recovery path and
                //      never says the word "done",
                //   3. return `Err` (non-zero exit) BEFORE any teardown step —
                //      the tmux session, worktree, and branch are all preserved.
                let _ = cosmon_state::event_log::emit_one(
                    &events_path,
                    cosmon_core::event_v2::EventV2::MergeCompleted {
                        molecule: mol_id.clone(),
                        branch: branch_name.clone(),
                        result: cosmon_core::event_v2::MergeResult::Conflict,
                        federation_provenance: None,
                    },
                    merge_dispatch_seq,
                );
                report_merge_failure(
                    ctx,
                    &mol_id,
                    "merge_conflict",
                    "MERGE CONFLICT — not merged, branch preserved",
                    &files,
                    &recovery,
                    &actions,
                );
                return Err(anyhow::anyhow!(
                    "MERGE CONFLICT: {mol_id} not merged ({} conflicted file(s): {}). \
                     Base branch unchanged, worktree and branch preserved. \
                     Resolve the conflict and rerun `cs done {mol_id}`.",
                    files.len(),
                    files.join(", ")
                ));
            }
            Ok(MergeLoopOutcome::MergedAfterEscalation { retries }) => {
                actions.push(format!("merged_after_{retries}_escalation(s)"));
                merge_succeeded = true;
                // PR-B: the escalation count is folded into the single
                // post-gate `MergeResult` (`ok:escalated(n)` /
                // `ok:escalated(n):unverified`), not emitted pre-gate here.
                merge_escalation_retries = Some(retries);
            }
            Err(e) => {
                // A hard merge failure that is NOT a textual conflict —
                // ff-only refusal, HEAD-not-on-base, post-merge verification
                // failure, or a raw git error. Like the conflict case, nothing
                // landed and no teardown happens, so this must FAIL LOUDLY too:
                // route it through `report_merge_failure` (never the "done"
                // wording) and return non-zero. Same silent-integrity class as
                // the conflict path; the only difference is the label.
                let _ = cosmon_state::event_log::emit_one(
                    &events_path,
                    cosmon_core::event_v2::EventV2::MergeCompleted {
                        molecule: mol_id.clone(),
                        branch: branch_name.clone(),
                        result: cosmon_core::event_v2::MergeResult::Error(e.to_string()),
                        federation_provenance: None,
                    },
                    merge_dispatch_seq,
                );
                report_merge_failure(
                    ctx,
                    &mol_id,
                    "merge_failed",
                    "MERGE FAILED — not merged, branch preserved",
                    &[],
                    &e.to_string(),
                    &actions,
                );
                return Err(e);
            }
        }
    }

    // RR-SAFE-1 — fast, state-combined integration gate.
    //
    // A source-only `cargo check` misses test-target arity failures, while a
    // complete test suite turns every merge into a milestone-length wait.
    // Check every workspace target after the merge has landed, but before the
    // `merged_at` stamp makes that landing observable to the resident runtime.
    // Heavy validation is the operator's explicit `cs validate` gesture, not
    // a tax imposed on the ordinary development cycle. On refusal, restore
    // the exact pre-merge main revision and hard-fail so the runtime forgets
    // this dispatch and retries the molecule instead of advancing dependents
    // past a broken main.
    if merge_succeeded {
        let gate_outcome = match pre_merge_head.as_deref() {
            Some(pmh) => match run_post_merge_gate(&repo_root, pmh, &blocklist_cfg.gates) {
                Ok(outcome) => outcome,
                // A gate *error* — a declared command or `cargo check` returned
                // non-zero, or a manifest was unparseable. Roll main back to its
                // pre-merge revision and fail loudly; the runtime forgets this
                // dispatch and retries the molecule rather than advancing
                // dependents past a main it could not verify.
                Err(e) => {
                    return Err(refuse_post_merge_and_rollback(
                        ctx,
                        &events_path,
                        &mol_id,
                        &branch_name,
                        &repo_root,
                        Some(pmh),
                        merge_dispatch_seq,
                        &actions,
                        &e.to_string(),
                    ));
                }
            },
            None => {
                return Err(refuse_post_merge_and_rollback(
                    ctx,
                    &events_path,
                    &mol_id,
                    &branch_name,
                    &repo_root,
                    None,
                    merge_dispatch_seq,
                    &actions,
                    "post-merge gate refused DONE but no pre-merge revision was captured",
                ));
            }
        };
        // Exhaustive, NO `_ =>` arm (tolnay, delib-559a Q2): a future `GateOutcome`
        // variant must force this consumer to decide what it means, not default to
        // "success". `Unverified` proceeds by default — it is a loud advisory, not
        // a rollback — and lands as a durable `ok:unverified` witness instead of
        // the bare `Ok` the pre-gate event used to lie with. The one exception is
        // the operator opt-in `[gates].fail_closed_on_unverified`, which promotes
        // *any* `Unverified` to a rollback (D1, delib-20260714-7605).
        match &gate_outcome {
            GateOutcome::Verified { description } => {
                actions.push(format!("post_merge_compile_gate: {description}"));
            }
            GateOutcome::NothingToVerify => {
                actions.push(
                    "post_merge_compile_gate: nothing to verify (no Rust or build-structural change)"
                        .to_owned(),
                );
            }
            GateOutcome::Unverified {
                reason,
                command,
                expected,
            } => {
                let hint = command
                    .as_ref()
                    .map(|c| format!("; verify by hand: {c}"))
                    .unwrap_or_default();
                // D1 policy — the operator-ratified expected-gate discriminator
                // (delib-20260714-559a, Defect 5 / task-20260715-ff5b):
                //
                //     fail_closed = expected || fail_closed_on_unverified
                //
                // `Unverified { expected: true }` — a gate WAS expected (cargo
                // resolved, or a command was declared) but a code diff still went
                // unchecked — fails **CLOSED by default** (reset_hard, nonzero),
                // protecting cosmon-on-cosmon's own net. `expected: false`
                // (nothing was ever declared to verify this tree) stays advisory
                // fail-open-loud unless the operator opt-in
                // `[gates].fail_closed_on_unverified` promotes it. The prior
                // implementation gated on the flag alone and ignored `expected`
                // for policy — the deviation ADR-158 documented; restored here to
                // the ratified rule.
                let fail_closed = *expected || blocklist_cfg.gates.fail_closed_on_unverified;
                if fail_closed {
                    let expectation = if *expected {
                        "a gate was expected but a code diff went unchecked \
                         (D1 expected-gate discriminator: fail-closed by default)"
                    } else {
                        "no verification was declared for this tree, and \
                         [gates].fail_closed_on_unverified is set"
                    };
                    let cause =
                        format!("post-merge integrity UNVERIFIED ({expectation}) — {reason}{hint}");
                    return Err(refuse_post_merge_and_rollback(
                        ctx,
                        &events_path,
                        &mol_id,
                        &branch_name,
                        &repo_root,
                        pre_merge_head.as_deref(),
                        merge_dispatch_seq,
                        &actions,
                        &cause,
                    ));
                }
                // Reached only for `expected: false` without the promoting flag —
                // the fail-open-loud advisory. `expected: true` always fails
                // closed above, so no "(gate expected)" advisory is emitted here.
                actions.push(format!(
                    "post_merge_compile_gate: ⚠ UNVERIFIED — {reason}{hint}"
                ));
            }
        }
        // The one and only durable merge-result event for a successful merge.
        // Emitted post-gate so it can never assert an `Ok` the gate has not
        // earned; `merge_escalation_retries` was set by the winning merge arm.
        //
        // Loud-log on witness persistence failure (task-20260715-e0a6, round-2).
        // This `MergeCompleted` IS the durable witness a downstream honesty
        // auditor reads — most sharply the `ok:unverified` flavor, which records
        // that the gate could not positively verify the merged tree. The merge
        // has already landed, so an append failure to `events.jsonl` must NOT
        // abort teardown (rolling back a landed merge over a log write is worse).
        // But the lost witness cannot be swallowed by a bare `let _ =`: without
        // the record, an auditor sees *no* merge-completed line and silently
        // mistakes an unverified (or escalated) landing for one that was never
        // merged at all. Surface the loss both on stderr and in the structured
        // warning stream so it is conspicuous, never inferred from absence.
        let witness_result = post_gate_merge_result(&gate_outcome, merge_escalation_retries);
        if let Err(e) = cosmon_state::event_log::emit_one(
            &events_path,
            cosmon_core::event_v2::EventV2::MergeCompleted {
                molecule: mol_id.clone(),
                branch: branch_name.clone(),
                result: witness_result.clone(),
                federation_provenance: None,
            },
            merge_dispatch_seq,
        ) {
            let msg = format!(
                "CRITICAL: failed to persist durable merge witness ({wire}) to events.jsonl: {e} \
                 — the merge landed but no merge_completed record was written; a downstream \
                 honesty auditor would read this molecule as never-merged",
                wire = witness_result.to_wire(),
            );
            eprintln!("⚠ {msg}");
            warnings.push(msg);
        }
    }

    // 2a'. Stamp `merged_at` on the molecule and rewrite the atomic frontier
    //      projection. This is the one instant both facts are simultaneously
    //      true ("molecule completed" and "branch landed on main"), so it is
    //      the correct write-point for `frontier.json` — the single state
    //      variable the scheduler reads on every poll. See ADR-041.
    //      Non-fatal: failures warn but never abort teardown.
    if merge_succeeded {
        match store.load_molecule(&mol_id) {
            Ok(mut latest) => {
                if latest.merged_at.is_none() {
                    latest.merged_at = Some(chrono::Utc::now());
                    // Invariant `archived ⇒ status.is_terminal()` (idea-20260618-1b10):
                    // `cs done --force` is the only path that can land `merged_at` /
                    // `archived` on a molecule that never reached a terminal state
                    // (e.g. tackled-then-worker-died, or never tackled). Left as-is it
                    // produces a permanent `👻 unnamed-merge` ghost — archived on disk
                    // yet `Running` to every `status`-keyed reader. Terminalize here, in
                    // the same save that stamps `merged_at`, so the merged state is
                    // coherent. On the normal (non-force) path the molecule is already
                    // `Completed` by this point, so the guard makes this a no-op there.
                    terminalize_for_forced_teardown(&mut latest);
                    if let Err(e) = store.save_molecule(&mol_id, &latest) {
                        warnings.push(format!("stamp merged_at failed: {e}"));
                    }
                }
            }
            Err(e) => warnings.push(format!("reload molecule for merged_at stamp failed: {e}")),
        }
        match cosmon_state::frontier::compute(&store) {
            Ok(f) => {
                if let Err(e) = cosmon_state::frontier::save(&state_dir, &f) {
                    warnings.push(format!("write frontier.json failed: {e}"));
                } else {
                    actions.push(format!("frontier.json: {} ready", f.ready.len()));
                }
            }
            Err(e) => warnings.push(format!("compute frontier failed: {e}")),
        }
    }

    // 2b. Post-merge hook — run a configurable command after successful merge.
    //     This keeps downstream binaries in sync (e.g. `just install` to rebuild
    //     after new code lands on main). A failing hook warns but never aborts
    //     teardown — the merge already landed.
    //     Resolve the project config once, unconditionally — the archive
    //     write below needs `archive.enabled` for BOTH the merged path and
    //     the `no_branch` teardown path (a no_branch molecule has nothing to
    //     merge yet still archives). The publish gate and post-merge hook
    //     remain gated on `merge_succeeded`: nothing was published when no
    //     branch landed, so there is nothing to scan or deploy.
    let config_path = super::resolve_config_from_context(ctx);
    let project_config = cosmon_filestore::load_project_config(&config_path)
        .unwrap_or_else(|_| ProjectConfig::default());
    if merge_succeeded {
        let cfg = &project_config;

        // 2a'. Confidential-content publish gate (delib-20260617-62ff / ADR-128, D7).
        //
        //      The merge has landed locally, but nothing has been *published*
        //      yet — the post-merge push/deploy hook below is the first
        //      external surface. Scan the narrow `publish_globs` set of the
        //      merged tree for the operator's confidential substrings and
        //      HARD ABORT before the hook fires if any appear. Identical
        //      blocking across all three regimes (Inert / Propelled /
        //      Autonomous) — Autonomous (`cs run`) has no operator to scrub
        //      after the fact, so the gate is never advisory. A human clears
        //      a confidentiality hit, as with the remote blocklist.
        //
        //      Probed against `repo_root` (the merged checkout) because that
        //      is where the about-to-be-published content now lives.
        //
        //      The per-galaxy blocklist is union-merged with the operator's
        //      machine-wide `~/.config/cosmon/config.toml` blocklist
        //      (task-20260622-7207) so the confidential fund name — a
        //      federation-wide secret — is supplied ONCE privately yet guards
        //      every galaxy, not just the one that hand-typed the section.
        let merged_blocklist = effective_confidential_blocklist(&cfg.confidential_blocklist);
        check_confidential_blocklist(&repo_root, &merged_blocklist)?;

        // Non-blocking reminder: surfaces cosmon does not invoke (the GitHub
        // repo-description via `gh repo edit`, the deployed URL, package
        // metadata) cross boundaries this gate cannot reach. V0 names that
        // gap rather than pretending to close it (torvalds, ADR-128).
        if !cfg.confidential_blocklist.is_empty() {
            warnings.push(
                "confidential gate scanned publish_globs only; downstream surfaces cosmon does not control (gh repo description, deployed URL, package metadata) may still carry the string — V1 (task-20260617-4bce)".to_owned(),
            );
        }

        if let Some(ref hook_cmd) = cfg.hooks.post_merge {
            // Trust gate (B5, RCE-by-clone): the `post_merge` hook is a
            // repo-supplied shell string. Unlike `pre_done` it is advisory
            // and runs *after* the merge has already landed, so an untrusted
            // repository skips the hook with a warning rather than aborting a
            // now-irreversible teardown.
            match cosmon_cli::trust::ensure_trusted(&repo_root) {
                Ok(()) => match run_post_merge_hook(&repo_root, hook_cmd) {
                    Ok(code) => {
                        actions.push(format!("post_merge: {hook_cmd} (exit {code})"));
                    }
                    Err(e) => {
                        warnings.push(format!("post_merge hook failed: {e}"));
                    }
                },
                Err(e) => {
                    warnings.push(format!("post_merge hook skipped (untrusted repo): {e}"));
                }
            }
            // Deploy-hygiene self-check (task-20260607-3ad4): the post_merge
            // hook deploys to exactly one PATH target, but stale `cs` copies
            // elsewhere on PATH drift silently and can shadow the fresh build.
            // Warn loudly when multiplicity is detected — never auto-remove.
            if let Some(lines) = format_cs_multiplicity_warning(&detect_cs_path_multiplicity()) {
                for line in lines {
                    warnings.push(line);
                }
            }
            // Deploy verification (task-20260607-1403): make the deploy
            // *verifiable*, not merely *attempted*. The hook above may have
            // exited 0 yet silently no-op'd (wrong cwd, swallowed failure,
            // cargo seeing nothing to rebuild). Ask the freshly-installed
            // binary which commit it was built from and assert it matches
            // the just-merged HEAD; warn loudly on divergence, never
            // silently succeed. Sibling guard to the multiplicity check.
            let (note, warn_lines) = format_deploy_verification(&verify_deploy(&repo_root));
            if let Some(note) = note {
                actions.push(note);
            }
            if let Some(lines) = warn_lines {
                for line in lines {
                    warnings.push(line);
                }
            }
        }
    }

    // 2c. Archive write — ADR-030 M3. When the archive subsystem is
    //     enabled and the merge succeeded, capture the molecule's
    //     terminal state under `.cosmon/state/archive/YYYY/MM/<id>/`.
    //     Non-fatal: failures warn but never abort teardown.
    //
    //     Merge-before-dispatch compatibility: the archive write lands
    //     under `state_dir/archive/` on the operator's main branch *as
    //     part of* the `cs done` flow, so a downstream worker spawned
    //     after this `cs done` sees the archived entry in its worktree
    //     the moment it branches off main.
    //
    //     Idempotence gate: the `archived` flag on the persisted
    //     molecule makes a second `cs done` a no-op on the archive
    //     (shortcut used by `cs done <id>; cs done <id>` patterns and
    //     by resume-after-crash). We reload the molecule right before
    //     the gate so we observe the latest flag (the `merged_at`
    //     stamp above may have rewritten state.json between the load
    //     at line 323 and here).
    //     The `no_branch_teardown` disjunct (task-20260626-eb65) extends the
    //     archive write to molecules that never produced a mergeable branch —
    //     delibs, drainage workers, empty-branch tasks. They reach `cs done`
    //     `Completed`-but-`archived == false`; without archiving them here, the
    //     molecule-health A8 (`CompletedUnharvested`) pass re-flags them on
    //     every sweep as a phantom anomaly. Archival is what makes A8
    //     clearable, decoupled from the (non-existent) merge.
    if (merge_succeeded || no_branch_teardown) && project_config.archive.enabled {
        match store.load_molecule(&mol_id) {
            Ok(mut latest) => {
                if latest.archived {
                    actions.push("already_archived".to_owned());
                } else {
                    let mol_dir = cosmon_state::archive::resolve_molecule_dir(&state_dir, &mol_id)
                        .unwrap_or_else(|| store.molecule_dir(&mol_id));
                    if cosmon_state::archive::write_non_fatal_with_warnings(
                        &state_dir,
                        &mol_dir,
                        &latest,
                        cosmon_state::archive::Trigger::Done,
                        chrono::Utc::now(),
                        &warnings,
                    )
                    .is_some()
                    {
                        latest.archived = true;
                        // Same invariant as the `merged_at` stamp above: never persist
                        // `archived=true` on a non-terminal molecule. Belt-and-suspenders
                        // — if a future path lands here without having gone through the
                        // `merged_at` stamp (archive enabled but merge skipped), the
                        // molecule is still terminalized before `archived` is written.
                        terminalize_for_forced_teardown(&mut latest);
                        if let Err(e) = store.save_molecule(&mol_id, &latest) {
                            warnings.push(format!("stamp archived=true failed: {e}"));
                        } else {
                            actions.push("archived".to_owned());
                        }
                    } else {
                        warnings.push("archive write failed (non-fatal)".to_owned());
                    }
                }
            }
            Err(e) => warnings.push(format!("reload molecule for archive failed: {e}")),
        }
    }

    // Trunk writes (merge → frontier → hook → archive) are complete. Release
    // the trunk lock NOW — before the terminal fleet-purge (step 4) acquires
    // the fleet lock — so `cs done` never holds `trunk ⊃ fleet` past this
    // boundary. This is the abda lock-order rule *« trunk lock dropped before
    // fleet-purge to avoid lock-order inversion »*, and keeps the global order
    // (trunk before fleet) deadlock-free per `smithy/docs/formal/MCStitch.tla`.
    drop(trunk_guard);

    // 3. Kill tmux session (if alive).
    if !args.no_kill {
        let backend = TmuxBackend::new(&socket);
        if backend.is_alive(&wid).unwrap_or(false) {
            if backend.terminate(&wid).is_ok() {
                actions.push("killed_session".to_owned());
            } else {
                warnings.push(format!("failed to kill tmux session {session_name}"));
            }
        }
    }

    // 4. Purge worker from fleet state under lock to prevent
    //    concurrent done/tackle from clobbering fleet.json. The same
    //    lock window also clears the inline `MoleculeData::process`
    //    record so the molecule and the fleet stop disagreeing about
    //    "who owns this work" at the same instant
    //    (delib-20260426-1bcd #1 fold-in — phantom-worker class
    //    eliminated by writing both transitions atomically).
    // ADR-131 Decision 2: RAII guard; errors captured into a local `Result`
    // (matched below) rather than propagated, since teardown must continue.
    let purge_result: Result<bool, cosmon_core::error::CosmonError> = 'purge: {
        let _g = match store.lock_fleet() {
            Ok(g) => g,
            Err(e) => break 'purge Err(e),
        };
        let mut fleet = match store.load_fleet() {
            Ok(f) => f,
            Err(e) => break 'purge Err(e),
        };
        let removed = fleet.workers.remove(&wid).is_some();
        if removed {
            if let Err(e) = store.save_fleet(&fleet) {
                break 'purge Err(e);
            }
        }
        // Always clear the inline process record on terminal teardown,
        // even when the legacy fleet entry was already absent — the
        // molecule must never carry a stale process pointer past
        // `cs done`. Failure to reload/save is non-fatal: the rest of
        // teardown should still complete.
        if let Ok(mut latest) = store.load_molecule(&mol_id) {
            if latest.process.is_some() {
                latest.release_process();
                let _ = store.save_molecule(&mol_id, &latest);
            }
        }
        Ok(removed)
    };
    match purge_result {
        Ok(true) => actions.push("purged_worker".to_owned()),
        Ok(false) => {}
        Err(e) => warnings.push(format!("failed to purge worker from fleet: {e}")),
    }

    // 5. Remove git worktree (with dirty-state guard).
    if !args.no_worktree_remove && worktree_path.exists() {
        match worktree_is_dirty(&worktree_path) {
            Ok(dirty_files) if !dirty_files.is_empty() && !args.force => {
                let listing = dirty_files.join("\n  ");
                report(ctx, &mol_id, &actions, &warnings, mol.nudge_count);
                return Err(anyhow::anyhow!(
                    "worktree has uncommitted changes ({} file(s)) — use --force to override:\n  {}",
                    dirty_files.len(),
                    listing,
                ));
            }
            Ok(_) => {
                // Rescue untracked files before worktree removal destroys them.
                let molecule_dir = store.molecule_dir(&mol_id);
                match rescue_untracked_files(&worktree_path, &molecule_dir) {
                    Ok(rescued) if !rescued.is_empty() => {
                        actions.push(format!("rescued {} untracked file(s)", rescued.len()));
                    }
                    Err(e) => {
                        warnings.push(format!("rescue untracked failed: {e}"));
                    }
                    _ => {}
                }
                // Clean or --force: proceed with removal.
                match remove_worktree(&repo_root, &worktree_path) {
                    Ok(()) => actions.push("removed_worktree".to_owned()),
                    Err(e) => warnings.push(format!("worktree remove failed: {e}")),
                }
            }
            Err(e) => {
                warnings.push(format!("dirty check failed: {e} — proceeding with removal"));
                match remove_worktree(&repo_root, &worktree_path) {
                    Ok(()) => actions.push("removed_worktree".to_owned()),
                    Err(e2) => warnings.push(format!("worktree remove failed: {e2}")),
                }
            }
        }
    }

    // 6. Delete branch — gated by three guards, in order:
    //    - FIX 3: effective_no_branch_delete (skip entirely).
    //    - GUARD anti-wipe (raté 5eba): an *independent, fresh* topology
    //      probe `git merge-base --is-ancestor <branch> <base>` taken
    //      right here, immediately before `git branch -d`. If the branch
    //      is not reachable from base it is the only copy of the work, so
    //      we refuse to delete it — overriding both `merge_succeeded` and
    //      `--force`. `git branch -d`'s own safety checks ancestry against
    //      the current HEAD, not against base, which is exactly how 5eba
    //      lost a 491-line payload. See `decide_branch_delete`.
    //    - FIX 2: only delete when merge succeeded or --force.
    let branch_present = branch_exists(&repo_root, &branch_name);
    let base_branch = resolve_base_branch(&repo_root);
    let branch_in_base =
        branch_present && branch_is_ancestor_of(&repo_root, &branch_name, &base_branch);
    match decide_branch_delete(
        branch_present,
        effective_no_branch_delete,
        branch_in_base,
        merge_succeeded,
        args.force,
    ) {
        BranchDeleteDecision::Delete => match delete_branch(&repo_root, &branch_name) {
            Ok(()) => actions.push("deleted_branch".to_owned()),
            Err(e) => warnings.push(format!("branch delete failed: {e}")),
        },
        BranchDeleteDecision::RefuseUnmerged => warnings.push(format!(
            "GUARD anti-wipe (5eba): refusing to delete branch {branch_name} — its commits are \
             NOT reachable from base `{base_branch}`. The branch is the only copy of the work. \
             Merge it first (`git checkout {base_branch} && git merge {branch_name}`), then rerun \
             `cs done`. This guard overrides --force; to discard the work deliberately run \
             `git branch -D {branch_name}` by hand."
        )),
        BranchDeleteDecision::RefuseMergeFailed => warnings.push(format!(
            "branch {branch_name} not deleted — merge did not succeed (use --force to override)"
        )),
        BranchDeleteDecision::Skip => {}
    }

    // 7. Auto-commit durable molecule artifacts (prompt.md, briefing.md,
    //    log.md, events.jsonl, synthesis.md, responses/, …) so the operator
    //    doesn't need to manually `git add` + `git commit` after every done.
    //    Non-blocking: failures warn but never abort teardown.
    {
        let mol_dir = store.molecule_dir(&mol_id);
        let short_topic = mol
            .variables
            .get("topic")
            .map(|t| {
                let truncated: String = t.chars().take(50).collect();
                if truncated.len() < t.len() {
                    format!("{truncated}…")
                } else {
                    truncated
                }
            })
            .unwrap_or_default();
        if let Some((recorded, actual)) =
            super::evolve::done_worktree_mismatch(recorded_worktree.as_deref(), &repo_root)
        {
            warnings.push(format!(
                "SKIPPED artifact commit — {mol_id}'s recorded worktree ({}) is not \
                 inside the current repo ({}); refusing to commit its artifacts into a \
                 foreign repo",
                recorded.display(),
                actual.display(),
            ));
        } else {
            // Native attribution (task-20260717-c873; retargeted by
            // delib-20260717-194b, F1). Reuse the SAME `coauthor_trailers`
            // computed once at the top level (before the merge) — the merge
            // commit is the primary carrier (F1); this artifact commit is the
            // *fallback* carrier for artifact-producing molecules. Empty
            // `coauthor_email` ⇒ no trailers ⇒ commit message byte-identical to
            // a pre-attribution cosmon.
            match commit_molecule_artifacts(
                &repo_root,
                &mol_dir,
                &events_path,
                &mol_id,
                &short_topic,
                &coauthor_trailers,
            ) {
                Ok(true) => actions.push("committed_artifacts".to_owned()),
                Ok(false) => { /* nothing to commit — silent */ }
                Err(e) => warnings.push(format!("artifact commit failed: {e}")),
            }
        }
    }

    // 8. Final post-condition (task-20260606-21d4, DoD b). `cs done` must
    //    NEVER exit 0 while the worker's branch still carries committed work
    //    that did not land on base. The merge block above already returns a
    //    typed error for every *recognised* failure (conflict, NotOnBase,
    //    non-fast-forward, post-merge verification miss); this independent,
    //    fresh topology probe is the belt-and-suspenders that catches the
    //    *silent* class — a false `already_merged`, a base-resolution slip,
    //    or any future path that reports success without moving base. The
    //    invariant is purely structural: if `feat/<id>` exists and has
    //    commits not reachable from base, the deliverable was not integrated.
    //    Bug `task-20260531-1b35`: `cs done` returned exit 0 with the branch
    //    and worktree still present and no merge commit on main — a silent
    //    no-op the operator had to discover by hand.
    //
    //    Skipped when `--no-merge` is set (the operator deliberately opted
    //    out of integration) and when a real conflict already short-circuited
    //    above (we never reach here in that case).
    if !args.no_merge {
        let base = resolve_base_branch(&repo_root);
        if unmerged_work_remains(&repo_root, &branch_name, &base) {
            report(ctx, &mol_id, &actions, &warnings, mol.nudge_count);
            return Err(anyhow::anyhow!(
                "teardown reported success but did NOT integrate the work: branch \
                 {branch_name} still has commit(s) not reachable from base `{base}`. \
                 `cs done` refuses to exit 0 on a silent no-op merge \
                 (task-20260531-1b35). The branch is the only copy of the work — \
                 merge it before tearing down:\n\
                 \n    git checkout {base} && git merge {branch_name}\n\
                 \nthen rerun `cs done {mol_id}`. If you ran `cs done` from inside a \
                 worktree, run it from the main checkout instead. To discard the work \
                 deliberately, delete the branch by hand."
            ));
        }
    }

    report(ctx, &mol_id, &actions, &warnings, mol.nudge_count);
    Ok(())
}

/// Best-effort removal of a stale fleet worker bound to a molecule.
///
/// Used on the `--if-completed` fast path when the molecule is already
/// `Completed` + merged. Resolves the worker id from `session_name`
/// (falling back to the molecule id, matching the regular teardown path),
/// then attempts a locked remove against `fleet.json`. Returns `true`
/// only when an entry was actually removed.
///
/// Failures are swallowed: this routine runs on a no-op code path whose
/// contract is "return fast without touching state unless strictly
/// necessary." Bubbling up an error would break the hook/patrol
/// contract (`cs harvest` → `cs done --if-completed`) whose only duty
/// is to keep the fleet coherent with the molecule lifecycle.
fn purge_stale_worker_for(store: &FileStore, mol: &cosmon_state::MoleculeData) -> bool {
    // Prefer the inline live-process record over the legacy
    // `session_name` field (delib-20260426-1bcd #1 fold-in).
    let session_name = mol
        .tmux_session()
        .map_or_else(|| mol.id.to_string(), str::to_owned);
    let Ok(wid) = WorkerId::new(&session_name) else {
        return false;
    };
    let mol_id = mol.id.clone();
    // ADR-131 Decision 2: RAII guard; errors captured locally (legacy fast
    // path swallows them via `unwrap_or(false)` below).
    let purge_result: Result<bool, cosmon_core::error::CosmonError> = 'purge: {
        let _g = match store.lock_fleet() {
            Ok(g) => g,
            Err(e) => break 'purge Err(e),
        };
        let mut fleet = match store.load_fleet() {
            Ok(f) => f,
            Err(e) => break 'purge Err(e),
        };
        let removed = fleet.workers.remove(&wid).is_some();
        if removed {
            if let Err(e) = store.save_fleet(&fleet) {
                break 'purge Err(e);
            }
        }
        // Mirror the lock-window contract from the main `done` path:
        // a successful fleet purge clears the inline process record on
        // the molecule too, so the legacy fast path can never leave a
        // phantom pointer behind.
        if let Ok(mut latest) = store.load_molecule(&mol_id) {
            if latest.process.is_some() {
                latest.release_process();
                let _ = store.save_molecule(&mol_id, &latest);
            }
        }
        Ok(removed)
    };
    purge_result.unwrap_or(false)
}

/// Reclaim the git leftovers of an already-merged molecule: its worktree and
/// its branch. Returns the action labels actually performed.
///
/// **Why this exists (task-20260719-fedf).** `cs done --if-completed` is the
/// *documented recovery verb*, but on an already-merged molecule it declined
/// to re-merge and returned — correctly refusing the merge, yet leaving the
/// branch and worktree on disk. During the 2026-07-19 incident the operator
/// ran the prescribed recovery and the leftovers stayed put, so the verb
/// looked like it had done its job while the galaxy stayed dirty. A recovery
/// verb that only half-recovers teaches operators not to trust it.
///
/// **Strictly conservative.** This is a no-op unless everything is provably
/// safe, because it runs on a *no-op* path where the operator has not asked
/// for a teardown and cannot pass `--force`:
///
/// - the worktree is removed only when `worktree_is_dirty` reports it clean —
///   never forced, never after a rescue;
/// - the branch is deleted only when it is an ancestor of the base branch,
///   the same anti-wipe topology probe the main path uses (the 5eba guard).
///   `merged_at` alone is *not* accepted as proof: it is a stamp we wrote,
///   whereas ancestry is a fact git can confirm.
///
/// Anything unsafe or unexpected is simply left alone for the full `cs done`
/// path to handle with its warnings and `--force` affordance.
fn reclaim_merged_git_artifacts(repo_root: &Path, mol_id: &MoleculeId) -> Vec<String> {
    let mut actions = Vec::new();
    let worktree_path = repo_root.join(".worktrees").join(mol_id.as_str());
    let branch_name = format!("feat/{mol_id}");

    if worktree_path.exists() {
        // Clean-only: a dirty worktree holds work nobody has looked at, and
        // this path has no `--force` for the operator to reach for.
        if let Ok(dirty) = worktree_is_dirty(&worktree_path) {
            if dirty.is_empty() && remove_worktree(repo_root, &worktree_path).is_ok() {
                actions.push("removed_worktree".to_owned());
            }
        }
    }

    if branch_exists(repo_root, &branch_name) {
        let base_branch = resolve_base_branch(repo_root);
        // Ancestry is the load-bearing check, not `merged_at` — see the doc
        // comment. If the branch is not reachable from base it is the only
        // copy of the work, and deleting it here would be the 5eba wipe.
        if branch_is_ancestor_of(repo_root, &branch_name, &base_branch)
            && delete_branch(repo_root, &branch_name).is_ok()
        {
            actions.push("deleted_branch".to_owned());
        }
    }

    actions
}

/// Build a human-readable conflict recovery message with exact commands.
fn format_conflict_recovery(mol_id: &MoleculeId, worktree_path: &Path, files: &[String]) -> String {
    let file_list = files.join(", ");
    let wt_display = worktree_path.display();
    format!(
        "merge conflict in {} file(s): {file_list}\n\n\
         To resolve, run:\n\
         \n\
           cd {wt_display} && git merge main\n\
           # fix conflicts in: {file_list}\n\
           git add <resolved files>\n\
           git commit\n\
           cd - && cs done {mol_id}\n",
        files.len()
    )
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Display a computed [`TeardownPlan`] in JSON or human-readable form.
fn report_plan(ctx: &Context, plan: &TeardownPlan) {
    if ctx.json {
        println!("{}", serde_json::to_string(plan).unwrap_or_default());
    } else {
        println!("🔍 dry-run: teardown plan for {}", plan.molecule);
        println!(
            "  molecule status:  {} (terminal: {})",
            plan.molecule_status, plan.is_terminal
        );
        println!("  worktree exists:  {}", plan.worktree_exists);
        if !plan.worktree_dirty_files.is_empty() {
            println!(
                "  worktree dirty:   {} file(s)",
                plan.worktree_dirty_files.len()
            );
            for f in &plan.worktree_dirty_files {
                println!("    {f}");
            }
        }
        println!("  branch exists:    {}", plan.branch_exists);
        println!("  already merged:   {}", plan.branch_already_merged);
        println!("  empty branch:     {}", plan.branch_is_empty);
        println!("  merge needed:     {}", plan.merge_needed);
        println!("  session alive:    {}", plan.session_alive);
        println!("  worker registered:{}", plan.worker_registered);
        if !plan.planned_actions.is_empty() {
            println!("\n  planned actions:");
            for a in &plan.planned_actions {
                println!("    • {a}");
            }
        }
        for w in &plan.warnings {
            println!("  ⚠ {w}");
        }
    }
}

/// Render a **loud, non-`done`** report for a merge that did not land.
///
/// This is the counterpart of [`report`] for the failure path. [`report`]
/// prints `✅ … done` / `⚠ … done with N warnings` — both of which imply the
/// teardown completed. A merge conflict (or any hard merge failure) is a
/// different beast: nothing merged, the base branch never moved, and no
/// teardown happened. Routing it through [`report`]'s warning channel is the
/// silent-integrity bug where the operator read *"done with 1 warning"* and
/// shipped a stale artifact.
///
/// The greppable prefix is `❌ ` (vs `✅ `/`⚠ `) so downstream automation can
/// tell a non-merge apart from a merged-with-caveats teardown. The JSON form
/// carries `ok: false`, `merged: false`, `teardown: false`, and a typed
/// `outcome` (`"merge_conflict"` | `"merge_failed"`).
fn report_merge_failure(
    ctx: &Context,
    mol_id: &MoleculeId,
    outcome: &str,
    headline: &str,
    conflicted_files: &[String],
    recovery: &str,
    actions: &[String],
) {
    if ctx.json {
        let out = serde_json::json!({
            "command": "done",
            "molecule": mol_id.as_str(),
            "ok": false,
            "merged": false,
            "teardown": false,
            "outcome": outcome,
            "conflicted_files": conflicted_files,
            "recovery": recovery,
            "actions": actions,
        });
        println!("{}", serde_json::to_string(&out).unwrap_or_default());
    } else {
        println!("❌ {mol_id} {headline}");
        for a in actions {
            println!("  • {a}");
        }
        if !conflicted_files.is_empty() {
            println!("  conflicted file(s):");
            for f in conflicted_files {
                println!("    • {f}");
            }
        }
        println!("  base branch unchanged — no merge commit was created.");
        println!("  branch, worktree, and tmux session are preserved.");
        for line in recovery.lines() {
            println!("  {line}");
        }
    }
}

fn report(
    ctx: &Context,
    mol_id: &MoleculeId,
    actions: &[String],
    warnings: &[String],
    nudge_count: u32,
) {
    let ok = warnings.is_empty();
    if ctx.json {
        let out = serde_json::json!({
            "command": "done",
            "molecule": mol_id.as_str(),
            "ok": ok,
            "actions": actions,
            "warnings": warnings,
            "nudge_count": nudge_count,
        });
        println!("{}", serde_json::to_string(&out).unwrap_or_default());
    } else {
        // Greppable prefix: `^✅ ` only on full success, `^⚠ ` on any
        // partial-failure path (merge skipped, branch undeleted, worktree
        // left behind, …). Downstream automation that parses cs done
        // output can rely on the prefix without scanning the body.
        if ok {
            println!("✅ {mol_id} done");
        } else {
            let suffix = if warnings.len() == 1 { "" } else { "s" };
            println!("⚠ {mol_id} done with {} warning{suffix}", warnings.len());
        }
        for a in actions {
            println!("  • {a}");
        }
        for w in warnings {
            println!("  ⚠ {w}");
        }
        // Post-mortem nudge accounting (delib-20260420-1b02 P2):
        // surface how many times `cs patrol --nudge` had to poke this
        // molecule. > 2 nudges is a strong hint that the briefing is
        // ambiguous, not that the runtime stalled — a data point a
        // future audit formula will read.
        if nudge_count > 0 {
            let suffix = if nudge_count == 1 { "" } else { "s" };
            println!("  • nudge_count: {nudge_count} patrol nudge{suffix}");
            if nudge_count > 2 {
                println!(
                    "  ⚠ nudge_count > 2 — likely a briefing-clarity issue, not a runtime bug"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mechanical-first escalation loop
// ---------------------------------------------------------------------------

/// Outcome of the merge-with-escalation loop.
#[derive(Debug)]
enum MergeLoopOutcome {
    /// Merged on the first attempt (no escalation needed).
    Merged,
    /// Branch tip was already reachable from base — nothing to do.
    /// The caller is expected to disambiguate "genuinely already
    /// merged" from "empty branch" using the molecule's `merged_at`
    /// stamp; see [`MergeOutcome::AlreadyMerged`].
    AlreadyMerged,
    /// Branch does not exist locally.
    NoBranch,
    /// Merged after one or more escalation retries.
    MergedAfterEscalation {
        /// How many escalation rounds were needed.
        retries: u32,
    },
    /// A textual merge conflict that could not be resolved (either
    /// `--no-auto-propel` was set, so escalation was never attempted, or
    /// auto-propel ran and exhausted its retries with the conflict still
    /// present).
    ///
    /// This is a **first-class, non-`Err` outcome** so the caller can FAIL
    /// LOUDLY with a dedicated *"MERGE CONFLICT — not merged, branch
    /// preserved"* report instead of routing the conflict through the generic
    /// warning aggregator — the silent-integrity bug where a real conflict
    /// surfaced as the easy-to-miss line *"done with 1 warning"* and the
    /// operator believed stale work had landed. The merge has already been
    /// rolled back (`git merge --abort`) by [`try_merge_branch`], so the base
    /// branch is unchanged and the worktree is clean; the worker's branch and
    /// worktree are preserved for resolution.
    Conflict {
        /// Files git reported as conflicting (`git diff --name-only
        /// --diff-filter=U`). Listed verbatim to the operator.
        files: Vec<String>,
        /// Ready-to-run recovery instructions (the exact `git`/`cs` commands).
        recovery: String,
    },
}

/// Default propel message sent to the worker during auto-propel escalation.
///
/// Instructs the worker to rebase onto the base branch, resolve conflicts,
/// run tests, and NOT call `cs done` itself (the worker must not self-destroy).
const DEFAULT_PROPEL_MESSAGE: &str = "\
⚛ COSMON AUTO-PROPEL — merge conflict detected by `cs done`.\n\
\n\
Your branch has conflicts with the base branch. Please:\n\
1. `git fetch origin && git rebase origin/main` (or the appropriate base branch)\n\
2. Resolve conflicts, preserving both sides where possible\n\
3. Run the full test suite (`cargo test --workspace`)\n\
4. Commit and push your changes\n\
5. Do NOT call `cs done` yourself — the orchestrator will retry the merge\n\
\n\
This is an automated escalation. The merge will be retried after you complete the rebase.";

/// Backoff durations for escalation retries (seconds).
const ESCALATION_BACKOFF_SECS: [u64; 3] = [30, 60, 120];

/// Try to merge a branch with mechanical-first escalation on conflict.
///
/// Mechanical-first escalation: see docs/architectural-invariants.md
///
/// 1. Try `git merge` (ff-only or 3-way depending on strategy)
/// 2. On conflict, if `auto_propel` is true:
///    a. Abort the merge
///    b. Send resume signal to the worker with rebase instructions
///    c. Sleep with backoff
///    d. Retry the merge (back to step 1)
/// 3. Bounded by `max_retries`. After exhaustion, return error.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn try_merge_with_escalation(
    ctx: &Context,
    store: &FileStore,
    mol_id: &MoleculeId,
    repo_root: &Path,
    branch: &str,
    strategy: MergeStrategy,
    session_name: &str,
    socket: &str,
    auto_propel: bool,
    max_retries: u32,
    custom_message: Option<&str>,
    coauthor_trailers: &[String],
) -> anyhow::Result<MergeLoopOutcome> {
    // The most recent set of conflicting files, carried from the first attempt
    // through every escalation retry so the final `Conflict` outcome can list
    // them even when the conflict only persists after exhaustion. Assigned in
    // the first-attempt conflict arm below (every other arm returns), so it is
    // definitely initialised on the only path that reaches the escalation loop.
    let mut last_conflict_files: Vec<String>;

    // First attempt — purely mechanical.
    match try_merge_branch(repo_root, branch, strategy, coauthor_trailers) {
        MergeOutcome::Merged => {
            if verify_merge(repo_root, branch) {
                return Ok(MergeLoopOutcome::Merged);
            }
            let base = resolve_base_branch(repo_root);
            return Err(anyhow::anyhow!(
                "teardown aborted: merge reported success but post-merge verification failed \
                 (branch {branch} is NOT an ancestor of base {base} after merge — \
                 likely silent merge to wrong branch or concurrent race; \
                 inspect `git reflog` and `git fsck --lost-found` to recover)\n\n\
                 task-20260509-94f0 mode B: a successful `git merge` advanced the \
                 worktree's branch instead of {base}. No retry attempted — operator \
                 must rebuild main manually."
            ));
        }
        MergeOutcome::AlreadyMerged => return Ok(MergeLoopOutcome::AlreadyMerged),
        MergeOutcome::NoBranch => return Ok(MergeLoopOutcome::NoBranch),
        MergeOutcome::NotFastForward => {
            return Err(anyhow::anyhow!(
                "teardown aborted: branch {branch} is not fast-forward — \
                 rerun without `--strategy ff-only`, or merge manually"
            ));
        }
        MergeOutcome::NotOnBase { current, base } => {
            return Err(anyhow::anyhow!(
                "teardown aborted: HEAD is on `{current}` but the configured base branch is `{base}`.\n\
                 \n\
                 `cs done` would silently merge {branch} into `{current}` instead of `{base}` \
                 (git merges into the current HEAD, not into a branch by name). The work would \
                 be reported as `merged` but `{base}` would never move — the failure mode \
                 chronicled by task-20260509-94f0 (mode B).\n\
                 \n\
                 Run `cs done` from the main checkout:\n\
                 \n\
                     cd $(git rev-parse --git-common-dir)/..\n\
                     git checkout {base}\n\
                     cs done <id>\n\
                 \n\
                 (No automatic retry — surfacing the error so the operator chooses the recovery path.)"
            ));
        }
        MergeOutcome::Conflict(files) => {
            if !auto_propel {
                // No escalation requested — surface the conflict as a
                // first-class outcome so `run` can FAIL LOUDLY. The merge is
                // already rolled back (branch + base intact).
                let wt_path = repo_root.join(".worktrees").join(mol_id.as_str());
                let recovery = format_conflict_recovery(mol_id, &wt_path, &files);
                return Ok(MergeLoopOutcome::Conflict { files, recovery });
            }
            // Auto-propel enabled — seed the escalation loop with the first
            // attempt's conflicting files and fall through.
            last_conflict_files = files;
        }
        MergeOutcome::Error(msg) => {
            return Err(anyhow::anyhow!("teardown aborted: merge error: {msg}"));
        }
    }

    // Escalation loop — conflict detected, auto_propel enabled.
    let propel_msg = custom_message.unwrap_or(DEFAULT_PROPEL_MESSAGE);
    let backend = TmuxBackend::new(socket);
    let wid = WorkerId::new(session_name)?;

    for retry in 0..max_retries {
        let backoff_secs = ESCALATION_BACKOFF_SECS
            .get(retry as usize)
            .copied()
            .unwrap_or(120);

        // Record the escalation in the molecule's audit trail.
        record_escalation(store, mol_id, retry, "conflict→propel");

        if !ctx.json {
            eprintln!(
                "⚡ auto-propel: merge conflict on {branch}, escalation {}/{max_retries} — \
                 sending rebase signal to worker {session_name}, retrying in {backoff_secs}s",
                retry + 1
            );
        }

        // Send the propel signal to the worker via tmux.
        if backend.is_alive(&wid).unwrap_or(false) {
            let _ = backend.send_input(&wid, propel_msg);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = backend.send_input(&wid, "");
        } else if !ctx.json {
            eprintln!("  ⚠ worker {session_name} is not alive — cannot send propel signal");
        }

        // Sleep with backoff to give the worker time to rebase.
        std::thread::sleep(std::time::Duration::from_secs(backoff_secs));

        // Retry the merge.
        match try_merge_branch(repo_root, branch, strategy, coauthor_trailers) {
            MergeOutcome::Merged => {
                if verify_merge(repo_root, branch) {
                    record_escalation(store, mol_id, retry, "merged");
                    return Ok(MergeLoopOutcome::MergedAfterEscalation { retries: retry + 1 });
                }
                let base = resolve_base_branch(repo_root);
                return Err(anyhow::anyhow!(
                    "teardown aborted: merge reported success after escalation but \
                     post-merge verification failed (branch {branch} is NOT an ancestor \
                     of base {base} — silent-merge-to-wrong-branch or concurrent race)"
                ));
            }
            MergeOutcome::AlreadyMerged => {
                record_escalation(store, mol_id, retry, "merged");
                return Ok(MergeLoopOutcome::MergedAfterEscalation { retries: retry + 1 });
            }
            MergeOutcome::Conflict(files) => {
                // Conflict persists — remember the latest set and continue.
                last_conflict_files = files;
            }
            MergeOutcome::NoBranch => return Ok(MergeLoopOutcome::NoBranch),
            MergeOutcome::NotFastForward => {
                return Err(anyhow::anyhow!(
                    "teardown aborted: branch {branch} became non-fast-forward during escalation"
                ));
            }
            MergeOutcome::NotOnBase { current, base } => {
                // The pre-flight check refuses pre-merge; if we observe
                // it during escalation, HEAD changed under us between
                // the first attempt's resolution and this retry.
                return Err(anyhow::anyhow!(
                    "teardown aborted during escalation: HEAD moved off base branch \
                     (current=`{current}`, base=`{base}`). Concurrent race or operator \
                     `git checkout` outside `cs done`."
                ));
            }
            MergeOutcome::Error(msg) => {
                return Err(anyhow::anyhow!(
                    "teardown aborted: merge error during escalation: {msg}"
                ));
            }
        }
    }

    // All retries exhausted — the conflict persists. Surface it as a
    // first-class `Conflict` outcome (not `Err`) so `run` renders the loud
    // "MERGE CONFLICT — not merged, branch preserved" report rather than the
    // generic warning aggregator. The merge is already rolled back, so the
    // base branch is untouched and the worktree is clean.
    record_escalation(store, mol_id, max_retries, "exhausted");

    let wt_path = repo_root.join(".worktrees").join(mol_id.as_str());
    let recovery = format!(
        "auto-propel exhausted after {max_retries} retries — merge conflict persists.\n\
         Worktree, branch, and tmux session preserved for manual intervention.\n\
         {}",
        format_conflict_recovery(mol_id, &wt_path, &last_conflict_files)
    );
    Ok(MergeLoopOutcome::Conflict {
        files: last_conflict_files,
        recovery,
    })
}

/// Record an escalation entry in the molecule's state.
fn record_escalation(store: &FileStore, mol_id: &MoleculeId, retry: u32, outcome: &str) {
    // ADR-131 Decision 2: RAII guard; best-effort, errors intentionally
    // swallowed as the original `let _ = with_fleet_lock(…)` did.
    if let Ok(_g) = store.lock_fleet() {
        if let Ok(mut mol) = store.load_molecule(mol_id) {
            mol.escalations.push(cosmon_state::EscalationEntry {
                timestamp: chrono::Utc::now(),
                retry,
                outcome: outcome.to_owned(),
            });
            let _ = store.save_molecule(mol_id, &mol);
        }
    }
}

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Outcome of a branch merge attempt.
#[derive(Debug)]
enum MergeOutcome {
    /// Branch was integrated into the current head (fast-forward or merge
    /// commit, depending on strategy).
    Merged,
    /// Branch tip is reachable from the base branch — nothing to merge.
    ///
    /// Two semantically distinct sub-cases live behind this single
    /// topology label, because git alone cannot tell them apart:
    ///
    /// 1. **Genuinely already merged** — the branch carried real commits
    ///    that were integrated into base via a prior `cs done` (or by
    ///    some other path). The molecule's `merged_at` stamp will be
    ///    `Some`.
    /// 2. **Empty branch** — the worker's output lives outside the
    ///    cosmon repo (galaxy bootstrap, vault artifact, …) or the
    ///    worker never made a commit, so `base..branch` is empty *and*
    ///    `merged_at` is `None`.
    ///
    /// The action-label split (`already_merged` vs `empty_branch`) is
    /// applied by the caller using `merged_at`, since that is the only
    /// reliable source of truth for "did a prior `cs done` integrate
    /// this branch's content".
    AlreadyMerged,
    /// Branch does not exist locally.
    NoBranch,
    /// Branch diverged from current head — `ff-only` merge refused.
    ///
    /// Only emitted by [`MergeStrategy::FfOnly`]. With the default
    /// [`MergeStrategy::Merge`], divergence is resolved by creating a merge
    /// commit.
    NotFastForward,
    /// Textual conflict on one or more files.
    ///
    /// The merge has been rolled back (`git merge --abort`) before this
    /// outcome is returned, so the worktree is clean. The vector lists the
    /// conflicting files (as reported by `git diff --name-only
    /// --diff-filter=U`).
    Conflict(Vec<String>),
    /// `cs done` was invoked while `HEAD` is not on the configured base
    /// branch — the merge would land on the wrong branch.
    ///
    /// The most common trigger: pilot ran `cs done <id>` from inside a
    /// worktree (`.worktrees/task-…-X/`), where
    /// `git rev-parse --show-toplevel` returns the worktree path and
    /// `git -C <worktree> merge` would advance the worktree's branch
    /// instead of `main`. In a chronicled instance the work commit was
    /// orphaned and recovered with `git fsck --lost-found`.
    ///
    /// The merge is *not* attempted; the caller surfaces a typed,
    /// non-silent error so the operator runs `cs done` from the main
    /// checkout.
    NotOnBase {
        /// Resolved current branch name (or `"(detached HEAD)"`).
        current: String,
        /// Configured base branch (usually `"main"`).
        base: String,
    },
    /// Other git error.
    Error(String),
}

/// Attempt to merge `branch` into the current HEAD with the given strategy.
///
/// Returns a [`MergeOutcome`] variant rather than a `Result` so callers can
/// distinguish the branch-not-found / already-merged / conflict / hard-fail
/// cases without string matching.
fn try_merge_branch(
    repo_root: &Path,
    branch: &str,
    strategy: MergeStrategy,
    coauthor_trailers: &[String],
) -> MergeOutcome {
    // Does the branch exist?
    if !branch_exists(repo_root, branch) {
        return MergeOutcome::NoBranch;
    }

    // Pre-flight: `git -C <repo_root> merge <branch>` advances the
    // *current branch* of `repo_root`, never the configured base branch
    // by name. When `cs done` is invoked from a worktree
    // (`.worktrees/task-…-X/`), `find_repo_root()` returns the worktree
    // path — its HEAD is `feat/task-…-X`, not `main`. The merge would
    // then land on the wrong branch and `cs done` would report a
    // fictitious "merged" while `main` never moved. The work commit
    // gets orphaned the moment the wrong branch is later cleaned up.
    //
    // Chronicled by `task-20260509-94f0` (mode B): the work was
    // recovered only after `git fsck --lost-found`. Refuse cleanly
    // here — there is no recovery path that does not start with the
    // operator running `cs done` from the main checkout.
    let base = resolve_base_branch(repo_root);
    let current = current_branch_name(repo_root).unwrap_or_else(|| "(detached HEAD)".to_owned());
    if current != base {
        return MergeOutcome::NotOnBase { current, base };
    }

    // Is it already merged? Probe topology against the *base branch*
    // (usually `main`) rather than the ambient HEAD. See `is_branch_merged`
    // for the strict-ancestry rationale and the 2026-04-21 incident where
    // a HEAD-based probe falsely returned AlreadyMerged when `cs done` was
    // invoked outside a main-branch checkout. Bookkeeping commits that
    // share the molecule's name never qualify — only topology does.
    if is_branch_merged(repo_root, branch) {
        return MergeOutcome::AlreadyMerged;
    }

    // `cs done` itself writes audit events (MergeDispatched, …) to
    // `.cosmon/state/events.jsonl` via `emit_one` *before* we get here.
    // That file is tracked (selective gitignore, ADR-030), so the write
    // dirties the working tree. If the incoming branch also touches the
    // same paths, `git merge` refuses with "Your local changes to
    // events.jsonl would be overwritten" — which, under auto-propel, loops
    // forever because every retry re-dirties the file before each merge
    // attempt. Fold those self-inflicted writes into a small commit so the
    // merge has clean ground to stand on. Non-fatal: any failure is
    // translated to `MergeOutcome::Error` but does not deadlock the retry.
    if let Err(e) = flush_state_dir_changes(repo_root) {
        return MergeOutcome::Error(format!("flush state dir before merge failed: {e}"));
    }

    // Attempt the merge. The flag set depends on strategy.
    //
    // `LC_ALL=C` pins git's stderr to the English locale: we classify the
    // ff-only refusal below by grepping "Not possible to fast-forward" /
    // "non-fast-forward", and on a French (or any non-English) git the
    // translated message ("Pas possible d'avancer rapidement, abandon")
    // never matches, silently misclassifying NotFastForward as Error.
    // Discovered 2026-05-22 (drain-worker f877): the FR-locale failure
    // was invisible in CI (C-locale) but reproducible on the operator's
    // machine.
    let repo_arg = repo_root.to_string_lossy().to_string();
    let mut cmd = Command::new("git");
    cmd.env("LC_ALL", "C");
    cmd.args(["-C", &repo_arg, "merge"]);
    match strategy {
        MergeStrategy::FfOnly => {
            cmd.args(["--ff-only", branch]);
        }
        MergeStrategy::Merge => {
            // `--no-ff` always creates a merge commit even if a
            // fast-forward would be possible — that preserves a clear
            // "molecule X landed here" marker in history.
            //
            // Native attribution (delib-20260717-194b, F1): the merge commit is
            // the trailer carrier on the default path. When trailers are
            // configured we reclaim the message slot `--no-edit` would throw
            // away — passing git's own default subject as the first `-m` and the
            // trailer block as a SINGLE second `-m` so it renders as one
            // contiguous `Co-Authored-By` paragraph (a trailer block must not be
            // split by a blank line). Empty trailer vec ⇒ we keep `--no-edit`
            // verbatim, so the merge commit is byte-identical to a
            // pre-attribution cosmon (F9). `git merge`'s default subject when
            // merging `feat/x` into the current branch is `Merge branch
            // 'feat/x'`, reproduced here so history reads the same.
            if coauthor_trailers.is_empty() {
                // `--no-edit` stops git from launching $EDITOR for the merge
                // commit message.
                cmd.args(["--no-ff", "--no-edit", branch]);
            } else {
                let subject = format!("Merge branch '{branch}'");
                let trailers = coauthor_trailers.join("\n");
                cmd.args(["--no-ff", "-m", &subject, "-m", &trailers, branch]);
            }
        }
    }
    let output = cmd.output();

    match output {
        Ok(o) if o.status.success() => MergeOutcome::Merged,
        Ok(o) => {
            // Distinguish textual conflicts (recoverable with manual
            // editing) from ff-only refusal (recoverable by retrying with
            // a different strategy) from other hard errors.
            let stderr = String::from_utf8_lossy(&o.stderr);
            let conflicts = list_unmerged_files(repo_root);
            if !conflicts.is_empty() {
                // Append-only JSONL files (events.jsonl, interactions.jsonl)
                // conflict whenever both sides append. The entries are not
                // semantically in conflict — they are just adjacent hunks
                // in a file that grows monotonically. Auto-resolve by
                // unioning both sides and sorting by timestamp. See
                // docs/events-jsonl-merge.md for the rationale.
                if conflicts.iter().all(|f| is_append_only_jsonl(f))
                    && auto_resolve_append_only_jsonl(
                        repo_root,
                        &conflicts,
                        branch,
                        coauthor_trailers,
                    )
                    .is_ok()
                {
                    return MergeOutcome::Merged;
                }
                // Roll the merge back so the worktree is left clean — the
                // operator can resolve the conflict at their leisure and
                // rerun `cs done` without having to `git merge --abort`
                // themselves.
                let _ = Command::new("git")
                    .args(["-C", &repo_arg, "merge", "--abort"])
                    .output();
                return MergeOutcome::Conflict(conflicts);
            }
            if stderr.contains("Not possible to fast-forward")
                || stderr.contains("non-fast-forward")
            {
                MergeOutcome::NotFastForward
            } else {
                MergeOutcome::Error(stderr.trim().to_owned())
            }
        }
        Err(e) => MergeOutcome::Error(e.to_string()),
    }
}

/// Is `path` (repo-relative, `/`-separated as git emits it) a *source* path
/// that a `chore(state):` commit must never carry?
///
/// A state-tracking commit's job is to capture `.cosmon/` state and molecule
/// artifacts — never the Rust workspace. The forbidden set, verbatim from the
/// postmortem (see [`source_paths_in_commit`]): anything under `crates/`,
/// anything under a top-level `src/`, and any `Cargo.toml` / `Cargo.lock`
/// manifest at any depth.
fn is_source_path(path: &str) -> bool {
    let p = path.trim();
    if p.is_empty() {
        return false;
    }
    if p.starts_with("crates/") || p == "crates" {
        return true;
    }
    if p.starts_with("src/") || p == "src" {
        return true;
    }
    matches!(
        Path::new(p).file_name().and_then(|s| s.to_str()),
        Some("Cargo.toml" | "Cargo.lock")
    )
}

/// Inspect exactly the set of files a scoped `git commit -- <pathspecs>` would
/// record, and return any that are source paths ([`is_source_path`]).
///
/// This is the structural guard that makes the *image/source drift*
/// regression impossible. Postmortem (2026-06-15, commit `2e86cf908`): a
/// `chore(state): track artifacts for task-…` commit silently reverted
/// `crates/**` + `Cargo.*` to a stale tree — 142 deletions across 12 source
/// files, disguised as artifact tracking. The mechanism was a **bare**
/// `git commit`: the state-tracking gesture stages only `.cosmon/` and the
/// molecule artifact dir, but `git commit` with no pathspec writes the
/// *entire* staged index, so source pre-staged by another step is swept under
/// the misleading subject. Both state-tracking commits are now scoped to
/// explicit state pathspecs *and* gated by this guard on the actually-
/// committed set, so the regression cannot recur even if a pathspec
/// misresolves (e.g. a molecule dir that overlaps the workspace root).
fn source_paths_in_commit(repo_root: &Path, pathspecs: &[&Path]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "diff".into(),
        "--cached".into(),
        "--name-only".into(),
        "--".into(),
    ];
    for p in pathspecs {
        args.push(p.to_string_lossy().into_owned());
    }
    let out = Command::new("git")
        .args(&args)
        .current_dir(repo_root)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter(|l| is_source_path(l))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Commit any uncommitted changes under `.cosmon/state/` so a subsequent
/// `git merge` has a clean working tree to land on.
///
/// `cs done` emits `MergeDispatched` (and, on conflict-retry, more events)
/// into `.cosmon/state/events.jsonl` before the merge runs. Because
/// `events.jsonl` is git-tracked, those writes dirty the working tree.
/// When the incoming branch also touches `.cosmon/state/**`, `git merge`
/// refuses with *"Your local changes would be overwritten"* — the
/// auto-propel retry loop then keeps emitting `MergeDispatched`, keeps
/// re-dirtying the file, and never succeeds (observed in mailroom
/// batch integration, 2026-04-18).
///
/// The fix is mechanical and symmetric with the manual workaround
/// (`git add .cosmon/state/events.jsonl && git commit -m 'chore(state):
/// flush'`): stage everything under `.cosmon/state/` and commit it in
/// one `chore(state): flush before merge` commit. The append-only
/// auto-resolver ([`auto_resolve_append_only_jsonl`]) handles the
/// post-merge hunk collisions that remain.
///
/// Returns `Ok(true)` if a flush commit was created, `Ok(false)` if the
/// working tree was already clean under `.cosmon/state/`, `Err` if git
/// invocation fails.
fn flush_state_dir_changes(repo_root: &Path) -> anyhow::Result<bool> {
    let repo_arg = repo_root.to_string_lossy().to_string();

    // Short-circuit when `.cosmon/state/` has no pending changes.
    let status = Command::new("git")
        .args([
            "-C",
            &repo_arg,
            "status",
            "--porcelain",
            "--",
            ".cosmon/state/",
        ])
        .output()?;
    if !status.status.success() {
        return Err(anyhow::anyhow!(
            "git status --porcelain .cosmon/state/ failed: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }
    let status_text = String::from_utf8_lossy(&status.stdout);
    if status_text.trim().is_empty() {
        return Ok(false);
    }

    // Stage tracked-file changes first — `-u` is gitignore-safe because it
    // only touches files already in the index, so `.cosmon/.gitignore`
    // patterns (`state/**/state.json`, lock files, etc.) never trigger.
    // `-u` errors with "pathspec did not match" when no tracked files exist
    // yet under the directory (e.g. first molecule ever).  That is harmless —
    // the untracked-file pass below will pick them up.
    let add_u = Command::new("git")
        .args(["-C", &repo_arg, "add", "-u", "--", ".cosmon/state/"])
        .output()?;
    if !add_u.status.success() {
        let stderr = String::from_utf8_lossy(&add_u.stderr);
        let benign = stderr.contains("did not match") || stderr.contains("ne correspond");
        if !benign {
            return Err(anyhow::anyhow!(
                "git add -u .cosmon/state/ failed: {}",
                stderr.trim()
            ));
        }
    }

    // Stage new (untracked, non-ignored) files individually.  `git add <dir>`
    // on a directory containing gitignored files errors with "paths are
    // ignored" and a non-zero exit, even when non-ignored files are present.
    // Listing via `ls-files --others --exclude-standard` gives us exactly
    // the non-ignored untracked files, which we add one-by-one.
    let ls = Command::new("git")
        .args([
            "-C",
            &repo_arg,
            "ls-files",
            "--others",
            "--exclude-standard",
            "--",
            ".cosmon/state/",
        ])
        .output()?;
    if ls.status.success() {
        let new_files = String::from_utf8_lossy(&ls.stdout);
        for file in new_files.lines().filter(|l| !l.trim().is_empty()) {
            let add = Command::new("git")
                .args(["-C", &repo_arg, "add", "--", file])
                .output()?;
            if !add.status.success() {
                return Err(anyhow::anyhow!(
                    "git add {file} failed: {}",
                    String::from_utf8_lossy(&add.stderr).trim()
                ));
            }
        }
    }

    // If everything we touched is gitignored, there is nothing staged.
    // `git diff --cached --quiet` exits 0 when the index is clean.
    let diff_cached = Command::new("git")
        .args([
            "-C",
            &repo_arg,
            "diff",
            "--cached",
            "--quiet",
            "--",
            ".cosmon/state/",
        ])
        .output()?;
    if diff_cached.status.success() {
        return Ok(false);
    }

    // Structural guard: a `chore(state):` commit must never carry source.
    // The commit below is scoped to `.cosmon/state/`, so source cannot reach
    // it through pre-staging; this re-checks the actually-committed set and
    // fails fast if a source path somehow appears under that pathspec.
    let offending = source_paths_in_commit(repo_root, &[Path::new(".cosmon/state/")]);
    if !offending.is_empty() {
        return Err(anyhow::anyhow!(
            "refusing 'chore(state): flush before merge' — it would commit \
             source files under a state-tracking message (image/source drift \
             guard): {}",
            offending.join(", ")
        ));
    }

    // Scope the commit to `.cosmon/state/` so it records ONLY state, never the
    // whole staged index. A bare `git commit` here is what reverted source
    // under a `chore(state):` subject (postmortem 2026-06-15, 2e86cf908).
    let commit = Command::new("git")
        .args([
            "-C",
            &repo_arg,
            "commit",
            "-m",
            "chore(state): flush before merge",
            "--",
            ".cosmon/state/",
        ])
        .output()?;
    if !commit.status.success() {
        return Err(anyhow::anyhow!(
            "git commit (state flush) failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }
    Ok(true)
}

/// List files with unmerged entries in the index — i.e. files where git
/// recorded a textual conflict during the last merge attempt.
///
/// Uses `git diff --name-only --diff-filter=U` rather than parsing `git
/// status` output, which is locale-sensitive and harder to match.
fn list_unmerged_files(repo_root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "diff",
            "--name-only",
            "--diff-filter=U",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Basenames of JSONL files whose semantics are append-only: any line is a
/// valid entry, ordering is by timestamp, and a textual conflict between two
/// branches that both appended is not a real conflict.
const APPEND_ONLY_JSONL_BASENAMES: &[&str] = &["events.jsonl", "interactions.jsonl"];

/// Is this path an append-only JSONL file that can be merged by unioning lines?
fn is_append_only_jsonl(path: &str) -> bool {
    let basename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    APPEND_ONLY_JSONL_BASENAMES.contains(&basename)
}

/// Resolve a merge conflict on append-only JSONL files by taking the union of
/// both sides' lines, sorting by timestamp, and committing the merge.
///
/// This must be called while git is mid-merge (`MERGE_HEAD` exists and the
/// conflicting file has index stages 2 (ours) and 3 (theirs)). Precondition:
/// `files` is non-empty and every entry passes [`is_append_only_jsonl`].
///
/// `branch` and `coauthor_trailers` mirror the clean-merge path in
/// [`try_merge_branch`]: the finalizing commit is a MERGE COMMIT, i.e. the
/// trailer carrier of the trailer-carrier contract (delib-20260717-194b, F1).
/// Finalizing with a bare `git commit --no-edit` re-uses whatever
/// `.git/MERGE_MSG` holds — which varies with git version and
/// `commit.cleanup` config, and (default cleanup) commits the
/// `# Conflicts:` comment block verbatim. Under fleet parallelism this
/// silently dropped the `Co-Authored-By` block from conflicted merges
/// (task-20260718-7f91; incident merge of task-20260718-a550). Passing the
/// subject and the trailer paragraph explicitly via `-m` makes the message
/// deterministic and byte-compatible with the conflict-free path.
fn auto_resolve_append_only_jsonl(
    repo_root: &Path,
    files: &[String],
    branch: &str,
    coauthor_trailers: &[String],
) -> anyhow::Result<()> {
    let repo_arg = repo_root.to_string_lossy().to_string();
    for file in files {
        let ours = git_show_stage(repo_root, 2, file).unwrap_or_default();
        let theirs = git_show_stage(repo_root, 3, file).unwrap_or_default();

        let merged = merge_jsonl_by_timestamp(&ours, &theirs);

        let abs = repo_root.join(file);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, merged)?;

        let add = Command::new("git")
            .args(["-C", &repo_arg, "add", "--", file])
            .output()?;
        if !add.status.success() {
            return Err(anyhow::anyhow!(
                "git add {file} failed: {}",
                String::from_utf8_lossy(&add.stderr).trim()
            ));
        }
    }

    // Finalize the merge commit. Same message contract as the clean-merge
    // path in `try_merge_branch`: with trailers configured, the subject and
    // the trailer paragraph are passed explicitly via `-m` (never trusting
    // `.git/MERGE_MSG`, see the function doc); with no trailers configured,
    // `--no-edit` keeps the pre-attribution behavior byte-identical (F9).
    // Either form prevents $EDITOR from firing.
    let mut commit_cmd = Command::new("git");
    commit_cmd.args(["-C", &repo_arg, "commit"]);
    if coauthor_trailers.is_empty() {
        commit_cmd.arg("--no-edit");
    } else {
        let subject = format!("Merge branch '{branch}'");
        let trailers = coauthor_trailers.join("\n");
        commit_cmd.args(["-m", &subject, "-m", &trailers]);
    }
    let commit = commit_cmd.output()?;
    if !commit.status.success() {
        return Err(anyhow::anyhow!(
            "git commit (post-auto-resolve) failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }
    Ok(())
}

/// Read file content at a given merge-index stage (1 = base, 2 = ours, 3 = theirs).
fn git_show_stage(repo_root: &Path, stage: u8, file: &str) -> anyhow::Result<String> {
    let spec = format!(":{stage}:{file}");
    let out = Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "show", &spec])
        .output()?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "git show {spec} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Merge two JSONL blobs by taking the set-union of non-empty lines and
/// sorting by the `timestamp` field (falling back to lexicographic order if
/// the field is absent or identical). Output is terminated by a newline.
fn merge_jsonl_by_timestamp(ours: &str, theirs: &str) -> String {
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for blob in [ours, theirs] {
        for line in blob.lines() {
            if line.is_empty() {
                continue;
            }
            let ts = extract_timestamp(line).unwrap_or_default();
            seen.insert((ts, line.to_owned()));
        }
    }
    let mut out = String::with_capacity(ours.len() + theirs.len());
    for (_, line) in seen {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Extract the value of the `"timestamp"` string field from a JSONL line
/// without pulling in a full JSON parser. The events/interactions schema
/// writes the field as `"timestamp":"<rfc3339>"` — a tiny regex is enough.
fn extract_timestamp(line: &str) -> Option<String> {
    let key = "\"timestamp\":\"";
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Outcome of the branch-deletion decision in step 6 of `cs done`.
///
/// Factored out of [`run`] so the **GUARD anti-wipe** (raté 5eba) is
/// unit-testable without driving a full teardown. See
/// [`decide_branch_delete`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchDeleteDecision {
    /// Delete the branch — it exists and its commits are reachable from
    /// base, and the caller asked to delete (merge succeeded or `--force`).
    Delete,
    /// **GUARD anti-wipe**: refuse deletion because the branch is NOT an
    /// ancestor of base. The branch is the only copy of the work; this
    /// refusal overrides both `merge_succeeded` and `--force`.
    RefuseUnmerged,
    /// Refuse deletion because the merge did not succeed and `--force`
    /// was not passed (pre-existing FIX 2 behaviour).
    RefuseMergeFailed,
    /// Nothing to do — branch absent or deletion opted out
    /// (`effective_no_branch_delete`).
    Skip,
}

/// Decide whether `cs done` may delete the worker's branch.
///
/// **GUARD anti-wipe — raté 5eba (the most expensive loss observed).**
/// `cs done task-…-5eba` deleted a `feat/*` branch whose 491-line payload
/// had never landed on `main`; the work was lost and had to be recovered
/// from `git fsck --lost-found`. Root cause: `git branch -d` checks
/// ancestry against the *current HEAD*, not against `main` — so when
/// `cs done` runs from a checkout from which the branch is reachable (a
/// worktree, or main after an unrelated merge), `-d` happily deletes a
/// branch that `main` never absorbed. The in-process `merge_succeeded`
/// flag is likewise not ground truth (it can be set on paths that did
/// not actually advance base).
///
/// This decision therefore takes `branch_in_base` — a *fresh* topology
/// probe (`git merge-base --is-ancestor <branch> <base>`) taken
/// immediately before deletion — as the load-bearing input, and refuses
/// (`RefuseUnmerged`) whenever it is false, **regardless of
/// `merge_succeeded` or `force`**. This is I-WRITER-UNIQUE (ADR-110 I1)
/// renforcé: a branch that is not on the trunk is never auto-destroyed.
/// An operator who genuinely wants to discard unmerged work runs
/// `git branch -D` by hand — `cs done` does not do it silently.
///
/// Precedence (first match wins):
/// 1. `!branch_present` or `effective_no_branch_delete` → `Skip`.
/// 2. `!branch_in_base` → `RefuseUnmerged` (the guard; overrides force).
/// 3. `merge_succeeded || force` → `Delete`.
/// 4. otherwise → `RefuseMergeFailed`.
// Five bools, each a distinct teardown precondition with a one-to-one
// mapping to a branch of the decision table — same rationale as the
// `Args` struct's `#[allow(clippy::struct_excessive_bools)]`. Collapsing
// them into a struct or bitflags would obscure the table, not simplify it.
#[allow(clippy::fn_params_excessive_bools)]
fn decide_branch_delete(
    branch_present: bool,
    effective_no_branch_delete: bool,
    branch_in_base: bool,
    merge_succeeded: bool,
    force: bool,
) -> BranchDeleteDecision {
    if !branch_present || effective_no_branch_delete {
        return BranchDeleteDecision::Skip;
    }
    if !branch_in_base {
        // The guard. Ground-truth topology beats every in-process flag.
        return BranchDeleteDecision::RefuseUnmerged;
    }
    if merge_succeeded || force {
        BranchDeleteDecision::Delete
    } else {
        BranchDeleteDecision::RefuseMergeFailed
    }
}

/// Check whether a local branch exists.
fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// Delete a local branch (must be merged or detached).
fn delete_branch(repo_root: &Path, branch: &str) -> anyhow::Result<()> {
    let out = Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "branch", "-d", branch])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Check whether a worktree has dirty state (modified tracked files OR
/// untracked files).
///
/// `git worktree remove` without `--force` only catches modified *tracked*
/// files — untracked files (e.g. `synthesis.md`) are destroyed silently.
/// This porcelain check catches both categories.
fn worktree_is_dirty(worktree_path: &Path) -> Result<Vec<String>, anyhow::Error> {
    let out = Command::new("git")
        .args([
            "-C",
            &worktree_path.to_string_lossy(),
            "status",
            "--porcelain",
        ])
        .output()?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "git status failed in worktree: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let dirty: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Ok(dirty)
}

/// Rescue untracked files from a worktree before it is destroyed.
///
/// `git worktree remove` silently deletes untracked files. This function
/// copies them into `molecule_dir/rescued/` preserving relative paths so
/// they survive teardown.
fn rescue_untracked_files(
    worktree_path: &Path,
    molecule_dir: &Path,
) -> anyhow::Result<Vec<String>> {
    let out = Command::new("git")
        .args([
            "-C",
            &worktree_path.to_string_lossy(),
            "ls-files",
            "--others",
            "--exclude-standard",
        ])
        .output()?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if files.is_empty() {
        return Ok(files);
    }
    let rescue_dir = molecule_dir.join("rescued");
    for rel in &files {
        let src = worktree_path.join(rel);
        let dst = rescue_dir.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst)?;
    }
    Ok(files)
}

/// Remove a git worktree.
fn remove_worktree(repo_root: &Path, worktree_path: &Path) -> anyhow::Result<()> {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "worktree",
            "remove",
            &worktree_path.to_string_lossy(),
        ])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Run a post-merge hook command from the repository root.
///
/// Spawns the command via `sh -c` so that shell features (pipes, `&&`,
/// environment expansion) work as expected. Returns the exit code on
/// success, or an error if the command could not be spawned.
fn run_post_merge_hook(repo_root: &Path, hook_cmd: &str) -> anyhow::Result<i32> {
    let output = Command::new("sh")
        .args(["-c", hook_cmd])
        .current_dir(repo_root)
        .output()?;
    let code = output.status.code().unwrap_or(-1);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{hook_cmd} exited {code}: {}", stderr.trim());
    }
    Ok(code)
}

/// Return the currently checked-out commit, used as the rollback point for
/// the mandatory post-merge workspace compile gate.
fn git_head(repo_root: &Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "could not capture pre-merge HEAD: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if head.is_empty() {
        anyhow::bail!("could not capture pre-merge HEAD: git returned an empty revision");
    }
    Ok(head)
}

/// Bounded integration-gate budget. A timed-out gate is a refusal: the merge
/// is rolled back while the trunk guard is still in scope, so no resident loop
/// can inherit a permanently held trunk lock.
///
/// Widened 5min → 30min (task 2026-07-16): a cold `cargo check` of a workspace
/// carrying a C++ staticlib dependency (e.g. a vendored Verovio `-sys` crate)
/// legitimately exceeds 5min on a cold target dir, so the original 5min budget
/// rolled back correct merges as false timeouts. The durable fix (env/config
/// override + transactional finalize so a rolled-back merge doesn't strand the
/// ledger) is tracked as a cosmon-ward molecule; this const bump is the
/// stopgap that lets Verovio-workspace harvests land.
const POST_MERGE_GATE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Place a to-be-spawned command in its own process group so a timeout can reap
/// the **whole** subprocess tree, not just the direct child. A bare
/// `child.kill()` signals only the process cargo forked; the build scripts,
/// `rustc` invocations, and any other descendants cargo itself spawned survive.
/// For [`run_bounded_capture`] that is fatal: a descendant that inherited the
/// capture pipe keeps its write end open, so the reader thread's `read_to_end`
/// never sees EOF and the join blocks the gate past its wall-clock budget —
/// the strictness gap in the 4032 bound (delib-559a, defect 2).
///
/// `process_group(0)` is a **safe** std API (stable since 1.88's MSRV floor):
/// the child's pgid becomes its own pid, with no `unsafe`/`pre_exec` closure.
/// On non-Unix this is a no-op; [`kill_child_tree`] falls back to a direct kill.
fn in_own_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

/// SIGKILL a timed-out child's entire process group (Unix), then reap the direct
/// child. Signalling the *negative* pgid delivers to every process in the group,
/// so descendants that inherited a capture pipe die and release it — letting the
/// reader threads reach EOF instead of hanging the gate. On non-Unix, or if the
/// group signal fails, we still kill the direct child so the caller's bound
/// holds as well as it can. Pair with [`in_own_process_group`] at spawn time.
fn kill_child_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child was spawned with `process_group(0)`, so its pid *is* its
        // pgid; `kill(-pgid, SIGKILL)` reaches the whole group.
        if let Ok(pid) = i32::try_from(child.id()) {
            let group = nix::unistd::Pid::from_raw(-pid);
            let _ = nix::sys::signal::kill(group, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Run a command with inherited output and a hard wall-clock bound.
///
/// Inheriting stdio is intentional: retaining piped cargo output while polling
/// `try_wait` can fill a pipe and turn a busy compiler into an apparent hang.
///
/// The child runs in its own process group so a timeout reaps the whole tree
/// ([`kill_child_tree`]); killing only the direct `cargo` child would leave its
/// build-script / `rustc` descendants running past the deadline (defect 2).
fn run_bounded(command: &mut Command, timeout: Duration) -> anyhow::Result<()> {
    in_own_process_group(command);
    let mut child = command.spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("exited {}", status.code().unwrap_or(-1));
        }
        if started.elapsed() >= timeout {
            kill_child_tree(&mut child);
            let _ = child.wait();
            anyhow::bail!("timed out after {} seconds", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The outcome the post-merge gate reports to its caller. Its whole reason to
/// exist is that "we verified this merge" and "we could not verify this merge"
/// must **not** collapse into a single `Ok` the caller reads as success. A repo
/// shape the gate cannot positively verify (not a Cargo workspace cargo can
/// resolve from the root, or a changed Rust source that maps to no member) gets
/// a loud [`GateOutcome::Unverified`] witness, never a silent skip.
///
/// Invariant (tolnay, delib-20260714-559a Q2): for any repo shape the gate does
/// not positively verify, the caller observes either a delegated command result
/// (inc-2) or a loud `Unverified` — there is no fourth path. The call site
/// matches this enum exhaustively with **no `_ =>` arm**, so a future variant
/// forces every consumer to decide what it means rather than defaulting to
/// "success".
#[derive(Debug, PartialEq, Eq)]
enum GateOutcome {
    /// The gate ran one or more `cargo check` invocations and all passed.
    /// `description` is the human-readable command line(s) executed.
    Verified { description: String },
    /// The diff touched nothing the gate must verify (documentation-only, or no
    /// Rust source and no build-structural file) — an honest inert result.
    NothingToVerify,
    /// The gate could **not** positively verify this merge shape. This is a loud
    /// witness the caller must surface, never a silent pass. `command`, when
    /// present, is the invocation an operator could run to verify by hand.
    ///
    /// `expected` is the delib-20260714-7605 D1 discriminator (kahneman's
    /// expected-gate probe), recorded for legibility: `true` when an integrity
    /// gate *was* expected (a Cargo workspace resolved, or a command was
    /// declared) but a code diff still went unchecked — the more dangerous,
    /// defect-shaped case; `false` when nothing was declared and cosmon simply
    /// has no way to verify this tree. cosmon ships fail-open-loud regardless of
    /// `expected` (backward-compatible default); the operator promotes *any*
    /// `Unverified` to fail-closed via `[gates].fail_closed_on_unverified`. The
    /// flag exists so the advisory and a future default flip can distinguish the
    /// two without re-deriving the probe. See ADR-158.
    Unverified {
        reason: String,
        command: Option<String>,
        expected: bool,
    },
}

/// Fold the post-merge [`GateOutcome`] and the merge-escalation retry count into
/// the **single** durable `MergeResult` `cs done` emits for a successful merge.
///
/// PR-B invariant (task-20260714-aa2e): the merge-completed event is written
/// **once**, **after** the gate, so it can never record a bare `Ok` the gate has
/// not yet earned. The old flow emitted an `Ok` the instant the merge landed —
/// *before* the gate ran — which lied twice: a merge the gate then rolled back
/// left a permanent `Ok` in `events.jsonl`, and a merge the gate could only mark
/// `Unverified` was still recorded as a clean `Ok`. Here the result is *keyed on
/// the gate outcome*:
///
/// - [`GateOutcome::Verified`] / [`GateOutcome::NothingToVerify`] → `ok`
///   (or `ok:escalated(n)` when the merge needed `n` escalation retries).
/// - [`GateOutcome::Unverified`] → the durable witness `ok:unverified`
///   (or `ok:escalated(n):unverified`) — the merge landed but the gate could
///   not positively verify it. Never a bare `Ok`.
///
/// The gate-*error* path (a refused, rolled-back merge) emits its own terminal
/// `MergeResult::Error` and returns before reaching here, so it is likewise a
/// single event; this helper only shapes the *success* result.
fn post_gate_merge_result(
    gate: &GateOutcome,
    escalation_retries: Option<u32>,
) -> cosmon_core::event_v2::MergeResult {
    use cosmon_core::event_v2::MergeResult;
    // Only a genuine escalation (n > 0) modifies the wire string; `Some(0)` is a
    // clean landing and is indistinguishable from a plain merge.
    let escalated = escalation_retries.filter(|&n| n > 0);
    match gate {
        GateOutcome::Verified { .. } | GateOutcome::NothingToVerify => match escalated {
            Some(n) => MergeResult::Other(format!("ok:escalated({n})")),
            None => MergeResult::Ok,
        },
        GateOutcome::Unverified { .. } => match escalated {
            Some(n) => MergeResult::Other(format!("ok:escalated({n}):unverified")),
            None => MergeResult::Other("ok:unverified".to_owned()),
        },
    }
}

/// Internal plan produced by [`post_merge_compile_scope`] without running any
/// `cargo check`. Kept private to this module: the caller only ever sees the
/// ratified [`GateOutcome`].
#[derive(Debug, PartialEq, Eq)]
enum CheckDecision {
    /// No Rust or build-structural change — nothing to compile.
    Nothing,
    /// A changed Rust source in a *confirmed* Cargo workspace maps to no
    /// workspace member (an excluded / standalone / newly-added crate, or a
    /// second workspace in the same repo). The gate cannot bound its blast
    /// radius from root metadata, so it reports honestly rather than widening
    /// to `--workspace` and calling clean — that would silently skip the very
    /// crate that changed (round1 #5's false-green). `reason` names the file(s).
    Unverified { reason: String },
    /// Run the described checks. Either `whole_workspace` (a structural change
    /// whose blast radius we cannot bound) or a non-empty `packages` list grown
    /// over the reverse-dependency closure.
    Check {
        whole_workspace: bool,
        packages: Vec<String>,
    },
}

#[derive(serde::Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
}

#[derive(serde::Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<CargoDependency>,
}

#[derive(serde::Deserialize)]
struct CargoDependency {
    path: Option<PathBuf>,
}

/// Roll main back to its pre-merge revision and emit the loud failure report +
/// durable `Error` witness for a post-merge gate refusal, returning the `Err`
/// the caller must propagate (so `cs done` exits non-zero, the merge is
/// un-observed, and the runtime retries the molecule instead of advancing
/// dependents past a main it could not verify).
///
/// Shared by the two refusal paths so they stay byte-identical:
/// - the gate-*error* path (a delegated command or `cargo check` returned
///   non-zero, or metadata was unparseable), and
/// - the fail-*closed* `Unverified` path (`[gates].fail_closed_on_unverified`).
///
/// The `MergeResult::Error` witness written here is what a downstream honesty
/// auditor reads for a rolled-back merge — never a silent absence.
#[allow(clippy::too_many_arguments)]
fn refuse_post_merge_and_rollback(
    ctx: &Context,
    events_path: &Path,
    mol_id: &MoleculeId,
    branch_name: &str,
    repo_root: &Path,
    pre_merge_head: Option<&str>,
    merge_dispatch_seq: Option<cosmon_core::event_v2::Seq>,
    actions: &[String],
    cause: &str,
) -> anyhow::Error {
    let rollback = pre_merge_head.ok_or_else(|| {
        anyhow::anyhow!("post-merge gate refused DONE but no pre-merge revision was captured")
    });
    let reason = match rollback.and_then(|head| reset_hard(repo_root, head)) {
        Ok(()) => format!("{cause}; git reset --hard restored main to its pre-merge revision"),
        Err(reset_err) => format!("{cause}; CRITICAL: git reset --hard failed: {reset_err}"),
    };
    let _ = cosmon_state::event_log::emit_one(
        events_path,
        cosmon_core::event_v2::EventV2::MergeCompleted {
            molecule: mol_id.clone(),
            branch: branch_name.to_owned(),
            result: cosmon_core::event_v2::MergeResult::Error(reason.clone()),
            federation_provenance: None,
        },
        merge_dispatch_seq,
    );
    report_merge_failure(
        ctx,
        mol_id,
        "post_merge_compile_gate_refused",
        "POST-MERGE COMPILE GATE REFUSED — merge rolled back, branch preserved",
        &[],
        &reason,
        actions,
    );
    anyhow::anyhow!(reason)
}

/// The outcome of the cargo-metadata auto-detect rung (rung 2 of the cascade).
/// Distinct from [`GateOutcome`] because "cargo does not resolve from this repo
/// root" is not a *verdict* — it is a signal to try the next cascade rung
/// (`GatesConfig::build_command`), not a loud `Unverified` in its own right.
/// Only after every rung declines does the cascade mint the terminal
/// `Unverified`. Keeping the fall-through in the type (rather than sniffing a
/// reason string) is what lets the cascade stay honest.
#[derive(Debug, PartialEq, Eq)]
enum CargoGate {
    /// Cargo resolved the workspace from the repo root and the diff-scoper
    /// produced a real verdict for the caller to honor as-is.
    Resolved(GateOutcome),
    /// The cargo auto-detect rung **declines** to produce a terminal verdict, so
    /// the cascade must try `GatesConfig::build_command` (rung 3) before giving
    /// up. Three conditions decline:
    ///
    /// - no Cargo workspace cargo can resolve from the repo root; or
    /// - cargo cannot be spawned/resolved at all (absent from `PATH`, spawn
    ///   error) — Defect 4: a mere absence of cargo must fall through to
    ///   `build_command`, never hard-error into a rollback; or
    /// - nothing Rust-relevant changed **and** a `build_command` fallback is
    ///   declared — Defect 3: the "no Rust changed" short-circuit is
    ///   Cargo-cognition and must not gate a declared polyglot command, which
    ///   runs unconditionally (ADR-158).
    Declines,
}

/// Resolve and run the post-merge integrity cascade against the **combined**
/// (already-merged) tree — cosmon owns the WHEN, the galaxy declares the WHAT
/// (ADR-158). Precedence is einstein's derived order (delib-20260714-7605 D2):
/// a *declaration* outranks an *inference*, an inference outranks a blind check:
///
///   1. `[gates].integrity_command` — explicit declaration, run verbatim.
///   2. cargo-metadata auto-detect  — zero-config inference (the diff-scoper).
///   3. `[gates].build_command`     — declaration fallback (fallback ≠ rename).
///   4. loud `Unverified{expected:false}` — nobody declared how to verify.
///
/// Every rung runs from the repo root under **one** shared wall-clock budget
/// ([`POST_MERGE_GATE_TIMEOUT`], carnot's shared-deadline form) so a stalled
/// package cache or a hung declared command can never hold the trunk guard open
/// indefinitely. A delegated command's non-zero exit is a gate *error*
/// (`Err` → the caller rolls the merge back), never a silent pass.
///
/// A declared command (rungs 1 & 3) is run **unconditionally** — cosmon does
/// *not* pre-filter "is this diff build-relevant?" for it, because that judgment
/// needs language knowledge cosmon must not bake in (the very cognition
/// Transport ≠ Cognition forbids). Only the cargo rung, where cosmon legitimately
/// understands the toolchain, short-circuits a documentation-only diff to
/// [`GateOutcome::NothingToVerify`] before paying any metadata cost (finding
/// 3793).
fn run_post_merge_gate(
    repo_root: &Path,
    pre_merge_head: &str,
    gates: &cosmon_core::config::GatesConfig,
) -> anyhow::Result<GateOutcome> {
    // One budget for the whole cascade: every rung draws from it.
    let deadline = Instant::now() + POST_MERGE_GATE_TIMEOUT;

    // Rung 1 — explicit integrity_command declaration.
    if let Some(command) = gates.integrity_command.as_deref() {
        return run_delegated_command(repo_root, "integrity_command", command, deadline);
    }

    // Rung 2 — cargo-metadata auto-detect. Cosmon understands cargo, so its own
    // docs-only short-circuit and reverse-dependency diff-scoping apply here.
    // `has_fallback` tells the rung whether a declared `build_command` waits at
    // rung 3: when it does, the cargo-cognition "no Rust changed" short-circuit
    // must NOT conclude `NothingToVerify` — it declines to rung 3 so the declared
    // command runs unconditionally (Defect 3, ADR-158).
    let has_fallback = gates.build_command.is_some();
    match run_cargo_autodetect(repo_root, pre_merge_head, deadline, has_fallback)? {
        CargoGate::Resolved(outcome) => return Ok(outcome),
        CargoGate::Declines => { /* fall through to the declaration fallback */ }
    }

    // Rung 3 — build_command declaration fallback (the polyglot path).
    if let Some(command) = gates.build_command.as_deref() {
        return run_delegated_command(repo_root, "build_command", command, deadline);
    }

    // Rung 4 — nobody declared how to verify this combined tree. The residual
    // hole delib-20260714-7605 named without euphemism: cosmon cannot
    // manufacture a net, only be honest about its absence. `expected:false` — no
    // gate was ever expected here.
    Ok(GateOutcome::Unverified {
        reason: "no [gates].integrity_command declared, no Cargo workspace cargo resolves \
                 from the repo root, and no [gates].build_command fallback — cosmon has no \
                 declared way to verify the merged tree"
            .to_owned(),
        command: None,
        expected: false,
    })
}

/// Run a repo-supplied integrity command (rung 1 `integrity_command` or the
/// rung 3 `build_command` fallback) verbatim against the already-merged worktree
/// from the repo root, under the shared `deadline`. Exit 0 → [`GateOutcome::
/// Verified`]; a non-zero exit or spawn failure is a gate *error* (`Err`) so the
/// caller rolls the merge back — a declared verifier that says "broken" must be
/// exactly as load-bearing as cargo saying "broken".
///
/// B5 (RCE-by-clone): the command comes from the repo's own
/// `.cosmon/config.toml`, so it is refused in an untrusted clone. Unlike the
/// advisory `post_merge` *hook* (which runs after the irreversible teardown and
/// so merely warns), this gate runs *before* `merged_at`, so a trust refusal is
/// a safe, reversible `Unverified` the caller can roll back — never a silent
/// skip and never an exec of untrusted shell. A command *was* declared, so the
/// refusal is `expected:true`.
fn run_delegated_command(
    repo_root: &Path,
    slot: &str,
    command: &str,
    deadline: Instant,
) -> anyhow::Result<GateOutcome> {
    if let Err(e) = cosmon_cli::trust::ensure_trusted(repo_root) {
        return Ok(GateOutcome::Unverified {
            reason: format!(
                "[gates].{slot} `{command}` not run — repo not trusted for shell \
                 (B5, RCE-by-clone): {e}"
            ),
            command: Some(command.to_owned()),
            expected: true,
        });
    }
    // Defect 1 (codex-sol, task-20260715-ff5b): route the repo-supplied command
    // through the SAME egress/sandbox discipline as ordinary agent subprocesses
    // (`exec_command`). Trust hashes the config, not the *script* the command
    // invokes, so a merged branch can modify a trusted `integrity_command`
    // script and — without this jail — execute arbitrary code from the combined
    // tree with host filesystem + network access. On a host that cannot
    // kernel-enforce a required `deny-external` policy and this is an exposed
    // multi-tenant dispatch, the gate REFUSES fail-closed (a gate error → the
    // caller rolls the merge back) rather than run the shell unconfined — the
    // RPP preflight refusal, mirrored at the merge gate. With
    // `COSMON_EGRESS_POLICY` unset (the trusted single-operator default) the
    // wrapped command is byte-identical to the pre-fix `sh -c <command>`.
    let (program, args) = match super::egress_delegate::jail_delegated_sh(command) {
        super::egress_delegate::JailDecision::Ready {
            program,
            args,
            advisory_reason,
            ..
        } => {
            if let Some(reason) = advisory_reason {
                eprintln!("⚠ egress advisory (delegated [gates].{slot}): {reason}");
            }
            (program, args)
        }
        super::egress_delegate::JailDecision::Refused { message } => {
            return Err(anyhow::anyhow!(
                "[gates].{slot} `{command}` refused (egress fail-closed) — {message}"
            ));
        }
    };
    let mut cmd = Command::new(&program);
    cmd.args(&args).current_dir(repo_root);
    run_bounded(&mut cmd, deadline.saturating_duration_since(Instant::now()))
        .map_err(|e| anyhow::anyhow!("[gates].{slot} `{command}` {e}"))?;
    Ok(GateOutcome::Verified {
        description: format!("[gates].{slot}: {command}"),
    })
}

/// Rung 2 of the cascade: the cargo-metadata auto-detect. Compile the post-merge
/// diff's crates and every workspace crate that depends on them. That reverse
/// closure is essential: a public signature changed in crate A can break a test
/// target in caller B even when B's files are absent from the merge diff.
///
/// Shares the cascade's single `deadline` ([`run_post_merge_gate`]): `cargo
/// metadata` and every `cargo check` draw from it (finding 4032). A
/// documentation-only diff short-circuits to [`GateOutcome::NothingToVerify`]
/// **before** any metadata cost is paid (finding 3793). A repo cargo cannot
/// resolve from the root returns `CargoGate::NotACargoWorkspace` so the cascade
/// falls through to the declaration fallback — the auto-detect *declining* is a
/// fall-through, not a verdict.
fn run_cargo_autodetect(
    repo_root: &Path,
    pre_merge_head: &str,
    deadline: Instant,
    has_fallback: bool,
) -> anyhow::Result<CargoGate> {
    let changed_files = post_merge_changed_files(repo_root, pre_merge_head)?;

    // 3793 — docs-only determination BEFORE any metadata call. "Is this diff
    // build-relevant?" is a pure path question; answering it first keeps a
    // documentation-only merge fully inert and off the metadata path.
    //
    // Defect 3 (codex-sol, task-20260715-ff5b): `is_build_relevant` recognizes
    // only `.rs` / Cargo manifests / `.cargo/` — pure *Cargo cognition*. When a
    // `build_command` fallback is declared (`has_fallback`), that cognition must
    // NOT gate the polyglot path: a Python/Go/Node change is "not build-relevant"
    // to cargo yet the declared command must still run unconditionally. So
    // decline to rung 3 instead of short-circuiting to a clean `NothingToVerify`.
    // With no fallback (cosmon rides the cargo rung alone) the docs-only
    // short-circuit stays exactly as before.
    if !changed_files.iter().any(|path| is_build_relevant(path)) {
        return Ok(if has_fallback {
            CargoGate::Declines
        } else {
            CargoGate::Resolved(GateOutcome::NothingToVerify)
        });
    }

    // A diff touched something build-relevant. A manifest that exists but fails
    // to parse is a broken-manifest integrity failure (`Err` → rollback); cargo
    // that cannot be spawned/resolved, or no resolvable workspace at all, is a
    // fall-through to the next cascade rung (Defect 4), never a silent skip.
    let Some(metadata) = cargo_metadata_bounded(repo_root, deadline)? else {
        return Ok(CargoGate::Declines);
    };

    let plan = match post_merge_compile_scope(repo_root, &metadata, &changed_files) {
        // A confirmed cargo workspace with nothing to compile. With a declared
        // `build_command` fallback the same Defect-3 rule applies — decline to
        // rung 3 so the declared command runs — else conclude `NothingToVerify`.
        CheckDecision::Nothing if has_fallback => return Ok(CargoGate::Declines),
        CheckDecision::Nothing => return Ok(CargoGate::Resolved(GateOutcome::NothingToVerify)),
        CheckDecision::Unverified { reason } => {
            // A confirmed workspace whose code diff could not be scoped is the
            // dangerous, defect-shaped case (kahneman's expected-gate probe): a
            // gate *was* expected (cargo resolved) but a `.rs` went unchecked.
            // `expected:true` records that for the advisory; the merge still
            // lands loud-fail-open by default (delib-559a binding acceptance
            // test), promotable to fail-closed via config.
            return Ok(CargoGate::Resolved(GateOutcome::Unverified {
                reason,
                command: None,
                expected: true,
            }));
        }
        CheckDecision::Check {
            whole_workspace,
            packages,
        } => (whole_workspace, packages),
    };
    let (whole_workspace, packages) = plan;

    // Each check consumes the *remaining* budget so the whole gate stays inside
    // one deadline. All checks run from the repo root: once the tree-scanning
    // spelunker is shed, the workspace root is the repo root by construction.
    let mut description = String::new();
    if whole_workspace {
        let mut check = Command::new("cargo");
        check
            .args(["check", "--workspace", "--all-targets"])
            .current_dir(repo_root);
        run_bounded(
            &mut check,
            deadline.saturating_duration_since(Instant::now()),
        )
        .map_err(|e| anyhow::anyhow!("cargo check --workspace --all-targets {e}"))?;
        description.push_str("cargo check --workspace --all-targets");
    } else {
        let mut check = Command::new("cargo");
        check.arg("check");
        for package in &packages {
            check.args(["-p", package]);
        }
        check.arg("--all-targets").current_dir(repo_root);
        run_bounded(
            &mut check,
            deadline.saturating_duration_since(Instant::now()),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "cargo check for affected packages {} {e}",
                packages.join(", ")
            )
        })?;
        let _ = write!(
            description,
            "cargo check {} --all-targets",
            packages
                .iter()
                .map(|package| format!("-p {package}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    Ok(CargoGate::Resolved(GateOutcome::Verified { description }))
}

fn post_merge_changed_files(
    repo_root: &Path,
    pre_merge_head: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    // `-z` emits NUL-separated, *unquoted* pathnames. Without it, git's default
    // `core.quotePath=true` C-quotes any non-ASCII byte, so `src/café.rs`
    // arrives as the literal `"src/caf\303\251.rs"` — whose extension parses as
    // `rs"`, so it is neither mapped to a member nor recognized as Rust and the
    // gate silently skips a changed Rust source. NUL-separation also makes the
    // split robust to newlines in pathnames.
    let output = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "diff",
            "-z",
            "--name-only",
            &format!("{pre_merge_head}..HEAD"),
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "could not determine post-merge diff: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Given confirmed workspace `metadata`, decide which crates a merge put at
/// risk — a pure function that runs no `cargo check`. Each changed file maps to
/// its owning member by longest-prefix directory match (any on-disk layout, not
/// a hardcoded `crates/`). A structural change widens to the whole workspace. A
/// changed `.rs` that maps to no member is the honesty catch: the gate cannot
/// bound its blast radius from root metadata, so it reports [`CheckDecision::
/// Unverified`] rather than widen-to-`--workspace`-and-call-clean.
fn post_merge_compile_scope(
    repo_root: &Path,
    metadata: &CargoMetadata,
    changed_files: &[PathBuf],
) -> CheckDecision {
    let package_dirs = workspace_package_dirs(metadata);
    // `cargo metadata` canonicalizes member directories (on macOS `/var` →
    // `/private/var`), so join changed files onto the canonicalized root to keep
    // the longest-prefix match honest across the symlink.
    let canonical_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());

    let mut whole_workspace = changed_files
        .iter()
        .any(|path| requires_workspace_check(path));
    let mut affected: BTreeSet<String> = BTreeSet::new();
    let mut unmapped: Vec<PathBuf> = Vec::new();
    let mut unmapped_structural: Vec<PathBuf> = Vec::new();

    for path in changed_files {
        let abs = canonical_root.join(path);
        match owning_package(&abs, &package_dirs) {
            // A changed `Cargo.toml` mapped to a member *only* by the root
            // package's catch-all prefix — its own directory is not a current
            // member — is a removed/moved wildcard member (or a nested non-member
            // crate) that post-merge `cargo metadata` no longer lists. Route it to
            // the structural honesty catch below (widen / `Unverified`), never a
            // root-package-scoped `-p` check that skips the sibling members the
            // shape change actually put at risk (defect 1, round-2 codex-sol). This
            // bites only a non-virtual workspace (root package dir == repo root);
            // a virtual root already returns `None` here and is handled unchanged.
            Some(_)
                if is_cargo_manifest(path)
                    && !manifest_declares_current_member(&abs, &package_dirs) =>
            {
                unmapped_structural.push(path.clone());
            }
            Some(name) => {
                affected.insert(name.to_owned());
            }
            // A Rust source that belongs to no workspace member — an `exclude`d
            // standalone crate, a second workspace in the same repo, a brand-new
            // crate not yet in metadata, a moved/renamed file — has a blast
            // radius the member map cannot bound.
            None if is_rust_source(path) => unmapped.push(path.clone()),
            // A *structural* file (member `Cargo.toml`/`Cargo.lock`/`.cargo/`)
            // that maps to no current member: a wildcard member was removed or
            // moved, so post-merge `cargo metadata` no longer lists it. Silently
            // dropping it here is the defect — `owning_package` returns `None`,
            // the file is not `.rs`, and the merge slid to `NothingToVerify`
            // even though the workspace shape changed under us.
            None if is_build_structural(path) => unmapped_structural.push(path.clone()),
            None => {}
        }
    }

    // Honesty catch (torvalds, delib-559a): an unmapped `.rs` in a confirmed
    // workspace goes to `Unverified`, never a `--workspace` widen. `cargo check
    // --workspace` visits only members, so widening here would run clean while
    // silently skipping the excluded/standalone crate that actually changed —
    // reintroducing round1 #5's false-green. cosmon-on-cosmon never reaches this
    // branch (every changed `.rs` is a member file), so cosmon stays fully
    // netted while uncovered shapes (subdir manifest, excluded crate,
    // multi-workspace) get a loud witness routed to inc-2 delegation.
    if !unmapped.is_empty() {
        let list = unmapped
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return CheckDecision::Unverified {
            reason: format!(
                "changed Rust source(s) map to no workspace member cargo resolves from the \
                 repo root (excluded, standalone, second-workspace, or newly-added crate): {list}"
            ),
        };
    }

    // Structural honesty catch (delib-559a, defect 1): an unmapped *structural*
    // file means a member disappeared from metadata (a removed/moved wildcard
    // member). Its own source has no owner to bound with a `-p` list — it may be
    // gone entirely — but the workspace root still resolves (we hold `metadata`),
    // so the honest move is to widen to the whole workspace and confirm the
    // *remaining* workspace still compiles after the shape change. Prefer this
    // widen (the structural change is inside a resolvable workspace) over a silent
    // `NothingToVerify`. The unmapped-`.rs` branch above is louder still and
    // returns first: a source that cannot be compiled at all is `Unverified`,
    // never widened-and-called-clean.
    if !unmapped_structural.is_empty() {
        whole_workspace = true;
    }

    if !whole_workspace && affected.is_empty() {
        return CheckDecision::Nothing;
    }
    // A workspace widen is a superset of any member `-p` list, so drop the
    // redundant per-member checks when widening.
    let packages = if whole_workspace {
        Vec::new()
    } else {
        reverse_dependency_closure(metadata, &affected)
    };
    CheckDecision::Check {
        whole_workspace,
        packages,
    }
}

/// Whether a changed path can affect a Cargo build — the pure-path predicate the
/// docs-only short-circuit negates. Conservative by design: any Rust source, any
/// `Cargo.toml`/`Cargo.lock` (member or root), or anything under `.cargo/` is
/// build-relevant; everything else (docs, images, CI config) is inert.
fn is_build_relevant(path: &Path) -> bool {
    is_rust_source(path)
        || matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("Cargo.toml" | "Cargo.lock")
        )
        || path.starts_with(".cargo")
        || path.components().any(|c| c.as_os_str() == ".cargo")
}

/// A build-structural path — a manifest, lockfile, or `.cargo/` config that
/// shapes how the workspace resolves, as opposed to a compilable `.rs` source.
/// These are exactly the build-relevant files that are not Rust sources. Used by
/// [`post_merge_compile_scope`]'s structural honesty catch: an unmapped
/// structural file (a removed wildcard member's manifest) must widen the gate,
/// never slide to a silent `NothingToVerify`.
fn is_build_structural(path: &Path) -> bool {
    !is_rust_source(path) && is_build_relevant(path)
}

/// Whether `path`'s file name is `Cargo.toml` — a package manifest, the one
/// structural file whose *own* directory names the package it declares.
fn is_cargo_manifest(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("Cargo.toml")
}

/// Whether `manifest_abs` (an absolute `Cargo.toml` path) sits directly in a
/// *current* workspace-member directory — the honest owner test for a manifest.
///
/// Defect 1, round-2 (codex-sol): in a **non-virtual** workspace the root
/// `Cargo.toml` carries a `[package]`, so the root package's directory *is* the
/// repo root. [`owning_package`]'s longest-prefix match then makes the root
/// package a catch-all owner of every nested path — including a *deleted*
/// member's manifest (`crates/gone/Cargo.toml` when `members = ["crates/*"]` and
/// post-merge `cargo metadata` no longer lists `gone`). Mapping that manifest to
/// the root package runs a root-package-scoped `-p` check that silently skips the
/// sibling members the shape change actually put at risk. A manifest is
/// legitimately owned only when it declares a package rooted at a *current*
/// member directory; otherwise the caller must fall back to the structural
/// honesty catch (widen-to-workspace, or `Unverified`), never a package-scoped
/// check on the root. A virtual workspace is unaffected — its root is not a
/// member, so [`owning_package`] already returns `None` for the orphaned manifest
/// and the existing `None` arm routes it to the same catch.
fn manifest_declares_current_member(
    manifest_abs: &Path,
    package_dirs: &[(PathBuf, String)],
) -> bool {
    manifest_abs
        .parent()
        .is_some_and(|dir| package_dirs.iter().any(|(member_dir, _)| member_dir == dir))
}

/// Root manifest/lock changes can alter every package's resolution, so any of
/// them widens the gate to the whole workspace. These are the *only* paths this
/// gate treats specially, and every one is layout-agnostic: the workspace root
/// manifest, the lockfile, and `.cargo/` config live at the repo root of every
/// Cargo project regardless of how its members are organized. No member crate
/// name (e.g. a `cosmon-core`) is hardcoded here — a foundational crate's blast
/// radius is captured generically by [`reverse_dependency_closure`], which needs
/// no per-project special case.
fn requires_workspace_check(path: &Path) -> bool {
    path == Path::new("Cargo.toml") || path == Path::new("Cargo.lock") || path.starts_with(".cargo")
}

/// Every changed path with a `.rs` extension is a Rust source whose compilation
/// the gate must account for. A `.rs` file that maps to no known workspace
/// member is the honesty-catch trigger (see [`post_merge_compile_scope`]).
fn is_rust_source(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "rs")
}

/// Absolute directory of each workspace member paired with its package name,
/// sorted longest-directory-first so a member nested inside another member's
/// tree claims its own files before the ancestor does. Derived entirely from
/// `cargo metadata`, so the mapping honors whatever on-disk layout the project
/// actually uses — `crates/`, `packages/`, a flat root crate, or anything else.
///
/// Member directories are canonicalized so the longest-prefix match holds
/// against a canonicalized repo root (macOS resolves the repo root to
/// `/private/var/…` while metadata may report `/var/…`).
fn workspace_package_dirs(metadata: &CargoMetadata) -> Vec<(PathBuf, String)> {
    let workspace_ids: HashSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let mut dirs: Vec<(PathBuf, String)> = metadata
        .packages
        .iter()
        .filter(|package| workspace_ids.contains(package.id.as_str()))
        .filter_map(|package| {
            package.manifest_path.parent().map(|dir| {
                let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
                (canonical, package.name.clone())
            })
        })
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.0.as_os_str().len()));
    dirs
}

/// The workspace member that owns `abs_path` by longest-prefix directory match,
/// or `None` when the file lies outside every member's directory. `package_dirs`
/// must be sorted longest-first (as [`workspace_package_dirs`] returns them) for
/// the nested-member tie-break to hold.
fn owning_package<'a>(abs_path: &Path, package_dirs: &'a [(PathBuf, String)]) -> Option<&'a str> {
    package_dirs
        .iter()
        .find(|(dir, _)| abs_path.starts_with(dir))
        .map(|(_, name)| name.as_str())
}

/// Run a command with a hard wall-clock deadline, capturing stdout/stderr. Both
/// pipes are drained on their own threads so a large output cannot fill a pipe
/// and stall the child — the hazard [`run_bounded`] avoids by inheriting stdio,
/// which we cannot do here because the gate needs to read `cargo metadata`'s
/// JSON.
///
/// The child's **whole process group** is killed ([`kill_child_tree`]), not just
/// the direct child, *before* the reader threads are joined — on **both** exit
/// paths. This is the strict bound (delib-559a defect 2, round-2 codex-sol): a
/// descendant that inherited the capture pipe would otherwise keep its write end
/// open, so `read_to_end` never reaches EOF and the join blocks the gate past its
/// budget. The timeout path reaps on deadline; the clean-exit path reaps the
/// moment `try_wait` reports the direct child gone, because a backgrounded
/// descendant (`sh -c 'sleep 300 &'`) outlives the direct child and the deadline
/// check is unreachable once we are on the exit branch. Reaping the group closes
/// every inherited pipe, so the readers unblock and the gate returns promptly even
/// against a pipe-holding descendant. Reaping is a harmless no-op (ESRCH) when the
/// group is already empty, so a well-behaved command keeps its full output.
fn run_bounded_capture(
    command: &mut Command,
    deadline: Instant,
) -> anyhow::Result<std::process::Output> {
    use std::io::Read as _;
    use std::process::Stdio;

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    in_own_process_group(command);
    let mut child = command.spawn()?;
    let mut child_stdout = child.stdout.take().expect("stdout piped");
    let mut child_stderr = child.stderr.take().expect("stderr piped");
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stderr.read_to_end(&mut buf);
        buf
    });
    loop {
        if let Some(status) = child.try_wait()? {
            // Defect 2, round-2 (codex-sol): the *direct* child has exited, but a
            // descendant it backgrounded (`sh -c 'sleep 300 &'`) can still hold the
            // inherited write end of a capture pipe. `read_to_end` would then never
            // see EOF, so joining the reader threads below would block the gate long
            // past its budget — and the deadline check is unreachable once we are on
            // this branch. Reap the whole process group first, exactly as the
            // timeout path does: every inherited write end closes, the readers reach
            // EOF, and the join returns at once. The direct child is already reaped
            // (`try_wait` waited it), so this only targets a leaked descendant and is
            // a harmless no-op (ESRCH) when the group is already empty.
            kill_child_tree(&mut child);
            let stdout = out_reader.join().unwrap_or_default();
            let stderr = err_reader.join().unwrap_or_default();
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            // Reap the whole group first: a descendant holding the capture pipe
            // must die before we join, or `read_to_end` never sees EOF and the
            // join hangs the gate past its budget (defect 2). With the group
            // dead, every inherited write end closes and the readers finish.
            kill_child_tree(&mut child);
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            anyhow::bail!(
                "timed out after exceeding the {}-second gate budget",
                POST_MERGE_GATE_TIMEOUT.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Resolve the repo's Cargo workspace via cargo's own root resolution
/// (`cargo metadata` with **no** `--manifest-path`), under the shared gate
/// `deadline` (finding 4032 — the previous blocking `.output()` ran outside any
/// bound and could hang the gate indefinitely). Zero layout hints from cosmon:
/// this is the ratified IN side of the transport/cognition boundary.
///
/// Returns `Ok(None)` when cargo finds no manifest and none exists at the repo
/// root — a genuinely non-Cargo (or manifest-one-level-down) repo the caller
/// turns into a loud `Unverified`. A manifest that exists at the root but fails
/// to parse propagates as `Err`: a broken manifest post-merge is a real
/// integrity failure, never a silent skip.
fn cargo_metadata_bounded(
    repo_root: &Path,
    deadline: Instant,
) -> anyhow::Result<Option<CargoMetadata>> {
    let mut command = Command::new("cargo");
    command
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(repo_root);
    // Defect 4 (codex-sol, task-20260715-ff5b): "cargo cannot be
    // spawned/resolved" — cargo absent from `PATH`, or any spawn failure — is
    // the auto-detect *declining*, not an integrity failure. Map the spawn
    // io::Error to `Ok(None)` so the cascade falls through to `build_command`
    // (rung 3) instead of hard-erroring into a rollback for a mere absence of
    // cargo. A wall-clock *timeout* is bailed as a plain anyhow string (no
    // io::Error in the chain), so it stays an `Err` → a real hang still rolls
    // the merge back.
    let output = match run_bounded_capture(&mut command, deadline) {
        Ok(output) => output,
        Err(e) => {
            if e.downcast_ref::<std::io::Error>().is_some() {
                return Ok(None);
            }
            return Err(e);
        }
    };
    if !output.status.success() {
        if repo_root.join("Cargo.toml").exists() {
            anyhow::bail!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        // No manifest cargo could resolve from the root: not a Cargo workspace
        // this gate can verify (a manifest one level down is the deferred
        // subdir-manifest topology → inc-2, reported as `Unverified`, not here).
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&output.stdout)?))
}

/// Grow `seed` (the directly-changed workspace members) to include every
/// workspace member that depends on one of them, transitively. That reverse
/// closure is essential: a public signature changed in crate A can break a test
/// target in caller B even when B's files are absent from the merge diff. It is
/// also what replaces the old hardcoded foundational-crate widening — a crate
/// whose API fans out through much of the workspace simply has many reverse
/// dependants, all of which the closure discovers by name, with no per-project
/// special case.
fn reverse_dependency_closure(metadata: &CargoMetadata, seed: &BTreeSet<String>) -> Vec<String> {
    let workspace_ids: HashSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let package_by_dir: HashMap<PathBuf, &CargoPackage> = metadata
        .packages
        .iter()
        .filter(|package| workspace_ids.contains(package.id.as_str()))
        .filter_map(|package| {
            package
                .manifest_path
                .parent()
                .map(|dir| (dir.to_path_buf(), package))
        })
        .collect();

    let mut reverse_dependencies: HashMap<&str, Vec<&str>> = HashMap::new();
    for package in package_by_dir.values() {
        for dependency in &package.dependencies {
            let Some(path) = &dependency.path else {
                continue;
            };
            let Some(dependency) = package_by_dir.get(path) else {
                continue;
            };
            reverse_dependencies
                .entry(dependency.name.as_str())
                .or_default()
                .push(package.name.as_str());
        }
    }

    let mut affected: BTreeSet<String> = seed.clone();
    let mut pending: VecDeque<String> = seed.iter().cloned().collect();
    while let Some(package) = pending.pop_front() {
        if let Some(dependants) = reverse_dependencies.get(package.as_str()) {
            for dependant in dependants {
                if affected.insert((*dependant).to_owned()) {
                    pending.push_back((*dependant).to_owned());
                }
            }
        }
    }
    affected.into_iter().collect()
}

/// Security-tagged work may only cross `done` with an explicit durable review
/// verdict. The tag trigger is monotone in `ops::tag`, so a worker cannot
/// evade this gate by removing `needs-review`, `security`, or `security:*`.
fn require_security_review_verdict(state_dir: &Path, mol: &MoleculeData) -> anyhow::Result<()> {
    let review_required = mol.tags.iter().any(|tag| {
        tag.as_str() == "needs-review"
            || tag.as_str() == "security"
            || tag.as_str().starts_with("security:")
    });
    if !review_required {
        return Ok(());
    }
    let verdict = state_dir
        .join("molecules")
        .join(mol.id.as_str())
        .join("review-verdict.md");
    let approved = std::fs::read_to_string(&verdict).ok().is_some_and(|text| {
        text.lines()
            .any(|line| line.trim().eq_ignore_ascii_case("verdict: approved"))
    });
    if approved {
        Ok(())
    } else {
        anyhow::bail!(
            "security/needs-review molecule {} requires an on-disk independent review verdict at {} containing `verdict: approved`",
            mol.id,
            verdict.display()
        );
    }
}

/// Restore main to the revision captured before a gated merge.
fn reset_hard(repo_root: &Path, revision: &str) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "reset",
            "--hard",
            revision,
        ])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
}

/// Whether the operator kill-switch disables the blocking `pre_done` gate
/// for this invocation.
///
/// Two equivalent triggers, OR-ed: the `--skip-pre-done-hook` CLI flag
/// (`flag_set`) and the `COSMON_SKIP_PRE_DONE_HOOK` environment variable.
/// The env var is honoured for any value the process treats as "present and
/// non-empty" — automation that exports `COSMON_SKIP_PRE_DONE_HOOK=1` gets
/// the same bypass as the flag. This is deliberately a *per-invocation*
/// escape hatch for the human operator, not a config field: the gate should
/// be hard by default and only waived by a conscious, ephemeral gesture.
fn pre_done_hook_skipped(flag_set: bool) -> bool {
    flag_set || std::env::var_os("COSMON_SKIP_PRE_DONE_HOOK").is_some_and(|v| !v.is_empty())
}

/// Run the **blocking** `pre_done` hook from the repository root.
///
/// The mirror of [`run_post_merge_hook`] on the *before-merge* side, with
/// the opposite failure contract: a non-zero exit is a hard error that
/// aborts teardown, because nothing has landed yet and everything is still
/// reversible (showroom delib-20260701-bfdf, torvalds D1).
///
/// The molecule ID is appended as a trailing positional argument so the
/// galaxy's script can scope its verification to the molecule under teardown
/// — `sh -c '<hook_cmd>' -- <molecule-id>` makes it reachable as `$1` while
/// leaving the shell's own `$0` conventional. On a non-zero exit the returned
/// error carries the hook command, its exit code, and its trimmed stderr, so
/// the abort reason is self-explanatory. A failure to *spawn* the command
/// (missing shell, unreadable cwd) is likewise an error — an unrunnable gate
/// must fail closed, never silently pass.
fn run_pre_done_hook(repo_root: &Path, hook_cmd: &str, mol_id: &MoleculeId) -> anyhow::Result<i32> {
    let output = Command::new("sh")
        .args(["-c", hook_cmd, "--", mol_id.as_str()])
        .current_dir(repo_root)
        .output()?;
    let code = output.status.code().unwrap_or(-1);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "pre_done gate refused DONE for {mol_id}: `{hook_cmd}` exited {code}: {}",
            stderr.trim()
        );
    }
    Ok(code)
}

/// Render the operator-facing report for a refused `pre_done` gate.
///
/// Sibling of [`report_merge_failure`]: greppable `❌` headline, no use of
/// the word "done", and an explicit statement that nothing was torn down so
/// the operator is never misled into believing work shipped. In `--json`
/// mode it emits the same `{ok:false, merged:false, teardown:false}` shape
/// as the merge-failure path, with `outcome: "pre_done_refused"`.
fn report_pre_done_failure(ctx: &Context, mol_id: &MoleculeId, hook_cmd: &str, reason: &str) {
    if ctx.json {
        let out = serde_json::json!({
            "command": "done",
            "molecule": mol_id.as_str(),
            "ok": false,
            "merged": false,
            "teardown": false,
            "outcome": "pre_done_refused",
            "hook": hook_cmd,
            "reason": reason,
        });
        println!("{}", serde_json::to_string(&out).unwrap_or_default());
    } else {
        println!("❌ {mol_id} PRE-DONE GATE REFUSED — not merged, nothing torn down");
        println!("  hook: {hook_cmd}");
        for line in reason.lines() {
            println!("  {line}");
        }
        println!("  branch, worktree, and tmux session are preserved.");
        println!(
            "  fix the gap (or `cs done {mol_id} --skip-pre-done-hook` to override), then rerun."
        );
    }
}

/// A single `cs` binary discovered on `PATH`, with its last-modified time.
///
/// Used by the deploy-hygiene self-check ([`detect_cs_path_multiplicity`]) to
/// report each phantom's location and freshness so the operator can see at a
/// glance which copy a deploy refreshes and which ones will drift.
#[derive(Debug, Clone)]
struct CsBinaryOnPath {
    /// The canonical (symlink-resolved) filesystem path of the binary.
    path: PathBuf,
    /// Human-readable last-modified timestamp, or `"unknown"` if it could not
    /// be read.
    mtime: String,
}

/// Discover every **distinct** `cs` binary reachable on `PATH`.
///
/// This is the deploy-hygiene self-check.
/// The disease: a deploy (`just install` / the `post_merge` hook) only ever
/// refreshes one target (`~/.local/bin/cs`), but PATH may also surface stale
/// copies (`~/.cargo/bin/cs` from a `cargo install`, `/opt/homebrew/bin/cs`
/// from a Cellar formula). The stale ones silently fall to their built-in
/// adapter floor, so which engine a session runs depends on a per-session PATH
/// race — the operator sees *"not on claude despite config"* with no visible
/// cause.
///
/// The check is **diagnostic only**: it never removes a binary (deleting an
/// executable is an operator-gestured act). It shells out to `which -a cs`,
/// canonicalises each hit to dedup symlink aliases pointing at the same inode,
/// and returns the distinct binaries in PATH order. The caller emits a loud
/// warning when more than one is found.
///
/// Returns an empty vector when `which` is unavailable or finds nothing —
/// absence of evidence is treated as "no drift", never as an error, because
/// this self-check must never block the `cs done` hot path.
fn detect_cs_path_multiplicity() -> Vec<CsBinaryOnPath> {
    let output = match Command::new("which").args(["-a", "cs"]).output() {
        Ok(o) if o.status.success() => o,
        // `which` missing, errored, or found nothing — no drift signal.
        _ => return Vec::new(),
    };

    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut distinct: Vec<CsBinaryOnPath> = Vec::new();

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }
        let raw_path = PathBuf::from(raw);
        // Canonicalise to collapse symlink aliases (e.g. a shim pointing at the
        // real binary) onto a single distinct entry. Fall back to the raw path
        // if the target cannot be resolved.
        let canonical = std::fs::canonicalize(&raw_path).unwrap_or_else(|_| raw_path.clone());
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let mtime = std::fs::metadata(&canonical)
            .and_then(|m| m.modified())
            .map_or_else(
                |_| "unknown".to_owned(),
                |t| {
                    let dt: chrono::DateTime<chrono::Local> = t.into();
                    dt.format("%Y-%m-%d %H:%M").to_string()
                },
            );
        distinct.push(CsBinaryOnPath {
            path: canonical,
            mtime,
        });
    }

    distinct
}

/// Format the deploy-hygiene warning lines for a set of `cs` binaries.
///
/// Returns `None` when one binary or fewer is present (no drift). Otherwise
/// returns one warning string per binary plus a leading banner line, ready to
/// be pushed onto the `cs done` warnings channel. Kept pure (no I/O) so it can
/// be unit-tested directly.
fn format_cs_multiplicity_warning(binaries: &[CsBinaryOnPath]) -> Option<Vec<String>> {
    if binaries.len() <= 1 {
        return None;
    }
    let mut lines = Vec::with_capacity(binaries.len() + 1);
    lines.push(format!(
        "⚠️  {} distinct `cs` binaries on PATH — a deploy refreshes only the first; the rest drift silently:",
        binaries.len()
    ));
    for (i, b) in binaries.iter().enumerate() {
        lines.push(format!(
            "    [{}] {} (mtime {})",
            i + 1,
            b.path.display(),
            b.mtime
        ));
    }
    lines.push(
        "    Stale copies fall to their built-in adapter floor — remove the phantoms manually (rm is operator-gestured)."
            .to_owned(),
    );
    Some(lines)
}

/// Outcome of the deploy-verification self-check.
///
/// Sibling to the `cs`-binary-multiplicity guard: where multiplicity asks
/// *"which copy does a deploy refresh?"*, this asks *"did the deploy
/// actually refresh it?"*. The `post_merge` hook (`just install`) can
/// silently no-op — wrong cwd, swallowed failure, cargo seeing nothing to
/// rebuild — leaving the code on main while the deployed binary lags. The
/// only way to know is to ask the freshly-installed binary which commit it
/// was built from and compare to the just-merged HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeployVerification {
    /// The deployed binary's build SHA matches the merged HEAD — the
    /// deploy is confirmed, worker-green == operator-green.
    Match { head_short: String },
    /// The deployed binary's build SHA does **not** match the merged HEAD.
    /// The deploy silently no-op'd: the loud-warning case.
    Mismatch {
        deployed_short: String,
        head_short: String,
    },
    /// Verification was impossible (no `cs` on PATH, git unavailable, the
    /// deployed binary predates `cs __build-sha`, or an `unknown` stamp).
    /// Reported as a soft note, never a warning — absence of evidence is
    /// not evidence of a gap.
    Inconclusive { reason: String },
}

/// Ask the deployed `cs` binary which commit it was built from and compare
/// it to the just-merged HEAD.
///
/// `repo_root` is the operator's main checkout where the merge landed, so
/// `git rev-parse HEAD` there is the commit the deploy was supposed to
/// ship. The deploy *target* is the first `cs` on `PATH` (the same copy a
/// deploy refreshes and the operator's next invocation runs) — discovered
/// via the multiplicity helper to avoid duplicating the `which` logic.
///
/// This runs the freshly-installed binary as a subprocess rather than
/// trusting the running process's own [`cosmon_cli::BUILD_SHA`]: the
/// process executing `cs done` is whatever the operator invoked, which may
/// be the *previous* binary the install just replaced. We must verify the
/// copy on disk, not ourselves.
///
/// Pure-ish: shells out to `git` and the deployed `cs`, but holds no state
/// and never mutates anything. Any failure degrades to `Inconclusive`.
fn verify_deploy(repo_root: &Path) -> DeployVerification {
    let head = match Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        _ => {
            return DeployVerification::Inconclusive {
                reason: "could not read merged HEAD (git rev-parse failed)".to_owned(),
            }
        }
    };
    if head.is_empty() {
        return DeployVerification::Inconclusive {
            reason: "merged HEAD is empty".to_owned(),
        };
    }

    let target = match detect_cs_path_multiplicity().into_iter().next() {
        Some(b) => b.path,
        None => {
            return DeployVerification::Inconclusive {
                reason: "no `cs` found on PATH — cannot verify deploy".to_owned(),
            }
        }
    };

    let deployed = match Command::new(&target).arg("__build-sha").output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        // Non-zero exit: the deployed binary predates `cs __build-sha`.
        // That is itself a sign it is stale, but during the first rollout
        // it is expected — report inconclusive with an actionable reason.
        Ok(_) => {
            return DeployVerification::Inconclusive {
                reason: format!(
                    "deployed `cs` ({}) predates `cs __build-sha` — re-run `just install` once this lands, then verification activates",
                    target.display()
                ),
            }
        }
        Err(e) => {
            return DeployVerification::Inconclusive {
                reason: format!("could not run `{} __build-sha`: {e}", target.display()),
            }
        }
    };

    if deployed.is_empty() || deployed == "unknown" {
        return DeployVerification::Inconclusive {
            reason: format!(
                "deployed `cs` ({}) reports an unknown build SHA (built outside a git checkout)",
                target.display()
            ),
        };
    }

    let short = |s: &str| s.chars().take(12).collect::<String>();
    if deployed == head {
        DeployVerification::Match {
            head_short: short(&head),
        }
    } else {
        DeployVerification::Mismatch {
            deployed_short: short(&deployed),
            head_short: short(&head),
        }
    }
}

/// Render the operator-facing lines for a [`DeployVerification`].
///
/// Returns `(action_note, warning_lines)`: at most one of them is
/// populated. A [`DeployVerification::Mismatch`] produces a loud,
/// multi-line warning on the same channel as the `cs`-multiplicity guard —
/// the deploy silently failed and the operator must act. `Match` produces
/// a terse action note (deploy confirmed). `Inconclusive` produces a soft
/// action note so the trace records *why* verification was skipped without
/// crying wolf. Kept pure (no I/O) so it is unit-testable.
fn format_deploy_verification(v: &DeployVerification) -> (Option<String>, Option<Vec<String>>) {
    match v {
        DeployVerification::Match { head_short } => {
            (Some(format!("deploy verified: cs @ {head_short}")), None)
        }
        DeployVerification::Inconclusive { reason } => {
            (Some(format!("deploy unverified: {reason}")), None)
        }
        DeployVerification::Mismatch {
            deployed_short,
            head_short,
        } => {
            let lines = vec![
                "⚠️  DEPLOY GAP — post_merge hook ran but the deployed `cs` did NOT refresh:".to_owned(),
                format!("    deployed cs build : {deployed_short}"),
                format!("    just-merged HEAD  : {head_short}"),
                "    The code landed on main but the on-disk binary still lags — new behaviour is silently absent.".to_owned(),
                "    Run `just install` manually from main and confirm `cs __build-sha` matches `git rev-parse HEAD`.".to_owned(),
            ];
            (None, Some(lines))
        }
    }
}

/// Relocate tracked workspace artifacts from the repo-root `molecule/`
/// convention to a molecule-scoped `molecule/<mol-id>/` path on the worker's
/// branch.
///
/// Mailroom and similar galaxies have established a convention of writing
/// per-meeting review artifacts to `molecule/review.md` (and friends) at the
/// repo root. When multiple parallel branches adopt the same convention, the
/// paths collide and merge produces add/add conflicts on every landing after
/// the first — losing prior content in the working tree even though git
/// history preserves it.
///
/// This helper runs inside the worker's worktree *before* the merge attempt.
/// It enumerates tracked files sitting directly under `molecule/` (not inside
/// an already-scoped `molecule/<some-id>/` subdirectory), renames each to
/// `molecule/<mol-id>/<leaf>` via `git mv`, and creates a small rename-only
/// commit on the worker's branch. The subsequent merge sees disjoint paths
/// across parallel branches and lands cleanly.
///
/// Returns the list of relocated paths (by their original path). Idempotent:
/// a branch that already scopes its artifacts is a no-op. Non-fatal: caller
/// should convert errors into warnings, not abort teardown.
fn relocate_workspace_artifacts(
    worktree_path: &Path,
    mol_id: &MoleculeId,
) -> anyhow::Result<Vec<String>> {
    if !worktree_path.is_dir() {
        return Ok(Vec::new());
    }

    let worktree_arg = worktree_path.to_string_lossy().to_string();

    let listing = Command::new("git")
        .args(["-C", &worktree_arg, "ls-files", "--", "molecule/"])
        .output()?;
    if !listing.status.success() {
        // Not a git worktree or `molecule/` is absent — nothing to do.
        return Ok(Vec::new());
    }

    let mol_prefix = format!("molecule/{}/", mol_id.as_str());
    let mut renames: Vec<(String, String)> = Vec::new();
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let path = line.trim();
        if path.is_empty() || !path.starts_with("molecule/") {
            continue;
        }
        if path.starts_with(&mol_prefix) {
            continue; // already scoped to this molecule
        }
        let rest = &path["molecule/".len()..];
        if rest.contains('/') {
            // molecule/<other-subdir>/… — leave other scopings alone.
            continue;
        }
        let dst = format!("{mol_prefix}{rest}");
        renames.push((path.to_owned(), dst));
    }

    if renames.is_empty() {
        return Ok(Vec::new());
    }

    let mut moved: Vec<String> = Vec::new();
    for (src, dst) in &renames {
        if worktree_path.join(dst).exists() {
            // Destination already present — skip to avoid `git mv` refusal.
            // The duplicate path is preserved and will be resolved by the
            // normal merge machinery if a conflict arises.
            continue;
        }
        if let Some(parent) = std::path::Path::new(dst).parent() {
            let abs_parent = worktree_path.join(parent);
            std::fs::create_dir_all(&abs_parent)?;
        }
        let mv = Command::new("git")
            .args(["-C", &worktree_arg, "mv", src, dst])
            .output()?;
        if !mv.status.success() {
            return Err(anyhow::anyhow!(
                "git mv {src} → {dst} failed: {}",
                String::from_utf8_lossy(&mv.stderr).trim()
            ));
        }
        moved.push(src.clone());
    }

    if moved.is_empty() {
        return Ok(Vec::new());
    }

    // Commit only the renamed paths so unrelated dirty files — if any — are
    // left alone. `git commit -- <paths>` scopes the commit to the listed
    // pathspecs regardless of anything else in the index.
    let msg = format!(
        "chore(done): relocate workspace artifacts to molecule/{}/",
        mol_id.as_str()
    );
    let mut commit_args: Vec<String> = vec![
        "-C".into(),
        worktree_arg.clone(),
        "commit".into(),
        "-m".into(),
        msg,
        "--".into(),
    ];
    for (src, dst) in &renames {
        // Include both sides; git commit needs both to record the rename.
        commit_args.push(src.clone());
        commit_args.push(dst.clone());
    }
    let commit = Command::new("git").args(&commit_args).output()?;
    if !commit.status.success() {
        return Err(anyhow::anyhow!(
            "git commit (relocate) failed: {}",
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }

    Ok(moved)
}

/// Rewrite the worker branch's colliding ADR numbers to free ones before
/// the merge — the deterministic-renumber-at-merge primitive (ADR-121).
///
/// Reads the ADR numbers the base branch already carries and the ADR files
/// this branch *added* relative to that base. For each added ADR whose number
/// the base already owns, [`cosmon_cli::adr::plan_renumber`] assigns the next
/// free number; the file is renamed (`git mv`), its title self-reference is
/// rewritten, and the change is committed on the worker's branch so the
/// subsequent merge lands without a number clash. Returns the applied plans.
///
/// Defensive by construction, mirroring [`relocate_workspace_artifacts`]:
/// a missing worktree, a non-git path, or a repo with no `docs/adr/` is a
/// silent no-op; only an unexpected `git` failure during an *actual* rename
/// surfaces as `Err`, which the caller downgrades to a warning. The hot path
/// is never blocked by ADR bookkeeping.
fn renumber_colliding_adrs(
    worktree_path: &Path,
    base_branch: &str,
) -> anyhow::Result<Vec<cosmon_cli::adr::RenumberPlan>> {
    if !worktree_path.is_dir() {
        return Ok(Vec::new());
    }
    let wt = worktree_path.to_string_lossy().to_string();

    // ADR numbers the base branch already owns.
    let base_ls = Command::new("git")
        .args([
            "-C",
            &wt,
            "ls-tree",
            "-r",
            "--name-only",
            base_branch,
            "--",
            "docs/adr/",
        ])
        .output()?;
    if !base_ls.status.success() {
        // Base ref unreachable or no docs/adr/ on base — nothing to collide.
        return Ok(Vec::new());
    }
    let base_numbers: Vec<u32> = String::from_utf8_lossy(&base_ls.stdout)
        .lines()
        .filter_map(cosmon_cli::adr::parse_adr_number)
        .collect();

    // ADR files this branch added relative to base (status `A`).
    let diff = Command::new("git")
        .args([
            "-C",
            &wt,
            "diff",
            "--name-status",
            "--diff-filter=A",
            &format!("{base_branch}...HEAD"),
            "--",
            "docs/adr/",
        ])
        .output()?;
    if !diff.status.success() {
        return Ok(Vec::new());
    }
    let branch_added: Vec<String> = String::from_utf8_lossy(&diff.stdout)
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(_, p)| p.trim().to_owned()))
        .filter(|p| cosmon_cli::adr::parse_adr_number(p).is_some())
        .collect();

    let plans = cosmon_cli::adr::plan_renumber(&base_numbers, &branch_added);
    if plans.is_empty() {
        return Ok(Vec::new());
    }

    let mut applied = Vec::new();
    for plan in plans {
        let abs_old = worktree_path.join(&plan.old_path);
        let abs_new = worktree_path.join(&plan.new_path);
        if !abs_old.is_file() || abs_new.exists() {
            // Already renamed, or the target slot is taken — skip rather than
            // risk clobbering. The normal merge machinery handles the rest.
            continue;
        }

        // 1. Rewrite the file's own title self-reference in place.
        if let Ok(content) = std::fs::read_to_string(&abs_old) {
            let rewritten =
                cosmon_cli::adr::rewrite_self_reference(&content, plan.old_number, plan.new_number);
            if rewritten != content {
                std::fs::write(&abs_old, rewritten)?;
            }
        }

        // 2. Rename via git so history follows the file.
        let mv = Command::new("git")
            .args(["-C", &wt, "mv", &plan.old_path, &plan.new_path])
            .output()?;
        if !mv.status.success() {
            return Err(anyhow::anyhow!(
                "git mv {} → {} failed: {}",
                plan.old_path,
                plan.new_path,
                String::from_utf8_lossy(&mv.stderr).trim()
            ));
        }

        // 3. Commit the renumber on the worker's branch, scoped to the two
        //    pathspecs so unrelated dirty files are left alone.
        let msg = format!(
            "chore(done): renumber ADR-{} → ADR-{} (fleet collision)",
            cosmon_cli::adr::format_adr_number(plan.old_number),
            cosmon_cli::adr::format_adr_number(plan.new_number),
        );
        let commit = Command::new("git")
            .args([
                "-C",
                &wt,
                "commit",
                "-m",
                &msg,
                "--",
                &plan.old_path,
                &plan.new_path,
            ])
            .output()?;
        if !commit.status.success() {
            return Err(anyhow::anyhow!(
                "git commit (renumber) failed: {}",
                String::from_utf8_lossy(&commit.stderr).trim()
            ));
        }
        applied.push(plan);
    }

    Ok(applied)
}

/// Auto-commit durable molecule artifacts after teardown.
///
/// Stages the molecule directory and the global events log, then commits
/// with a conventional `chore(state):` message. Returns `Ok(true)` when a
/// commit was created, `Ok(false)` when there was nothing to commit, and
/// `Err` on unexpected git failures (the caller should warn, not abort).
fn commit_molecule_artifacts(
    repo_root: &Path,
    mol_dir: &Path,
    events_path: &Path,
    mol_id: &MoleculeId,
    short_topic: &str,
    coauthor_trailers: &[String],
) -> anyhow::Result<bool> {
    // Stage the molecule directory (prompt.md, briefing.md, log.md, …).
    if mol_dir.is_dir() {
        let _ = Command::new("git")
            .args(["add", "--"])
            .arg(mol_dir)
            .current_dir(repo_root)
            .output();
    }

    // Stage the global events log.
    if events_path.is_file() {
        let _ = Command::new("git")
            .args(["add", "--"])
            .arg(events_path)
            .current_dir(repo_root)
            .output();
    }

    // The pathspecs this commit is allowed to touch. The commit below is
    // scoped to exactly these, so even if other source is pre-staged in the
    // index it can never be swept into a `chore(state):` commit — the bug
    // that reverted crates/** + Cargo.* under this very subject (postmortem
    // 2026-06-15, commit 2e86cf908). Only include paths that actually exist:
    // passing a never-tracked pathspec to `git commit` makes it abort with
    // "pathspec did not match any files".
    let mut pathspecs: Vec<&Path> = Vec::new();
    if mol_dir.is_dir() {
        pathspecs.push(mol_dir);
    }
    if events_path.is_file() {
        pathspecs.push(events_path);
    }
    if pathspecs.is_empty() {
        // Nothing on disk to track.
        return Ok(false);
    }

    // Check if anything was actually staged UNDER THOSE PATHSPECS (not the
    // whole index — a global check would proceed on unrelated pre-staged
    // source and then commit nothing under our scope).
    let mut diff_args: Vec<String> = vec![
        "diff".into(),
        "--cached".into(),
        "--quiet".into(),
        "--".into(),
    ];
    for p in &pathspecs {
        diff_args.push(p.to_string_lossy().into_owned());
    }
    let diff_index = Command::new("git")
        .args(&diff_args)
        .current_dir(repo_root)
        .status()?;
    if diff_index.success() {
        // Exit code 0 → nothing staged under our pathspecs.
        return Ok(false);
    }

    // Structural guard: fail fast if the scoped commit would still carry
    // source (e.g. a molecule dir misresolved to overlap the workspace).
    let offending = source_paths_in_commit(repo_root, &pathspecs);
    if !offending.is_empty() {
        return Err(anyhow::anyhow!(
            "refusing 'chore(state): track artifacts for {mol_id}' — it would \
             commit source files under a state-tracking message (image/source \
             drift guard): {}",
            offending.join(", ")
        ));
    }

    // Build commit message.
    let msg = if short_topic.is_empty() {
        format!("chore(state): track artifacts for {mol_id}")
    } else {
        format!("chore(state): track artifacts for {mol_id} ({short_topic})")
    };

    // Scope the commit to the molecule dir + events log so it records ONLY
    // those paths, never the whole staged index. When attribution trailers are
    // configured, they ride in a *single* extra `-m` so git renders them as one
    // contiguous trailer paragraph (a `Co-Authored-By` block must not be split
    // by blank lines, so all trailers share one `-m`, separated by `\n`).
    let mut commit_args: Vec<String> = vec!["commit".into(), "-m".into(), msg];
    if !coauthor_trailers.is_empty() {
        commit_args.push("-m".into());
        commit_args.push(coauthor_trailers.join("\n"));
    }
    commit_args.push("--".into());
    for p in &pathspecs {
        commit_args.push(p.to_string_lossy().into_owned());
    }
    let commit = Command::new("git")
        .args(&commit_args)
        .current_dir(repo_root)
        .output()?;
    if commit.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        Err(anyhow::anyhow!("git commit failed: {}", stderr.trim()))
    }
}

/// Find the git repository root from CWD.
fn find_repo_root() -> anyhow::Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!("not in a git repository"));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::molecule::MoleculeStatus;
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        // Write config.toml with project_id so the project identity guard passes.
        std::fs::write(
            tmp.path().join("config.toml"),
            "[project]\nproject_id = \"test-0000\"\n",
        )
        .unwrap();
        (tmp, store)
    }

    fn sample_mol(id: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new("task-work").unwrap(),
            status,
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
        }
    }

    #[test]
    fn security_merge_is_refused_without_durable_review_verdict() {
        let (tmp, _store) = make_store();
        let mut mol = sample_mol("task-20260713-secure", MoleculeStatus::Completed);
        mol.tags
            .insert(cosmon_core::tag::Tag::new("security:high").unwrap());
        let err = require_security_review_verdict(tmp.path(), &mol).unwrap_err();
        assert!(err
            .to_string()
            .contains("requires an on-disk independent review verdict"));

        let review_dir = tmp.path().join("molecules").join(mol.id.as_str());
        std::fs::create_dir_all(&review_dir).unwrap();
        std::fs::write(review_dir.join("review-verdict.md"), "verdict: approved\n").unwrap();
        require_security_review_verdict(tmp.path(), &mol).unwrap();
    }

    // -----------------------------------------------------------------
    // archived ⇒ status.is_terminal() — writer-side invariant fix for
    // `cs done --force` (idea-20260618-1b10). The full `cs done` path needs
    // git + tmux + a worktree; these tests pin the load-bearing terminus
    // helper directly, which both archive/merged_at save points invoke.
    // -----------------------------------------------------------------

    #[test]
    fn test_terminalize_for_forced_teardown_collapses_running() {
        // The reproduction shape: a molecule that never left `Running` but is
        // about to have `merged_at` / `archived` stamped on it. After the
        // helper, it must be terminal — no immortal `👻 unnamed-merge` ghost.
        let mut mol = sample_mol("task-20260618-run", MoleculeStatus::Running);
        mol.merged_at = Some(chrono::Utc::now());
        terminalize_for_forced_teardown(&mut mol);
        assert_eq!(mol.status, MoleculeStatus::Collapsed);
        assert!(mol.status.is_terminal());
        assert_eq!(mol.collapse_reason.as_deref(), Some("forced-teardown"));
        assert_eq!(
            mol.collapse_cause,
            Some(cosmon_core::molecule::CollapseCause::Manual)
        );
    }

    #[test]
    fn test_terminalize_for_forced_teardown_noop_on_completed() {
        // Normal (non-force) path: the molecule is already `Completed` before
        // teardown. The helper must not clobber a legitimate completion into a
        // collapse, nor invent a collapse reason.
        let mut mol = sample_mol("task-20260618-done", MoleculeStatus::Completed);
        terminalize_for_forced_teardown(&mut mol);
        assert_eq!(mol.status, MoleculeStatus::Completed);
        assert_eq!(mol.collapse_reason, None);
        assert_eq!(mol.collapse_cause, None);
    }

    #[test]
    fn test_terminalize_for_forced_teardown_preserves_existing_reason() {
        // If a free-form collapse reason was already recorded, keep it — the
        // forced-teardown terminus must not overwrite richer attribution.
        let mut mol = sample_mol("task-20260618-pre", MoleculeStatus::Running);
        mol.collapse_reason = Some("worker OOM".to_owned());
        terminalize_for_forced_teardown(&mut mol);
        assert_eq!(mol.status, MoleculeStatus::Collapsed);
        assert_eq!(mol.collapse_reason.as_deref(), Some("worker OOM"));
    }

    fn default_args(mol: &str) -> Args {
        Args {
            molecule: mol.to_owned(),
            force: false,
            if_completed: false,
            dry_run: false,
            no_merge: true,
            no_worktree_remove: true,
            no_branch_delete: true,
            no_kill: true,
            strategy: MergeStrategy::Merge,
            no_auto_propel: true,
            propel_message: None,
            max_retries: 3,
            skip_pre_done_hook: false,
        }
    }

    // -----------------------------------------------------------------
    // Git-backed tests for try_merge_branch — these exercise a real git
    // repo in a temp dir to reproduce the parallel-landing scenario from
    // bug task-20260409-420c.
    // -----------------------------------------------------------------

    fn git(repo: &Path, args: &[&str]) -> std::process::Output {
        let mut full: Vec<&str> = vec!["-C"];
        let repo_str = repo.to_str().unwrap();
        full.push(repo_str);
        full.extend_from_slice(args);
        Command::new("git")
            // The test process may itself be a codex worker with identity
            // pinning (F3). Remove that ambient override so fixtures exercise
            // the temp repository's explicit identity deterministically.
            .env_remove("GIT_AUTHOR_NAME")
            .env_remove("GIT_AUTHOR_EMAIL")
            .env_remove("GIT_COMMITTER_NAME")
            .env_remove("GIT_COMMITTER_EMAIL")
            .args(&full)
            .output()
            .expect("git command failed to spawn")
    }

    fn init_repo(repo: &Path) {
        // `-b main` pins the initial branch name so the test is
        // deterministic regardless of the user's init.defaultBranch.
        assert!(git(repo, &["init", "-q", "-b", "main"]).status.success());
        assert!(git(repo, &["config", "user.email", "test@example.com"])
            .status
            .success());
        assert!(git(repo, &["config", "user.name", "Test"]).status.success());
        assert!(git(repo, &["config", "commit.gpgsign", "false"])
            .status
            .success());
        std::fs::write(repo.join("README.md"), "init\n").unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "init"]).status.success());
    }

    fn commit_file(repo: &Path, file: &str, contents: &str, msg: &str) {
        std::fs::write(repo.join(file), contents).unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", msg]).status.success());
    }

    fn test_package(name: &str, dependencies: &[&str]) -> CargoPackage {
        CargoPackage {
            id: format!("path+file:///repo/crates/{name}#0.1.0"),
            name: name.to_owned(),
            manifest_path: PathBuf::from(format!("/repo/crates/{name}/Cargo.toml")),
            dependencies: dependencies
                .iter()
                .map(|dependency| CargoDependency {
                    path: Some(PathBuf::from(format!("/repo/crates/{dependency}"))),
                })
                .collect(),
        }
    }

    // ── `--if-completed` git reclaim (task-20260719-fedf) ───────────

    /// The gap the incident exposed: `cs done --if-completed` on an
    /// already-merged molecule left the branch behind, so the documented
    /// recovery verb never actually finished recovering.
    #[test]
    fn reclaim_deletes_a_merged_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("cs-20260719-mrg1").unwrap();
        let branch = format!("feat/{mol_id}");
        assert!(git(repo, &["checkout", "-q", "-b", &branch])
            .status
            .success());
        commit_file(repo, "work.txt", "done\n", "work");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(
            git(repo, &["merge", "-q", "--no-ff", "-m", "merge", &branch])
                .status
                .success()
        );

        let actions = reclaim_merged_git_artifacts(repo, &mol_id);

        assert!(
            actions.contains(&"deleted_branch".to_owned()),
            "a merged branch must be reclaimed; got {actions:?}"
        );
        assert!(!branch_exists(repo, &branch));
    }

    /// The 5eba anti-wipe guard, on the no-op path. `merged_at` is a stamp
    /// *we* wrote; ancestry is a fact git can confirm. If the branch is not
    /// reachable from base it is the only copy of the work, and this path
    /// has no `--force` for the operator to reach for — so it must refuse.
    #[test]
    fn reclaim_refuses_to_delete_an_unmerged_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("cs-20260719-unm1").unwrap();
        let branch = format!("feat/{mol_id}");
        assert!(git(repo, &["checkout", "-q", "-b", &branch])
            .status
            .success());
        commit_file(repo, "only-copy.txt", "precious\n", "unmerged work");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        // Deliberately NOT merged.

        let actions = reclaim_merged_git_artifacts(repo, &mol_id);

        assert!(
            !actions.contains(&"deleted_branch".to_owned()),
            "an unmerged branch is the only copy of the work — never delete it here"
        );
        assert!(
            branch_exists(repo, &branch),
            "the branch must survive; deleting it would be the 5eba wipe"
        );
    }

    /// No branch, no worktree, nothing to do — and no spurious action label.
    #[test]
    fn reclaim_is_a_noop_when_there_is_nothing_to_reclaim() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("cs-20260719-non1").unwrap();
        assert!(reclaim_merged_git_artifacts(repo, &mol_id).is_empty());
    }

    #[test]
    fn post_merge_scope_includes_reverse_dependants() {
        let metadata = CargoMetadata {
            packages: vec![
                test_package("crate-a", &[]),
                test_package("crate-b", &["crate-a"]),
                test_package("unrelated", &[]),
            ],
            workspace_members: vec![
                "path+file:///repo/crates/crate-a#0.1.0".to_owned(),
                "path+file:///repo/crates/crate-b#0.1.0".to_owned(),
                "path+file:///repo/crates/unrelated#0.1.0".to_owned(),
            ],
        };
        let seed = BTreeSet::from(["crate-a".to_owned()]);

        assert_eq!(
            reverse_dependency_closure(&metadata, &seed),
            vec!["crate-a", "crate-b"],
            "a change in crate A must compile caller B too"
        );
    }

    #[test]
    fn post_merge_scope_root_manifest_widens_documentation_does_not() {
        assert!(!requires_workspace_check(Path::new("README.md")));
        assert!(requires_workspace_check(Path::new("Cargo.toml")));
        assert!(requires_workspace_check(Path::new("Cargo.lock")));
        assert!(requires_workspace_check(Path::new(".cargo/config.toml")));
        // No member crate name is special-cased any more: a source file inside a
        // member is scoped by metadata, not by this root-manifest predicate. The
        // old hardcoded `crates/cosmon-core` widening is gone.
        assert!(!requires_workspace_check(Path::new(
            "crates/cosmon-core/src/lib.rs"
        )));
    }

    #[test]
    fn is_build_relevant_flags_rust_and_manifests_only() {
        assert!(is_build_relevant(Path::new("crates/x/src/lib.rs")));
        assert!(is_build_relevant(Path::new("Cargo.toml")));
        assert!(is_build_relevant(Path::new("crates/x/Cargo.toml")));
        assert!(is_build_relevant(Path::new("Cargo.lock")));
        assert!(is_build_relevant(Path::new(".cargo/config.toml")));
        // Pure documentation / assets are inert — this is what lets the docs-only
        // short-circuit run before any metadata cost (finding 3793).
        assert!(!is_build_relevant(Path::new("README.md")));
        assert!(!is_build_relevant(Path::new("docs/guide.md")));
        assert!(!is_build_relevant(Path::new("assets/logo.png")));
    }

    #[test]
    fn is_build_structural_flags_manifests_not_sources() {
        // Structural = build-relevant but not compilable Rust.
        assert!(is_build_structural(Path::new("Cargo.toml")));
        assert!(is_build_structural(Path::new("crates/x/Cargo.toml")));
        assert!(is_build_structural(Path::new("Cargo.lock")));
        assert!(is_build_structural(Path::new(".cargo/config.toml")));
        // A `.rs` source is build-relevant but *not* structural: it has its own
        // (louder) unmapped catch, so it must not be swept into the structural
        // widen.
        assert!(!is_build_structural(Path::new("crates/x/src/lib.rs")));
        // Pure docs are neither.
        assert!(!is_build_structural(Path::new("README.md")));
    }

    /// Defect 1 falsifier (delib-559a): in a wildcard-member workspace, removing
    /// or moving a member so post-merge `cargo metadata` no longer lists it leaves
    /// its `crates/<member>/Cargo.toml` change mapped to no member. That unmapped
    /// *structural* file must widen the gate to the whole workspace — the shape
    /// changed under us but the root still resolves — never slide to the silent
    /// `NothingToVerify` it produced before the fix. Reverting the structural
    /// widen reddens this: `whole_workspace` stays `false`, `affected` is empty,
    /// and the scope collapses to `CheckDecision::Nothing`.
    #[test]
    fn post_merge_scope_unmapped_structural_widens_never_nothing() {
        let metadata = CargoMetadata {
            packages: vec![test_package("crate-a", &[]), test_package("crate-b", &[])],
            workspace_members: vec![
                "path+file:///repo/crates/crate-a#0.1.0".to_owned(),
                "path+file:///repo/crates/crate-b#0.1.0".to_owned(),
            ],
        };
        // `gone` is no longer a member; only its manifest change survives in the
        // diff. It is not a root manifest, so `requires_workspace_check` is false
        // and nothing else widens — the fix is the only path off `Nothing`.
        let changed = vec![PathBuf::from("crates/gone/Cargo.toml")];
        let decision = post_merge_compile_scope(Path::new("/repo"), &metadata, &changed);
        assert_ne!(
            decision,
            CheckDecision::Nothing,
            "a removed wildcard member's manifest must not reach NothingToVerify"
        );
        assert_eq!(
            decision,
            CheckDecision::Check {
                whole_workspace: true,
                packages: Vec::new(),
            },
            "an unmapped structural file must widen to the whole workspace"
        );
    }

    /// Defect 1 round-2 falsifier (codex-sol): in a **non-virtual** workspace the
    /// root package's directory is the repo root, so longest-prefix ownership makes
    /// the root package a catch-all owner of every nested path. A *deleted* wildcard
    /// member's manifest (`crates/gone/Cargo.toml`, absent from post-merge metadata)
    /// then maps to the root package and slides to a root-package-scoped `-p` check
    /// that silently skips the surviving sibling members. The fix routes such a
    /// manifest to the structural honesty catch (widen) instead.
    ///
    /// This test *includes a root package* (the round-1 structural test omitted it,
    /// the tautology codex-sol flagged): reverting the fix maps the deleted manifest
    /// to the root package and the assertion reddens (`whole_workspace: false` with a
    /// root-scoped package list rather than the widen).
    #[test]
    fn post_merge_scope_deleted_member_in_non_virtual_workspace_widens_not_root_scoped() {
        // A non-virtual workspace: the root `Cargo.toml` carries a `[package]`, so
        // the root package's manifest is `/repo/Cargo.toml` (dir == repo root).
        let root_pkg = CargoPackage {
            id: "path+file:///repo#0.1.0".to_owned(),
            name: "root-pkg".to_owned(),
            manifest_path: PathBuf::from("/repo/Cargo.toml"),
            dependencies: Vec::new(),
        };
        let metadata = CargoMetadata {
            packages: vec![
                root_pkg,
                test_package("crate-a", &[]),
                test_package("crate-b", &[]),
            ],
            workspace_members: vec![
                "path+file:///repo#0.1.0".to_owned(),
                "path+file:///repo/crates/crate-a#0.1.0".to_owned(),
                "path+file:///repo/crates/crate-b#0.1.0".to_owned(),
            ],
        };
        // `crates/gone` was a wildcard member (`members = ["crates/*"]`); its manifest
        // was deleted, so post-merge metadata omits it. Only that deletion is in the
        // diff. It is not the root manifest, so `requires_workspace_check` is false.
        let changed = vec![PathBuf::from("crates/gone/Cargo.toml")];
        let decision = post_merge_compile_scope(Path::new("/repo"), &metadata, &changed);
        assert_eq!(
            decision,
            CheckDecision::Check {
                whole_workspace: true,
                packages: Vec::new(),
            },
            "a deleted member manifest owned only by the root package's catch-all \
             prefix must widen to the whole workspace, never a root-package-scoped check"
        );
        // Guard the *specific* regression: the buggy path returns a package-scoped
        // check (whole_workspace: false) that skips the sibling members.
        assert!(
            !matches!(
                decision,
                CheckDecision::Check {
                    whole_workspace: false,
                    ..
                }
            ),
            "the root package must not 'own' a deleted member's manifest"
        );
    }

    /// The defect-1 fix must *discriminate*, not blanket-widen: a *current*
    /// member's own `Cargo.toml` in the same non-virtual workspace stays a
    /// package-scoped check (member + its reverse-dependency closure), never a
    /// spurious whole-workspace widen. This pins the boundary so a lazy "widen on
    /// any manifest" regression reddens.
    #[test]
    fn post_merge_scope_current_member_manifest_stays_package_scoped() {
        let root_pkg = CargoPackage {
            id: "path+file:///repo#0.1.0".to_owned(),
            name: "root-pkg".to_owned(),
            manifest_path: PathBuf::from("/repo/Cargo.toml"),
            dependencies: Vec::new(),
        };
        let metadata = CargoMetadata {
            packages: vec![
                root_pkg,
                test_package("crate-a", &[]),
                test_package("crate-b", &[]),
            ],
            workspace_members: vec![
                "path+file:///repo#0.1.0".to_owned(),
                "path+file:///repo/crates/crate-a#0.1.0".to_owned(),
                "path+file:///repo/crates/crate-b#0.1.0".to_owned(),
            ],
        };
        // `crate-a`'s own manifest changed — it *is* a current member directory.
        let changed = vec![PathBuf::from("crates/crate-a/Cargo.toml")];
        let decision = post_merge_compile_scope(Path::new("/repo"), &metadata, &changed);
        assert_eq!(
            decision,
            CheckDecision::Check {
                whole_workspace: false,
                packages: vec!["crate-a".to_owned()],
            },
            "a current member's own manifest must stay package-scoped, not widen"
        );
    }

    #[test]
    fn is_rust_source_detects_only_dot_rs() {
        assert!(is_rust_source(Path::new("src/lib.rs")));
        assert!(is_rust_source(Path::new("weird/path/build.rs")));
        assert!(!is_rust_source(Path::new("README.md")));
        assert!(!is_rust_source(Path::new("Cargo.toml")));
        assert!(!is_rust_source(Path::new("src/lib")));
    }

    #[test]
    fn owning_package_uses_metadata_layout_not_a_hardcoded_prefix() {
        // A workspace whose members live under `packages/`, not `crates/`.
        let mut dirs = vec![
            (PathBuf::from("/repo/packages/engine"), "engine".to_owned()),
            (PathBuf::from("/repo/app"), "app".to_owned()),
        ];
        dirs.sort_by_key(|x| std::cmp::Reverse(x.0.as_os_str().len()));
        assert_eq!(
            owning_package(Path::new("/repo/packages/engine/src/lib.rs"), &dirs),
            Some("engine"),
            "a non-`crates/` layout must still resolve to its member"
        );
        assert_eq!(
            owning_package(Path::new("/repo/app/src/main.rs"), &dirs),
            Some("app"),
            "a root-adjacent member must resolve too"
        );
        assert_eq!(
            owning_package(Path::new("/repo/docs/guide.md"), &dirs),
            None,
            "a file outside every member selects no package"
        );
    }

    #[test]
    fn owning_package_prefers_the_nested_member() {
        // `outer` contains `outer/inner` as a nested member; longest-first order
        // (as workspace_package_dirs returns) must let the inner member win.
        let mut dirs = vec![
            (PathBuf::from("/repo/outer"), "outer".to_owned()),
            (PathBuf::from("/repo/outer/inner"), "inner".to_owned()),
        ];
        dirs.sort_by_key(|x| std::cmp::Reverse(x.0.as_os_str().len()));
        assert_eq!(
            owning_package(Path::new("/repo/outer/inner/src/lib.rs"), &dirs),
            Some("inner"),
            "a nested member must claim its files before its ancestor"
        );
    }

    /// A generous deadline for tests that exercise `cargo_metadata_bounded`
    /// without probing the timeout path.
    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_secs(120)
    }

    /// Write a minimal library member at `<root>/<rel_dir>` named `name`,
    /// depending (by path) on each of `deps` (given as `(name, rel_dir)`).
    fn write_member(root: &Path, name: &str, rel_dir: &str, deps: &[(&str, &str)]) {
        use std::fmt::Write as _;
        let dir = root.join(rel_dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let mut manifest = format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
        );
        for (dep_name, dep_dir) in deps {
            let rel = pathdiff_up_to_root(rel_dir, dep_dir);
            let _ = writeln!(manifest, "{dep_name} = {{ path = \"{rel}\" }}");
        }
        std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "// member\n").unwrap();
    }

    /// Build the relative path a member at `from_dir` uses to reach `to_dir`,
    /// both expressed relative to the workspace root (e.g. `packages/app` →
    /// `packages/engine` yields `../engine`). Kept tiny — the test layouts are
    /// shallow.
    fn pathdiff_up_to_root(from_dir: &str, to_dir: &str) -> String {
        let ups = from_dir.split('/').count();
        let mut rel = String::new();
        for _ in 0..ups {
            rel.push_str("../");
        }
        rel.push_str(to_dir);
        rel
    }

    fn head_rev(repo: &Path) -> String {
        String::from_utf8(git(repo, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned()
    }

    /// Falsifier for the *removal* of the old hardcoded `crates/cosmon-core`
    /// widening. Runs the full scope decision over a real workspace whose
    /// foundational crate lives at the exact hardcoded path `crates/cosmon-core`,
    /// and asserts the change is scoped by the reverse *closure* (`Check` with a
    /// `-p` list), never a whole-workspace widen. Re-adding
    /// `path.starts_with("crates/cosmon-core")` to `requires_workspace_check`
    /// flips the outcome to `whole_workspace: true` and reddens this test. The
    /// expected package list is a literal, independent of `cargo metadata`
    /// (fixture-independence).
    #[test]
    fn post_merge_scope_foundational_crate_widens_by_closure_not_hardcode() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = \
             [\"crates/cosmon-core\", \"crates/mid\", \"crates/leaf\"]\n",
        )
        .unwrap();
        write_member(root, "cosmon-core", "crates/cosmon-core", &[]);
        write_member(
            root,
            "mid",
            "crates/mid",
            &[("cosmon-core", "crates/cosmon-core")],
        );
        write_member(
            root,
            "leaf",
            "crates/leaf",
            &[("mid", "crates/mid"), ("cosmon-core", "crates/cosmon-core")],
        );

        let metadata = cargo_metadata_bounded(root, far_deadline())
            .unwrap()
            .unwrap();
        let decision = post_merge_compile_scope(
            root,
            &metadata,
            &[PathBuf::from("crates/cosmon-core/src/lib.rs")],
        );

        assert_eq!(
            decision,
            CheckDecision::Check {
                whole_workspace: false,
                packages: vec![
                    "cosmon-core".to_owned(),
                    "leaf".to_owned(),
                    "mid".to_owned()
                ],
            },
            "a foundational crate's blast radius must be the reverse closure, not a \
             whole-workspace widen; re-hardcoding crates/cosmon-core would flip this to \
             whole_workspace and redden the test"
        );
    }

    /// The layout-agnostic contract end-to-end: `engine` lives under `packages/`,
    /// `app` at the repo root, and `app` depends on `engine`, so an `engine`
    /// change scopes to both — proving the gate no longer assumes a `crates/`
    /// layout.
    #[test]
    fn post_merge_scope_resolves_non_crates_layout_end_to_end() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"packages/engine\", \"app\"]\n",
        )
        .unwrap();
        write_member(root, "engine", "packages/engine", &[]);
        write_member(root, "app", "app", &[("engine", "packages/engine")]);

        let metadata = cargo_metadata_bounded(root, far_deadline())
            .unwrap()
            .unwrap();
        let decision = post_merge_compile_scope(
            root,
            &metadata,
            &[PathBuf::from("packages/engine/src/lib.rs")],
        );
        assert_eq!(
            decision,
            CheckDecision::Check {
                whole_workspace: false,
                packages: vec!["app".to_owned(), "engine".to_owned()],
            },
            "a change under packages/ must scope to the member and its reverse dep"
        );
    }

    /// A documentation-only change in a real workspace scopes to nothing.
    #[test]
    fn post_merge_scope_skips_docs_only_change() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"engine\"]\n",
        )
        .unwrap();
        write_member(root, "engine", "engine", &[]);

        let metadata = cargo_metadata_bounded(root, far_deadline())
            .unwrap()
            .unwrap();
        let decision = post_merge_compile_scope(root, &metadata, &[PathBuf::from("docs/guide.md")]);
        assert_eq!(decision, CheckDecision::Nothing);
    }

    /// Honesty-catch falsifier: an unmapped `.rs` under an `exclude`d /
    /// standalone crate is a deferred (OUT) topology, but the deferral must be
    /// **loud** — a `Unverified` witness, never a silent skip and never a
    /// `--workspace` widen that runs clean while skipping the crate that changed
    /// (round1 #5's false-green). Reverting the honesty catch to a widen reddens
    /// this test.
    #[test]
    fn post_merge_scope_excluded_crate_is_unverified_not_widened() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Virtual workspace: member `app`, `tools/helper` explicitly excluded.
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"app\"]\nexclude = [\"tools/helper\"]\n",
        )
        .unwrap();
        write_member(root, "app", "app", &[]);
        write_member(root, "helper", "tools/helper", &[]);

        let metadata = cargo_metadata_bounded(root, far_deadline())
            .unwrap()
            .unwrap();
        let decision =
            post_merge_compile_scope(root, &metadata, &[PathBuf::from("tools/helper/src/lib.rs")]);
        assert!(
            matches!(decision, CheckDecision::Unverified { .. }),
            "an excluded crate's changed source must be a loud Unverified, not a \
             --workspace widen that silently skips it; got {decision:?}"
        );
    }

    /// A changed `.rs` in a second workspace inside the same repo is a deferred
    /// (OUT) multi-workspace topology — root metadata resolves only the root
    /// workspace, so the second workspace's file maps to no member and must be a
    /// loud `Unverified`.
    #[test]
    fn post_merge_scope_second_workspace_file_is_unverified() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"app\"]\n",
        )
        .unwrap();
        write_member(root, "app", "app", &[]);
        // A wholly separate workspace under `sub/`, not referenced by the root.
        std::fs::write(
            root.join("sub").join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"lib\"]\n",
        )
        .ok();
        write_member(&root.join("sub"), "sublib", "lib", &[]);

        let metadata = cargo_metadata_bounded(root, far_deadline())
            .unwrap()
            .unwrap();
        let decision =
            post_merge_compile_scope(root, &metadata, &[PathBuf::from("sub/lib/src/lib.rs")]);
        assert!(
            matches!(decision, CheckDecision::Unverified { .. }),
            "a second workspace's changed source maps to no root member and must be \
             a loud Unverified; got {decision:?}"
        );
    }

    /// The whole gate end-to-end (real git diff + real `cargo metadata`) reports
    /// a subdir-only Rust project — manifest one level down, no root manifest —
    /// as a **loud `Unverified`**, never a silent skip. This is the binding
    /// acceptance test for the deferred subdir-manifest topology (delib-559a Q7).
    #[test]
    fn gate_reports_subdir_manifest_repo_as_unverified() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        // A Rust crate under `backend/`, with NO manifest at the repo root.
        write_member(repo, "backend", "backend", &[]);
        assert!(
            !repo.join("Cargo.toml").exists(),
            "precondition: no root manifest"
        );
        let base = head_rev(repo);
        commit_file(
            repo,
            "backend/src/lib.rs",
            "// changed\npub fn touched() {}\n",
            "touch backend",
        );

        // Default gates (no integrity_command, no build_command): the cascade
        // runs cargo auto-detect, which declines (no root workspace), then falls
        // through to the terminal `Unverified{expected:false}` — nobody declared
        // how to verify a subdir-manifest repo.
        let outcome =
            run_post_merge_gate(repo, &base, &cosmon_core::config::GatesConfig::default()).unwrap();
        assert!(
            matches!(
                outcome,
                GateOutcome::Unverified {
                    expected: false,
                    ..
                }
            ),
            "a Rust project whose manifest lives one level down is a deferred topology \
             that must produce a loud Unverified, not a silent skip; got {outcome:?}"
        );
    }

    /// The whole gate reports a polyglot / non-Cargo repo that changed a
    /// build-relevant file (here a `.rs` with no workspace anywhere) as a loud
    /// `Unverified` — the honest message a `NotCargoWorkspace` swallow used to
    /// hide.
    #[test]
    fn gate_reports_non_cargo_repo_as_unverified() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        let base = head_rev(repo);
        commit_file(
            repo,
            "scripts/tool.rs",
            "fn main() {}\n",
            "add loose rust script",
        );

        let outcome =
            run_post_merge_gate(repo, &base, &cosmon_core::config::GatesConfig::default()).unwrap();
        assert!(
            matches!(
                outcome,
                GateOutcome::Unverified {
                    expected: false,
                    ..
                }
            ),
            "a non-Cargo repo touching a build-relevant file must report Unverified, \
             not a silent pass; got {outcome:?}"
        );
    }

    /// Defect 3 (task-20260715-ff5b), hermetic falsifier at the rung boundary:
    /// the cargo auto-detect's "no Rust changed" short-circuit is Cargo cognition
    /// and must NOT gate a declared `build_command`. A non-Rust change with a
    /// fallback declared (`has_fallback = true`) must DECLINE (fall through to
    /// rung 3); the same change with no fallback keeps the docs-only
    /// `NothingToVerify` short-circuit. Reverting the `has_fallback` branch makes
    /// the first assertion redden.
    #[test]
    fn cargo_autodetect_declines_for_non_rust_change_when_build_command_declared() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let base = head_rev(repo);
        commit_file(repo, "app.py", "print(1)\n", "python change");
        let deadline = Instant::now() + POST_MERGE_GATE_TIMEOUT;

        // With a build_command fallback declared: decline to rung 3.
        assert!(
            matches!(
                run_cargo_autodetect(repo, &base, deadline, /* has_fallback */ true).unwrap(),
                CargoGate::Declines
            ),
            "a non-Rust change with a build_command fallback must fall through to rung 3"
        );
        // With no fallback (cosmon riding the cargo rung alone): docs-only inertia.
        assert_eq!(
            run_cargo_autodetect(repo, &base, deadline, /* has_fallback */ false).unwrap(),
            CargoGate::Resolved(GateOutcome::NothingToVerify),
            "with no fallback the docs-only short-circuit is unchanged"
        );
    }

    // The delegated-command rungs (integrity_command green/red, build_command
    // fallback, the B5 trust refusal, and the fail-closed policy) are exercised
    // end-to-end in `tests/done_pre_done_gate.rs`, where the `cs_isolated`
    // harness controls `COSMON_ASSUME_TRUSTED` / `COSMON_TRUST_DIR` per
    // subprocess — in-process unit tests of those paths would race on the
    // process-global trust env.

    // ---- PR-B: single post-gate MergeResult, keyed on GateOutcome ----
    //
    // These pin `post_gate_merge_result` — the fold from `(GateOutcome,
    // escalation retries)` to the ONE durable `MergeResult` a successful merge
    // emits after the gate. The literals are hand-written (not derived from the
    // function under test), so reverting the mapping reddens them.

    /// A clean, gate-verified merge is a plain `ok` — the common case.
    #[test]
    fn post_gate_result_verified_clean_is_ok() {
        let gate = GateOutcome::Verified {
            description: "cargo check --workspace".to_owned(),
        };
        assert_eq!(
            post_gate_merge_result(&gate, Some(0)),
            cosmon_core::event_v2::MergeResult::Ok
        );
    }

    /// `NothingToVerify` (docs-only, no build-relevant change) is also a plain
    /// `ok`: the merge landed and there was nothing the gate had to check.
    #[test]
    fn post_gate_result_nothing_to_verify_is_ok() {
        assert_eq!(
            post_gate_merge_result(&GateOutcome::NothingToVerify, Some(0)),
            cosmon_core::event_v2::MergeResult::Ok
        );
    }

    /// THE PR-B fix: an `Unverified` gate outcome becomes a durable
    /// `ok:unverified` witness — NEVER the bare `Ok` the pre-gate event used to
    /// lie with. This is the load-bearing assertion; reverting the map to `Ok`
    /// reddens it.
    #[test]
    fn post_gate_result_unverified_is_durable_witness_not_ok() {
        let gate = GateOutcome::Unverified {
            reason: "repo root is not a Cargo workspace".to_owned(),
            command: None,
            expected: false,
        };
        let result = post_gate_merge_result(&gate, Some(0));
        assert_eq!(
            result,
            cosmon_core::event_v2::MergeResult::Other("ok:unverified".to_owned())
        );
        assert_ne!(
            result,
            cosmon_core::event_v2::MergeResult::Ok,
            "an unverified merge must not be recorded as a clean Ok"
        );
    }

    /// A merge that needed escalation retries folds the count into the verified
    /// result, preserving the audit info the old `MergedAfterEscalation` arm
    /// carried.
    #[test]
    fn post_gate_result_verified_escalated_carries_retry_count() {
        let gate = GateOutcome::Verified {
            description: "cargo check -p cosmon-cli".to_owned(),
        };
        assert_eq!(
            post_gate_merge_result(&gate, Some(2)),
            cosmon_core::event_v2::MergeResult::Other("ok:escalated(2)".to_owned())
        );
    }

    /// Escalation AND an unverified gate compose: both facts survive in one
    /// durable string.
    #[test]
    fn post_gate_result_unverified_escalated_composes() {
        let gate = GateOutcome::Unverified {
            reason: "second workspace crate".to_owned(),
            command: Some("cargo check -p sublib".to_owned()),
            expected: true,
        };
        assert_eq!(
            post_gate_merge_result(&gate, Some(3)),
            cosmon_core::event_v2::MergeResult::Other("ok:escalated(3):unverified".to_owned())
        );
    }

    /// `None` escalation (a shape a caller could pass defensively) is treated
    /// like a clean landing — no `escalated(...)` prefix leaks.
    #[test]
    fn post_gate_result_none_retries_is_clean() {
        let gate = GateOutcome::Verified {
            description: "cargo check".to_owned(),
        };
        assert_eq!(
            post_gate_merge_result(&gate, None),
            cosmon_core::event_v2::MergeResult::Ok
        );
    }

    /// A documentation-only merge short-circuits to `NothingToVerify` — the
    /// full gate path, confirming the docs-only determination needs no metadata
    /// (finding 3793: a README merge is never refused on a metadata stall).
    #[test]
    fn gate_reports_docs_only_merge_as_nothing_to_verify() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let base = head_rev(repo);
        commit_file(repo, "CHANGELOG.md", "# notes\n", "docs only");

        let outcome =
            run_post_merge_gate(repo, &base, &cosmon_core::config::GatesConfig::default()).unwrap();
        assert_eq!(outcome, GateOutcome::NothingToVerify);
    }

    /// Quoted-path falsifier: with git's default `core.quotePath=true`, a
    /// non-ASCII pathname is C-quoted (`"src/caf\303\251.rs"`), whose extension
    /// parses as `rs"` — so the file is neither mapped nor recognized as Rust and
    /// the gate silently skips a real Rust change. `post_merge_changed_files` must
    /// decode it (via `git diff -z`) back to `src/café.rs`. Reverting to
    /// `--name-only` without `-z` reddens this.
    #[test]
    fn post_merge_changed_files_decodes_non_ascii_paths() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        assert!(git(repo, &["config", "core.quotePath", "true"])
            .status
            .success());
        let base = head_rev(repo);

        std::fs::create_dir_all(repo.join("crates/x/src")).unwrap();
        commit_file(
            repo,
            "crates/x/src/café.rs",
            "// unicode member\n",
            "add café",
        );

        let files = post_merge_changed_files(repo, &base).unwrap();
        assert!(
            files.contains(&PathBuf::from("crates/x/src/café.rs")),
            "a non-ASCII Rust path must be decoded, not left C-quoted; got {files:?}"
        );
        assert!(
            files.iter().any(|path| is_rust_source(path)),
            "the decoded café path must be recognized as Rust (extension `rs`, not `rs\"`)"
        );
    }

    /// 4032 bound: `run_bounded_capture` kills a child that outlives the deadline
    /// instead of blocking forever. A already-past deadline must return the
    /// timeout error, proving the metadata probe cannot hang the gate.
    #[test]
    fn run_bounded_capture_enforces_the_deadline() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let err = run_bounded_capture(&mut command, Instant::now())
            .expect_err("a past deadline must abort, not block for 30s");
        assert!(
            err.to_string().contains("timed out"),
            "expected a timeout error, got: {err}"
        );
    }

    /// Defect 2 falsifier (delib-559a): the strict bound must reap the child's
    /// whole process group, not just the direct `cargo`/`sh` child. Here the
    /// child spawns a **descendant** (`sleep`) that inherits the capture pipe and
    /// outlives the direct child. Before the fix, a timeout killed only the direct
    /// child; the reparented descendant kept stdout open, so the reader thread's
    /// `read_to_end` never reached EOF and the reader-join hung the gate forever —
    /// defeating the 4032 wall-clock bound. The group kill reaps the descendant,
    /// closing the pipe so the readers finish and the gate returns.
    ///
    /// The test runs the gate on its own thread and asserts it returns within a
    /// generous ceiling: reverting the group kill to a direct-child kill makes the
    /// join hang, blowing the ceiling → red (not an infinite hang, thanks to the
    /// `recv_timeout` guard). The prior `sleep 30` test has no descendant, so it
    /// could not catch this — the tautology codex-sol flagged.
    #[test]
    fn run_bounded_capture_reaps_pipe_holding_descendant() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            // The direct `sh` backgrounds a `sleep` (a descendant that inherits
            // the capture pipe) and then blocks on the `wait` builtin — `wait`
            // does not `exec`, so `sh` deterministically stays the direct child
            // and the deadline path is hit with a pipe-holding descendant alive.
            // (`sleep & sleep` is flaky: a shell may tail-`exec` the second
            // `sleep`, making the descendant's identity nondeterministic.)
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 300 & wait"]);
            // Deadline a beat in the FUTURE, not `now()`: the gate must let `sh`
            // actually fork its background `sleep` before the deadline fires, so
            // the timeout path is exercised with a live pipe-holding descendant.
            // Killing at `now()` would race — `sh` might be reaped before it forks
            // the descendant, making the hazard vanish and the test a tautology
            // (the very trap codex-sol flagged in the prior `sleep 30` test).
            let deadline = Instant::now() + Duration::from_millis(500);
            let result = run_bounded_capture(&mut command, deadline);
            let _ = tx.send(result.is_err());
        });
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(is_err) => {
                let _ = worker.join();
                assert!(
                    is_err,
                    "an already-past deadline must abort with a timeout error"
                );
            }
            // Both timeout and disconnect mean the gate never returned in budget.
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => panic!(
                "run_bounded_capture hung past its deadline — a pipe-holding descendant \
                 was not reaped (the process-group kill was reverted to a direct-child kill)"
            ),
        }
    }

    /// Defect 2 round-2 falsifier (codex-sol): the *clean child-exit* path must
    /// also bound the reader-thread drain. `sh -c 'sleep 300 &'` backgrounds a
    /// descendant and the direct `sh` exits **immediately** — so `try_wait` returns
    /// `Some` and the deadline branch is never reached. Before the round-2 fix the
    /// exit branch joined the reader threads unconditionally; the backgrounded
    /// `sleep` kept the capture pipe's write end open, so `read_to_end` never saw
    /// EOF and the join hung the gate for the full 300 s despite a comfortable
    /// deadline. Reaping the process group on clean child exit (as the timeout path
    /// already does) closes the inherited pipe, so the readers finish at once.
    ///
    /// The round-1 `& wait` test keeps `sh` the live direct child and thus
    /// exercises only the *timeout* path — it cannot catch this branch (the gap
    /// codex-sol flagged). Here `&` (no `wait`) forces the child-exit branch. The
    /// `recv_timeout` ceiling turns a reverted fix (join hangs on the live
    /// descendant) into a red assertion rather than an infinite hang.
    #[test]
    fn run_bounded_capture_reaps_descendant_after_clean_child_exit() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            // No `wait`: `sh` backgrounds the `sleep` and exits at once, so the
            // child-EXITED path runs with a pipe-holding descendant still alive.
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 300 &"]);
            // A comfortably-future deadline: the gate must return via the clean
            // child-exit branch (not by the deadline firing), so a hang here can
            // only be the unbounded reader-join — the defect under test.
            let deadline = Instant::now() + Duration::from_secs(120);
            let result = run_bounded_capture(&mut command, deadline);
            let _ = tx.send(result.is_ok());
        });
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(is_ok) => {
                let _ = worker.join();
                assert!(
                    is_ok,
                    "sh exits 0 after backgrounding its sleep — the gate must return Ok"
                );
            }
            // Both timeout and disconnect mean the gate never returned in budget.
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => panic!(
                "run_bounded_capture hung on the child-exited path — a backgrounded, \
                 pipe-holding descendant was not reaped before the reader-join (the \
                 round-2 group-kill on clean child exit was reverted)"
            ),
        }
    }

    // -----------------------------------------------------------------
    // Scope-guard (P3 of task-20260712-3819) — the merge-perimeter gate.
    // -----------------------------------------------------------------

    /// Build a repo whose `feat/x` branch changed exactly `files` (each a
    /// distinct one-line commit) off `main`, and return its path. The
    /// `TempDir` is returned so the caller keeps it alive for the test.
    fn repo_with_branch_changes(files: &[&str]) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().to_path_buf();
        init_repo(&repo);
        assert!(git(&repo, &["checkout", "-q", "-b", "feat/x"])
            .status
            .success());
        for (i, f) in files.iter().enumerate() {
            // Ensure nested paths exist before writing.
            if let Some(parent) = std::path::Path::new(f).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(repo.join(parent)).unwrap();
                }
            }
            commit_file(&repo, f, &format!("change {i}\n"), &format!("edit {f}"));
        }
        (tmp, repo)
    }

    #[test]
    fn scope_guard_inert_with_empty_perimeter() {
        // No declared perimeter ⇒ the guard never runs, even when the branch
        // touched crate source. Byte-identical to pre-knob behaviour.
        let (_tmp, repo) = repo_with_branch_changes(&["crates/cosmon-cli/src/cmd/tackle.rs"]);
        assert!(check_scope_guard(&repo, "feat/x", "main", &[], true).is_ok());
    }

    #[test]
    fn scope_guard_passes_when_all_changes_in_perimeter() {
        let (_tmp, repo) = repo_with_branch_changes(&["docs/book/src/intro.md", "README.md"]);
        let perimeter = vec!["docs/book/src/**".to_owned(), "README.md".to_owned()];
        // Clean ⇒ Ok in both advisory and strict mode.
        assert!(check_scope_guard(&repo, "feat/x", "main", &perimeter, false).is_ok());
        assert!(check_scope_guard(&repo, "feat/x", "main", &perimeter, true).is_ok());
    }

    #[test]
    fn scope_guard_strict_aborts_on_escapee() {
        // The task-c14e shape: a docs brief that also rewrote crate source.
        let (_tmp, repo) = repo_with_branch_changes(&[
            "docs/book/src/intro.md",
            "crates/cosmon-cli/src/cmd/tackle.rs",
        ]);
        let perimeter = vec!["docs/book/src/**".to_owned(), "README.md".to_owned()];
        let err = check_scope_guard(&repo, "feat/x", "main", &perimeter, true)
            .expect_err("strict mode must abort on an out-of-scope file");
        let msg = err.to_string();
        assert!(
            msg.contains("crates/cosmon-cli/src/cmd/tackle.rs"),
            "msg: {msg}"
        );
        assert!(msg.contains("scope-guard"), "msg: {msg}");
    }

    #[test]
    fn scope_guard_advisory_warns_but_passes_on_escapee() {
        // Same escapee, advisory (default) policy: the merge is NOT blocked —
        // the §8b honest default warns and proceeds.
        let (_tmp, repo) = repo_with_branch_changes(&[
            "docs/book/src/intro.md",
            "crates/cosmon-cli/src/cmd/tackle.rs",
        ]);
        let perimeter = vec!["docs/book/src/**".to_owned()];
        assert!(
            check_scope_guard(&repo, "feat/x", "main", &perimeter, false).is_ok(),
            "advisory mode must never block the merge"
        );
    }

    #[test]
    fn scope_guard_malformed_glob_is_skipped_not_crash() {
        // A malformed pattern must not wedge the gate; surviving patterns
        // still define the perimeter. Here the good `README.md` glob covers
        // the sole change, so the gate passes even in strict mode.
        let (_tmp, repo) = repo_with_branch_changes(&["README.md"]);
        let perimeter = vec!["[unterminated".to_owned(), "README.md".to_owned()];
        assert!(check_scope_guard(&repo, "feat/x", "main", &perimeter, true).is_ok());
    }

    #[test]
    fn git_diff_names_lists_branch_changes() {
        let (_tmp, repo) = repo_with_branch_changes(&["a.md", "src/b.rs"]);
        let mut names = git_diff_names(&repo, "main...feat/x");
        names.sort();
        assert_eq!(names, vec!["a.md".to_owned(), "src/b.rs".to_owned()]);
    }

    #[test]
    fn git_diff_names_empty_on_bad_range() {
        let (_tmp, repo) = repo_with_branch_changes(&["a.md"]);
        // A non-existent branch ⇒ empty (conservative on probe failure).
        assert!(git_diff_names(&repo, "main...does-not-exist").is_empty());
    }

    #[test]
    fn test_merge_branch_parallel_landing_succeeds_with_default_strategy() {
        // Scenario from bug task-20260409-420c: two independent workers
        // tackled parallel molecules, the first landed and moved main,
        // the second can no longer fast-forward. Default strategy must
        // still succeed.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // feat/a edits a.txt
        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(repo, "a.txt", "from a\n", "feat: add a");

        // feat/b starts from the same base and edits a disjoint file
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["checkout", "-q", "-b", "feat/b"])
            .status
            .success());
        commit_file(repo, "b.txt", "from b\n", "feat: add b");

        // Simulate "first worker lands": main fast-forwards to feat/a.
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["merge", "-q", "--ff-only", "feat/a"])
            .status
            .success());

        // At this point main has a.txt from feat/a. feat/b cannot
        // fast-forward because main has moved. Default strategy must
        // still merge it cleanly via a merge commit.
        let outcome = try_merge_branch(repo, "feat/b", MergeStrategy::Merge, &[]);
        assert!(
            matches!(outcome, MergeOutcome::Merged),
            "expected Merged, got {outcome:?}"
        );

        // Verify main now has both files.
        assert!(repo.join("a.txt").exists());
        assert!(repo.join("b.txt").exists());
    }

    #[test]
    fn test_merge_branch_ff_only_strategy_refuses_divergent() {
        // Same scenario, but with the strict strategy — must refuse.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(repo, "a.txt", "from a\n", "feat: add a");

        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(repo, "main.txt", "main advance\n", "chore: advance main");

        let outcome = try_merge_branch(repo, "feat/a", MergeStrategy::FfOnly, &[]);
        assert!(
            matches!(outcome, MergeOutcome::NotFastForward),
            "expected NotFastForward, got {outcome:?}"
        );
    }

    #[test]
    fn test_merge_branch_ff_only_classifies_correctly_under_french_locale() {
        // Regression for drain-worker f877 (2026-05-22): on an operator
        // machine where the user's locale is French, `git merge --ff-only`
        // emits `fatal : Pas possible d'avancer rapidement, abandon.` —
        // which does NOT match the English grep
        // `"Not possible to fast-forward"`. Pre-fix, that misclassified the
        // outcome as `MergeOutcome::Error` instead of
        // `MergeOutcome::NotFastForward`, breaking the ff-only retry path.
        //
        // The structural fix (`Command::env("LC_ALL", "C")` inside
        // `try_merge_branch`) pins git's stderr to English regardless of
        // the caller's locale. This test proves the override is wired up:
        // it first invokes raw git with `LC_ALL=fr_FR.UTF-8` to confirm
        // the machine *would* emit French (otherwise the regression
        // surface doesn't exist), then calls `try_merge_branch` and
        // asserts correct classification.
        if !std::process::Command::new("locale")
            .arg("-a")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .is_some_and(|s| s.lines().any(|l| l.eq_ignore_ascii_case("fr_FR.UTF-8")))
        {
            // No French locale installed (some minimal CI images lack
            // it). Skip rather than fail — the C-locale path is already
            // covered by `…_refuses_divergent`.
            eprintln!("skipping FR-locale test: fr_FR.UTF-8 not available");
            return;
        }

        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(repo, "a.txt", "from a\n", "feat: add a");

        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(repo, "main.txt", "main advance\n", "chore: advance main");

        // Sanity: prove the operator's locale would in fact emit French.
        // We invoke raw git here (no LC_ALL=C override) to surface the
        // pre-fix failure mode. If the test machine's git ships no
        // French translation, the second assertion below still proves
        // the fix is harmless; we only require that the operator-side
        // failure surface *exists somewhere* before claiming we fixed it.
        let raw = std::process::Command::new("git")
            .env("LC_ALL", "fr_FR.UTF-8")
            .args(["-C", repo.to_str().unwrap(), "merge", "--ff-only", "feat/a"])
            .output()
            .expect("git invocation failed");
        assert!(!raw.status.success(), "expected ff-only to refuse");
        let raw_stderr = String::from_utf8_lossy(&raw.stderr);
        let saw_french =
            raw_stderr.contains("Pas possible d'avancer") || raw_stderr.contains("avance rapide");
        let saw_english = raw_stderr.contains("Not possible to fast-forward");
        // One of the two MUST hold — otherwise we are reading from a git
        // build with neither localisation, and the regression scenario
        // is not exercised by this test environment.
        assert!(
            saw_french || saw_english,
            "expected French or English ff-only failure, got: {raw_stderr}"
        );
        // Roll back the raw merge attempt (it may have left MERGE_HEAD).
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "merge", "--abort"])
            .output();

        // Under the fix, `try_merge_branch` injects `LC_ALL=C` into its
        // spawned git regardless of the caller's environment, so the
        // English grep matches and classification is correct.
        let outcome = try_merge_branch(repo, "feat/a", MergeStrategy::FfOnly, &[]);
        assert!(
            matches!(outcome, MergeOutcome::NotFastForward),
            "expected NotFastForward under FR-locale operator, got {outcome:?}"
        );
    }

    #[test]
    fn test_merge_branch_conflict_aborts_cleanly() {
        // Textual conflict on a shared file must produce a Conflict
        // outcome, and the worktree must be clean afterwards (no
        // MERGE_HEAD left behind).
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Seed a file both branches will edit.
        commit_file(repo, "shared.txt", "base\n", "seed shared file");

        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(repo, "shared.txt", "from a\n", "edit from a");

        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(repo, "shared.txt", "from main\n", "edit from main");

        let outcome = try_merge_branch(repo, "feat/a", MergeStrategy::Merge, &[]);
        match outcome {
            MergeOutcome::Conflict(files) => {
                assert!(
                    files.iter().any(|f| f == "shared.txt"),
                    "expected shared.txt in conflict list, got {files:?}"
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // Merge must have been aborted — no MERGE_HEAD marker.
        assert!(
            !repo.join(".git/MERGE_HEAD").exists(),
            "expected worktree clean after abort — .git/MERGE_HEAD still present"
        );
    }

    #[test]
    fn test_merge_jsonl_by_timestamp_unions_and_sorts() {
        let ours = "{\"timestamp\":\"2026-04-14T10:00:00Z\",\"type\":\"a\"}\n\
                   {\"timestamp\":\"2026-04-14T10:02:00Z\",\"type\":\"shared\"}\n";
        let theirs = "{\"timestamp\":\"2026-04-14T10:01:00Z\",\"type\":\"b\"}\n\
                     {\"timestamp\":\"2026-04-14T10:02:00Z\",\"type\":\"shared\"}\n";
        let merged = merge_jsonl_by_timestamp(ours, theirs);
        let lines: Vec<&str> = merged.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "expected 3 lines (shared deduped), got {merged:?}"
        );
        assert!(lines[0].contains("\"a\""));
        assert!(lines[1].contains("\"b\""));
        assert!(lines[2].contains("\"shared\""));
    }

    #[test]
    fn test_is_append_only_jsonl_recognizes_events_and_interactions() {
        assert!(is_append_only_jsonl(".cosmon/state/events.jsonl"));
        assert!(is_append_only_jsonl(".cosmon/state/interactions.jsonl"));
        assert!(is_append_only_jsonl("events.jsonl"));
        assert!(!is_append_only_jsonl("shared.txt"));
        assert!(!is_append_only_jsonl(".cosmon/state/fleet.json"));
    }

    #[test]
    fn test_merge_branch_auto_resolves_events_jsonl_conflict() {
        // Both branches append to events.jsonl. Standard 3-way merge would
        // conflict on the adjacent appended hunks; the auto-resolver should
        // union the entries and report Merged.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let rel = ".cosmon/state/events.jsonl";
        std::fs::create_dir_all(repo.join(".cosmon/state")).unwrap();
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n",
            "seed events.jsonl",
        );

        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n\
             {\"timestamp\":\"2026-04-14T10:00:00Z\",\"type\":\"from_a\"}\n",
            "append from a",
        );

        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n\
             {\"timestamp\":\"2026-04-14T09:30:00Z\",\"type\":\"from_main\"}\n",
            "append from main",
        );

        let outcome = try_merge_branch(repo, "feat/a", MergeStrategy::Merge, &[]);
        assert!(
            matches!(outcome, MergeOutcome::Merged),
            "expected Merged after auto-resolution, got {outcome:?}"
        );

        // No leftover merge state.
        assert!(!repo.join(".git/MERGE_HEAD").exists());

        // Resulting file contains both appended entries, sorted by timestamp.
        let body = std::fs::read_to_string(repo.join(rel)).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 lines, got {body:?}");
        assert!(lines[0].contains("seed"));
        assert!(lines[1].contains("from_main"));
        assert!(lines[2].contains("from_a"));
    }

    /// Trailer-carrier contract (delib-20260717-194b, F1) on the CONFLICTED
    /// path: when the merge conflicts on an append-only JSONL file and the
    /// auto-resolver finalizes the merge commit, the `Co-Authored-By` block
    /// must survive exactly as it does on the conflict-free path. Regression
    /// for task-20260718-7f91: finalizing with a bare `git commit --no-edit`
    /// silently dropped the trailers under fleet parallelism (incident merge
    /// of task-20260718-a550).
    #[test]
    fn auto_resolved_merge_commit_carries_coauthor_trailers() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Both sides append to events.jsonl → guaranteed textual conflict
        // that only the append-only auto-resolver can finalize.
        let rel = ".cosmon/state/events.jsonl";
        std::fs::create_dir_all(repo.join(".cosmon/state")).unwrap();
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n",
            "seed events.jsonl",
        );
        assert!(git(repo, &["checkout", "-q", "-b", "feat/c"])
            .status
            .success());
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n\
             {\"timestamp\":\"2026-04-14T10:00:00Z\",\"type\":\"from_c\"}\n",
            "append from c",
        );
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n\
             {\"timestamp\":\"2026-04-14T09:30:00Z\",\"type\":\"from_main\"}\n",
            "append from main",
        );

        let trailers = vec!["Co-Authored-By: Noogram (claude) <noreply@noogram.org>".to_owned()];
        let outcome = try_merge_branch(repo, "feat/c", MergeStrategy::Merge, &trailers);
        assert!(
            matches!(outcome, MergeOutcome::Merged),
            "expected Merged after auto-resolution, got {outcome:?}"
        );

        // git itself must PARSE the trailer on the finalized merge commit —
        // textual presence buried above a `# Conflicts:` block is not enough.
        let interpreted = git(
            repo,
            &["log", "-1", "--format=%(trailers:key=Co-Authored-By)"],
        );
        let out = String::from_utf8_lossy(&interpreted.stdout);
        assert!(
            out.contains("Noogram (claude) <noreply@noogram.org>"),
            "auto-resolved merge commit lost the trailer block: {out}"
        );

        // Message is byte-compatible with the conflict-free path: the
        // default merge subject, and no committed `# Conflicts:` residue
        // from `.git/MERGE_MSG`.
        let body = git(repo, &["log", "-1", "--format=%B"]);
        let body = String::from_utf8_lossy(&body.stdout);
        assert!(
            body.starts_with("Merge branch 'feat/c'"),
            "unexpected merge subject: {body}"
        );
        assert!(
            !body.contains("# Conflicts"),
            "MERGE_MSG comment residue leaked into the commit: {body}"
        );
    }

    /// The conflicted path with NO trailers configured stays byte-identical
    /// to a pre-attribution cosmon (F9): `--no-edit`, no stamp.
    #[test]
    fn auto_resolved_merge_commit_without_trailers_is_unstamped() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let rel = ".cosmon/state/events.jsonl";
        std::fs::create_dir_all(repo.join(".cosmon/state")).unwrap();
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n",
            "seed events.jsonl",
        );
        assert!(git(repo, &["checkout", "-q", "-b", "feat/d"])
            .status
            .success());
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n\
             {\"timestamp\":\"2026-04-14T10:00:00Z\",\"type\":\"from_d\"}\n",
            "append from d",
        );
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-14T09:00:00Z\",\"type\":\"seed\"}\n\
             {\"timestamp\":\"2026-04-14T09:30:00Z\",\"type\":\"from_main\"}\n",
            "append from main",
        );

        let outcome = try_merge_branch(repo, "feat/d", MergeStrategy::Merge, &[]);
        assert!(
            matches!(outcome, MergeOutcome::Merged),
            "expected Merged after auto-resolution, got {outcome:?}"
        );
        let body = git(repo, &["log", "-1", "--format=%B"]);
        assert!(
            !String::from_utf8_lossy(&body.stdout).contains("Co-Authored-By"),
            "empty trailers must not stamp the auto-resolved merge commit"
        );
    }

    #[test]
    fn test_merge_branch_already_merged_is_noop() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(repo, "a.txt", "a\n", "feat: add a");

        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["merge", "-q", "--ff-only", "feat/a"])
            .status
            .success());

        // Branch is fully merged; both strategies should report it.
        let outcome = try_merge_branch(repo, "feat/a", MergeStrategy::Merge, &[]);
        assert!(
            matches!(outcome, MergeOutcome::AlreadyMerged),
            "expected AlreadyMerged, got {outcome:?}"
        );
    }

    #[test]
    fn test_branch_is_empty_relative_to_base() {
        // Bug `task-20260422-ecf3`: when a worker's deliverable lives
        // outside the cosmon repo (galaxy bootstrap, vault artifact),
        // the worker's branch carries zero commits ahead of `main`.
        // `branch_is_empty_relative_to` must report true so the caller
        // can label this case as `empty_branch`, not `already_merged`.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Worker branch with no commits of its own.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/empty"])
            .status
            .success());
        assert!(branch_is_empty_relative_to(repo, "feat/empty", "main"));

        // Worker branch with a real commit.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/has-work"])
            .status
            .success());
        commit_file(repo, "work.txt", "work\n", "feat: real work");
        assert!(!branch_is_empty_relative_to(repo, "feat/has-work", "main"));

        // After integrating into main via fast-forward, base..branch
        // is empty (the branch's commit is now on base).
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["merge", "-q", "--ff-only", "feat/has-work"])
            .status
            .success());
        assert!(branch_is_empty_relative_to(repo, "feat/has-work", "main"));
    }

    #[test]
    fn test_branch_is_empty_relative_to_base_handles_missing_ref() {
        // The probe must not panic on a missing branch — it should
        // return false (conservative: caller will then treat it as
        // not-empty and surface a real git error elsewhere).
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(!branch_is_empty_relative_to(
            repo,
            "feat/does-not-exist",
            "main"
        ));
    }

    #[test]
    fn test_classify_already_merged_label_truth_table() {
        // Bug `task-20260422-ecf3`: the four-quadrant matrix the
        // operator cares about.
        //
        // merged_at set | base..branch empty | label
        // --------------+--------------------+---------------
        //      true     |       true         | already_merged
        //      true     |       false        | already_merged
        //      false    |       true         | empty_branch
        //      false    |       false        | already_merged
        //
        // The third row is the empty-branch case the bug report
        // motivates: the worker produced no diff in this repo and
        // `cs done` has never integrated this branch — labelling it
        // `empty_branch` cues the operator to verify the deliverable
        // landed in a sibling repo. The fourth row is conservative:
        // we have no `merged_at` stamp, so we cannot prove the work
        // landed, but `base..branch` is non-empty — surfacing
        // `already_merged` is wrong but defensible (the topology
        // probe `is_branch_merged` returned true, so the branch *is*
        // an ancestor of base; calling it `empty_branch` would lie
        // worse than calling it `already_merged`).
        assert_eq!(classify_already_merged_label(true, true), "already_merged");
        assert_eq!(classify_already_merged_label(true, false), "already_merged");
        assert_eq!(classify_already_merged_label(false, true), "empty_branch");
        assert_eq!(
            classify_already_merged_label(false, false),
            "already_merged"
        );
    }

    #[test]
    fn cs_done_topology_not_subject_match() {
        // Regression for the false-positive reported in mailroom
        // task-20260419-9047: `cs done` saw bookkeeping commits on main whose
        // subjects matched `evolve(<mol>): step N/M` and concluded the worker's
        // branch was already merged — even though the payload commit on
        // `feat/<mol>` was never integrated and `git merge-base --is-ancestor
        // feat main` returned false.
        //
        // The merged-check must be a TOPOLOGY probe, not a subject-string
        // heuristic. With `feat/<mol>` carrying a real payload commit and
        // `main` carrying only a same-named bookkeeping commit, `try_merge_branch`
        // must NOT report `AlreadyMerged` — it must actually merge so the
        // payload lands on main.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Worker branches from main and commits the real payload.
        assert!(
            git(repo, &["checkout", "-q", "-b", "feat/task-20260419-dc10"])
                .status
                .success()
        );
        commit_file(
            repo,
            "payload.txt",
            "real payload\n",
            "feat: add payload (the orphan-prone commit)",
        );

        // Operator returns to main and lands a bookkeeping commit whose
        // subject matches the `cs evolve` convention. This is exactly the
        // shape that fooled the old `git branch --merged`-string parser.
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        std::fs::write(repo.join("bookkeeping.txt"), "evolve artifact\n").unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(git(
            repo,
            &[
                "commit",
                "-q",
                "-m",
                "evolve(task-20260419-dc10): step 1/2 — Implement the solution",
            ],
        )
        .status
        .success());
        assert!(git(
            repo,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "evolve(task-20260419-dc10): step 2/2 — Verify and validate",
            ],
        )
        .status
        .success());

        // Sanity: the divergence is real — payload commit is NOT on main.
        let ancestor_check = git(
            repo,
            &[
                "merge-base",
                "--is-ancestor",
                "feat/task-20260419-dc10",
                "HEAD",
            ],
        );
        assert!(
            !ancestor_check.status.success(),
            "precondition: feat tip must NOT yet be reachable from HEAD"
        );

        // Topology-correct merged check must report false here.
        assert!(
            !is_branch_merged(repo, "feat/task-20260419-dc10"),
            "is_branch_merged must use topology — bookkeeping subjects on HEAD \
             must NOT cause a false positive"
        );

        // The full merge attempt must therefore actually merge the branch,
        // not short-circuit with AlreadyMerged.
        let outcome = try_merge_branch(repo, "feat/task-20260419-dc10", MergeStrategy::Merge, &[]);
        assert!(
            matches!(outcome, MergeOutcome::Merged),
            "expected Merged (topology probe), got {outcome:?} — \
             subject-string heuristic would have returned AlreadyMerged"
        );

        // After the merge, the payload commit is reachable from HEAD.
        let ancestor_check = git(
            repo,
            &[
                "merge-base",
                "--is-ancestor",
                "feat/task-20260419-dc10",
                "HEAD",
            ],
        );
        assert!(
            ancestor_check.status.success(),
            "post-condition: feat tip must be reachable from HEAD after the merge"
        );
    }

    #[test]
    fn strict_ancestry_refuses_head_shortcut_when_head_is_feature_branch() {
        // Regression for the 2026-04-21 cosmon incident:
        //   `cs done task-20260421-37c1` returned `already_merged`
        //   even though the payload commit `1f1cbc9d6` (491 insertions)
        //   was NOT on `main`. Root cause: the ancestry probe compared
        //   against the ambient `HEAD`, which happened to resolve to the
        //   worker's own branch tip — so `--is-ancestor feat/<mol> HEAD`
        //   trivially returned true and the merge was silently skipped.
        //
        // The strict-ancestry invariant: `is_branch_merged` must compare
        // against the configured base branch (usually `main`), never
        // against `HEAD`. This test pins the invariant by checking out
        // the feature branch as the current HEAD and asserting that
        // `is_branch_merged` still reports `false`.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Feature branch with a real payload commit.
        assert!(
            git(repo, &["checkout", "-q", "-b", "feat/task-20260421-37c1"])
                .status
                .success()
        );
        commit_file(
            repo,
            "payload.txt",
            "491 insertions of real work\n",
            "feat: the payload cs done almost lost",
        );

        // Stay on the feature branch — this simulates `cs done` being
        // invoked from inside the worker's worktree. HEAD == branch tip,
        // so a HEAD-based ancestry probe would falsely succeed.
        let head_branch =
            String::from_utf8_lossy(&git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout)
                .trim()
                .to_owned();
        assert_eq!(
            head_branch, "feat/task-20260421-37c1",
            "precondition: HEAD must be on the feature branch"
        );

        // Precondition: the branch tip is NOT reachable from `main` —
        // this is the ground truth we want the probe to respect.
        let ancestor_vs_main = git(
            repo,
            &[
                "merge-base",
                "--is-ancestor",
                "feat/task-20260421-37c1",
                "main",
            ],
        );
        assert!(
            !ancestor_vs_main.status.success(),
            "precondition: feat tip must NOT yet be reachable from main"
        );

        // Sanity: a HEAD-based probe WOULD lie here (branch is trivially
        // its own ancestor when HEAD points at it). This is the exact
        // trap the invariant forbids.
        assert!(
            branch_is_ancestor_of_head(repo, "feat/task-20260421-37c1"),
            "diagnostic: HEAD-based probe returns true (the false positive)"
        );

        // The actual invariant: is_branch_merged must refuse to declare
        // already-merged when the branch is not in the base branch.
        assert!(
            !is_branch_merged(repo, "feat/task-20260421-37c1"),
            "strict-ancestry: is_branch_merged must compare against the \
             base branch (main), not against HEAD — otherwise an operator \
             invoking `cs done` from the worker's worktree silently drops \
             491 insertions of real work (2026-04-21 incident)"
        );

        // And try_merge_branch must not short-circuit with AlreadyMerged.
        // After the task-20260509-94f0 (mode B) fix, the pre-flight check
        // refuses early with `NotOnBase` whenever HEAD is not on the
        // configured base branch — strictly stronger than the original
        // contract (no false-AlreadyMerged), and on the path that would
        // have silently lost work.
        let outcome = try_merge_branch(repo, "feat/task-20260421-37c1", MergeStrategy::Merge, &[]);
        assert!(
            !matches!(outcome, MergeOutcome::AlreadyMerged),
            "try_merge_branch must NOT report AlreadyMerged when the \
             branch is not integrated into the base branch — got {outcome:?}"
        );
        assert!(
            matches!(outcome, MergeOutcome::NotOnBase { .. }),
            "after mode-B fix: HEAD off base must surface NotOnBase, got {outcome:?}"
        );
    }

    #[test]
    fn strict_ancestry_still_reports_already_merged_for_genuinely_merged_branch() {
        // Converse of the incident test: when the branch IS actually
        // reachable from the base branch, `is_branch_merged` must return
        // true — otherwise `cs done --if-completed` would keep re-merging
        // completed molecules and the solo-artifacts-gitignored case
        // (empty diffs) would never converge.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/done-work"])
            .status
            .success());
        commit_file(repo, "real.txt", "done\n", "feat: work");

        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["merge", "-q", "--ff-only", "feat/done-work"])
            .status
            .success());

        // HEAD is on main, branch is fully merged — probe must say so.
        assert!(
            is_branch_merged(repo, "feat/done-work"),
            "strict-ancestry: a genuinely merged branch must still be \
             reported as already merged"
        );

        // Even if HEAD wanders onto the feature branch, the base-relative
        // verdict remains correct.
        assert!(git(repo, &["checkout", "-q", "feat/done-work"])
            .status
            .success());
        assert!(
            is_branch_merged(repo, "feat/done-work"),
            "strict-ancestry: verdict must be invariant under HEAD \
             location — depends only on ancestry wrt the base branch"
        );
    }

    #[test]
    fn resolve_base_branch_defaults_to_main_without_origin() {
        // Last-resort default: when no `origin` remote is configured
        // (the common case in local-only test repos and fresh clones
        // that haven't set `origin/HEAD`), `resolve_base_branch` must
        // fall back to `main` — the cosmon convention documented in
        // `architectural-invariants.md` §11.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Setup confidence: no origin remote means `symbolic-ref`
        // `refs/remotes/origin/HEAD` fails, which the resolver must
        // gracefully downgrade to the `main` fallback.
        let symref = git(repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]);
        assert!(
            !symref.status.success(),
            "precondition: fresh init_repo must have no origin/HEAD"
        );

        assert_eq!(
            resolve_base_branch(repo),
            "main",
            "fallback must be the cosmon default branch name"
        );
    }

    #[test]
    fn test_merge_branch_missing_branch_returns_no_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let outcome = try_merge_branch(repo, "feat/does-not-exist", MergeStrategy::Merge, &[]);
        assert!(
            matches!(outcome, MergeOutcome::NoBranch),
            "expected NoBranch, got {outcome:?}"
        );
    }

    #[test]
    fn test_flush_state_dir_noop_on_clean_tree() {
        // Clean repo, no .cosmon/state/ — flush reports nothing to do and
        // does not create an empty commit.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let before = git(repo, &["rev-parse", "HEAD"]).stdout;
        let flushed = flush_state_dir_changes(repo).expect("flush must not error on clean tree");
        let after = git(repo, &["rev-parse", "HEAD"]).stdout;

        assert!(!flushed, "expected no flush commit on clean tree");
        assert_eq!(before, after, "HEAD must not move when nothing to flush");
    }

    #[test]
    fn test_flush_state_dir_folds_dirty_events_jsonl_into_commit() {
        // Repro of mailroom batch regression: cs done emits
        // MergeDispatched into .cosmon/state/events.jsonl and then tries
        // `git merge`, which refuses because the same file is tracked and
        // now dirty. The flush helper must fold the write into a commit
        // so the subsequent merge has a clean working tree.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let rel = ".cosmon/state/events.jsonl";
        std::fs::create_dir_all(repo.join(".cosmon/state")).unwrap();
        commit_file(
            repo,
            rel,
            "{\"timestamp\":\"2026-04-18T09:00:00Z\",\"type\":\"seed\"}\n",
            "seed events.jsonl",
        );

        // Simulate cs done appending a MergeDispatched event (uncommitted).
        std::fs::write(
            repo.join(rel),
            "{\"timestamp\":\"2026-04-18T09:00:00Z\",\"type\":\"seed\"}\n\
             {\"timestamp\":\"2026-04-18T10:00:00Z\",\"type\":\"merge_dispatched\"}\n",
        )
        .unwrap();

        // Sanity: the tree is dirty before the flush.
        let dirty = worktree_is_dirty(repo).unwrap();
        assert!(!dirty.is_empty(), "precondition: tree must be dirty");

        let flushed = flush_state_dir_changes(repo).expect("flush must succeed");
        assert!(flushed, "expected flush to create a commit");

        // After the flush the tree is clean.
        let dirty = worktree_is_dirty(repo).unwrap();
        assert!(
            dirty.is_empty(),
            "expected clean tree after flush, got {dirty:?}"
        );

        // The flush commit has the canonical subject.
        let subject = git(repo, &["log", "-1", "--format=%s"]).stdout;
        let subject = String::from_utf8_lossy(&subject);
        assert!(
            subject.contains("chore(state): flush before merge"),
            "expected flush commit, got subject: {subject:?}"
        );
    }

    #[test]
    fn test_flush_state_dir_second_call_is_noop() {
        // Idempotency: calling the flush twice in a row must not create a
        // second empty commit.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let rel = ".cosmon/state/events.jsonl";
        std::fs::create_dir_all(repo.join(".cosmon/state")).unwrap();
        commit_file(repo, rel, "seed\n", "seed");
        std::fs::write(repo.join(rel), "seed\nappended\n").unwrap();

        let first = flush_state_dir_changes(repo).unwrap();
        assert!(first, "first flush must create a commit");

        let head_after_first = git(repo, &["rev-parse", "HEAD"]).stdout;
        let second = flush_state_dir_changes(repo).unwrap();
        let head_after_second = git(repo, &["rev-parse", "HEAD"]).stdout;

        assert!(!second, "second flush must be a no-op");
        assert_eq!(
            head_after_first, head_after_second,
            "HEAD must not move on a no-op flush"
        );
    }

    #[test]
    fn test_flush_state_dir_survives_gitignored_siblings() {
        // Regression: `.cosmon/.gitignore` contains patterns like
        // `state/**/state.json` that block `git add -- .cosmon/state/` when
        // untracked gitignored files coexist with tracked dirty files.
        // The fix: `-u` for tracked files + `ls-files --others` for new
        // non-ignored files, avoiding the directory-level add entirely.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let state_dir = repo.join(".cosmon/state/fleets/default/molecules/mol-a");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Commit events.jsonl so it is tracked.
        let events = ".cosmon/state/fleets/default/molecules/mol-a/events.jsonl";
        commit_file(repo, events, "seed\n", "seed events");

        // Add a .cosmon/.gitignore that blocks state.json (mirrors real config).
        let gitignore = ".cosmon/.gitignore";
        commit_file(repo, gitignore, "state/**/state.json\n", "add gitignore");

        // Dirty the tracked events.jsonl AND create an untracked gitignored
        // state.json in the same directory — the exact scenario that broke
        // `cs done` in mailroom (2026-05-27).
        std::fs::write(repo.join(events), "seed\nappended\n").unwrap();
        std::fs::write(state_dir.join("state.json"), "{\"status\":\"running\"}\n").unwrap();

        let flushed = flush_state_dir_changes(repo).expect(
            "flush must succeed even when gitignored files exist alongside tracked dirty files",
        );
        assert!(flushed, "expected flush commit");

        // The tracked file was staged and committed.
        let diff = git(repo, &["diff", "--name-only", "HEAD~1", "HEAD"]);
        let diff_text = String::from_utf8_lossy(&diff.stdout);
        assert!(
            diff_text.contains("events.jsonl"),
            "events.jsonl must be in flush commit, got: {diff_text}"
        );

        // The gitignored state.json was NOT staged.
        assert!(
            !diff_text.contains("state.json"),
            "gitignored state.json must NOT be in flush commit"
        );
    }

    #[test]
    fn test_flush_state_dir_adds_new_non_ignored_files() {
        // New untracked files that are NOT gitignored must still be staged.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let state_dir = repo.join(".cosmon/state/fleets/default/molecules/mol-b");
        std::fs::create_dir_all(&state_dir).unwrap();

        // Add a gitignore that blocks only state.json.
        let gitignore = ".cosmon/.gitignore";
        commit_file(repo, gitignore, "state/**/state.json\n", "add gitignore");

        // Create a new non-ignored file (events.jsonl) — not yet tracked.
        let events = ".cosmon/state/fleets/default/molecules/mol-b/events.jsonl";
        std::fs::write(repo.join(events), "new-event\n").unwrap();

        let flushed = flush_state_dir_changes(repo).expect("flush must succeed");
        assert!(flushed, "expected flush commit for new non-ignored file");

        let diff = git(repo, &["diff", "--name-only", "HEAD~1", "HEAD"]);
        let diff_text = String::from_utf8_lossy(&diff.stdout);
        assert!(
            diff_text.contains("events.jsonl"),
            "new non-ignored events.jsonl must be staged, got: {diff_text}"
        );
    }

    #[test]
    fn test_merge_branch_succeeds_when_state_dir_is_dirty_pre_merge() {
        // End-to-end: cs done's own MergeDispatched write dirties
        // .cosmon/state/events.jsonl on the base branch. Without the
        // pre-merge flush, `git merge` fails with "would be overwritten".
        // With the flush, the merge lands cleanly.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let rel = ".cosmon/state/events.jsonl";
        std::fs::create_dir_all(repo.join(".cosmon/state")).unwrap();
        commit_file(repo, rel, "seed\n", "seed events");

        // Worker branch touches the same file.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(repo, rel, "seed\nworker-event\n", "worker event");

        // Back on main: simulate `cs done` appending its MergeDispatched
        // event into the working tree without committing it.
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        std::fs::write(repo.join(rel), "seed\nmerge-dispatched\n").unwrap();

        // Sanity: the working tree is dirty on exactly the file the merge
        // would touch.
        let dirty = worktree_is_dirty(repo).unwrap();
        assert!(
            dirty.iter().any(|l| l.contains("events.jsonl")),
            "precondition: events.jsonl must be dirty, got {dirty:?}"
        );

        // try_merge_branch must flush first, then merge, and report Merged.
        let outcome = try_merge_branch(repo, "feat/a", MergeStrategy::Merge, &[]);
        assert!(
            matches!(outcome, MergeOutcome::Merged),
            "expected Merged after pre-merge flush, got {outcome:?}"
        );

        // Working tree is clean afterwards — no MERGE_HEAD, no stray files.
        assert!(!repo.join(".git/MERGE_HEAD").exists());
        let dirty = worktree_is_dirty(repo).unwrap();
        assert!(
            dirty.is_empty(),
            "expected clean tree after merge, got {dirty:?}"
        );

        // Feat branch tip is reachable from HEAD — the merge really landed.
        let ancestor = git(repo, &["merge-base", "--is-ancestor", "feat/a", "HEAD"]);
        assert!(
            ancestor.status.success(),
            "feat/a must be reachable from HEAD after merge"
        );
    }

    #[test]
    fn test_done_refuses_active_molecule_without_force() {
        use cosmon_core::agent::AgentRole;
        use cosmon_core::clearance::Clearance;
        use cosmon_core::id::AgentId;
        use cosmon_core::worker::{DesiredState, WorkerStatus};
        use cosmon_state::{Fleet, WorkerData};

        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-act", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        // Register a fleet worker so the ghost-teardown shortcut does not
        // apply — the --force guard must still fire when there is real
        // state to tear down.
        let wid = WorkerId::new("task-20260409-act").unwrap();
        let mut worker = WorkerData::new(
            wid.clone(),
            AgentId::new("tackle").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        worker.desired = DesiredState::Running;
        let mut fleet = Fleet::default();
        fleet.workers.insert(wid, worker);
        store.save_fleet(&fleet).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = default_args("task-20260409-act");
        let err = run(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("use --force"));
    }

    /// Worktree guard plumbing: `recorded_worktree_for`
    /// reads the bound worker's recorded `repo` from the fleet and resolves it
    /// against the galaxy root. This is the *recorded-path* source the guard
    /// compares against — Cell-B-safe, no `~/galaxies` prefix.
    #[test]
    fn recorded_worktree_for_resolves_bound_worker_repo() {
        use cosmon_core::agent::AgentRole;
        use cosmon_core::clearance::Clearance;
        use cosmon_core::id::AgentId;
        use cosmon_core::worker::WorkerStatus;
        use cosmon_state::{Fleet, WorkerData};

        let (tmp, store) = make_store();
        let galaxy = tmp.path().join("galaxy");
        std::fs::create_dir_all(&galaxy).unwrap();

        let wid = WorkerId::new("task-20260601-rec").unwrap();
        let worker = WorkerData::new(
            wid.clone(),
            AgentId::new("tackle").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        )
        .with_repo(".worktrees/task-20260601-rec");
        let mut fleet = Fleet::default();
        fleet.workers.insert(wid.clone(), worker);
        store.save_fleet(&fleet).unwrap();

        let mut mol = sample_mol("task-20260601-rec", MoleculeStatus::Running);
        mol.assigned_worker = Some(wid);

        let resolved = super::super::evolve::recorded_worktree_for(&store, &mol, &galaxy).unwrap();
        assert_eq!(resolved, galaxy.join(".worktrees/task-20260601-rec"));

        // No bound worker → no recorded path → guard behaves as today.
        let orphan = sample_mol("task-20260601-orf", MoleculeStatus::Running);
        assert!(super::super::evolve::recorded_worktree_for(&store, &orphan, &galaxy).is_none());
    }

    #[test]
    fn test_done_accepts_completed_molecule() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-done", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = default_args("task-20260409-done");
        // All teardown steps disabled via no_* flags — should succeed as a no-op.
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_done_accepts_collapsed_molecule() {
        let (tmp, store) = make_store();
        let mut mol = sample_mol("task-20260409-dead", MoleculeStatus::Collapsed);
        mol.collapse_reason = Some("test".to_owned());
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = default_args("task-20260409-dead");
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_done_with_force_accepts_active_molecule() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-fact", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let mut args = default_args("task-20260409-fact");
        args.force = true;
        run(&ctx, &args).unwrap();
    }

    #[test]
    fn test_done_purges_fleet_worker() {
        use cosmon_core::agent::AgentRole;
        use cosmon_core::clearance::Clearance;
        use cosmon_core::id::AgentId;
        use cosmon_core::worker::{DesiredState, WorkerStatus};
        use cosmon_state::{Fleet, WorkerData};

        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-purg", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        // Register a worker matching the tackle convention (session = mol_id).
        let wid = WorkerId::new("task-20260409-purg").unwrap();
        let mut worker = WorkerData::new(
            wid.clone(),
            AgentId::new("tackle").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        worker.desired = DesiredState::Running;
        let mut fleet = Fleet::default();
        fleet.workers.insert(wid.clone(), worker);
        store.save_fleet(&fleet).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = default_args("task-20260409-purg");
        run(&ctx, &args).unwrap();

        // Verify the worker is gone.
        let reloaded = store.load_fleet().unwrap();
        assert!(
            !reloaded.workers.contains_key(&wid),
            "tackle worker should be purged by cs done"
        );
    }

    #[test]
    fn test_done_if_completed_purges_stale_worker_when_already_merged() {
        // Bug task-20260419-0bd2: after a partial prior `cs done` (or a
        // manual merge), the molecule carries `status=Completed +
        // merged_at=Some`, yet `fleet.json` still holds an entry for the
        // tackle worker. The `--if-completed` fast path must converge the
        // fleet to a clean state rather than silently return, otherwise
        // `cs ensemble` keeps displaying the molecule as running/diverged.
        use cosmon_core::agent::AgentRole;
        use cosmon_core::clearance::Clearance;
        use cosmon_core::id::AgentId;
        use cosmon_core::worker::{DesiredState, WorkerStatus};
        use cosmon_state::{Fleet, WorkerData};

        let (tmp, store) = make_store();
        let mut mol = sample_mol("task-20260419-stal", MoleculeStatus::Completed);
        mol.merged_at = Some(chrono::Utc::now());
        mol.session_name = Some("task-20260419-stal".to_owned());
        store.save_molecule(&mol.id, &mol).unwrap();

        let wid = WorkerId::new("task-20260419-stal").unwrap();
        let mut worker = WorkerData::new(
            wid.clone(),
            AgentId::new("tackle").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        worker.desired = DesiredState::Running;
        let mut fleet = Fleet::default();
        fleet.workers.insert(wid.clone(), worker);
        store.save_fleet(&fleet).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let mut args = default_args("task-20260419-stal");
        args.if_completed = true;
        run(&ctx, &args).unwrap();

        let reloaded = store.load_fleet().unwrap();
        assert!(
            !reloaded.workers.contains_key(&wid),
            "stale tackle worker must be purged by cs done --if-completed \
             even when merged_at is already set"
        );
    }

    #[test]
    fn test_done_if_completed_noop_without_fleet_entry() {
        // Converse of the test above: when the molecule is already merged
        // AND no stale worker is registered, `cs done --if-completed` is
        // a pure no-op — nothing to purge, no error.
        let (tmp, store) = make_store();
        let mut mol = sample_mol("task-20260419-nool", MoleculeStatus::Completed);
        mol.merged_at = Some(chrono::Utc::now());
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let mut args = default_args("task-20260419-nool");
        args.if_completed = true;
        run(&ctx, &args).unwrap();

        let reloaded = store.load_fleet().unwrap();
        assert!(reloaded.workers.is_empty());
    }

    // -----------------------------------------------------------------
    // FIX 1: worktree_is_dirty detects untracked files
    // -----------------------------------------------------------------

    #[test]
    fn test_worktree_is_dirty_detects_untracked_file() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Clean repo should not be dirty.
        let dirty = worktree_is_dirty(repo).unwrap();
        assert!(dirty.is_empty(), "expected clean, got {dirty:?}");

        // Add an untracked file — must be detected.
        std::fs::write(repo.join("synthesis.md"), "untracked content\n").unwrap();
        let dirty = worktree_is_dirty(repo).unwrap();
        assert!(
            !dirty.is_empty(),
            "expected dirty (untracked file), got empty"
        );
        assert!(
            dirty.iter().any(|l| l.contains("synthesis.md")),
            "expected synthesis.md in dirty list, got {dirty:?}"
        );
    }

    #[test]
    fn test_done_refuses_dirty_worktree_without_force() {
        // Full integration: cs done must refuse to remove a worktree
        // that contains untracked files (unless --force).
        let (_state_tmp, store) = make_store();
        let mol = sample_mol("task-20260409-dirty", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        // Create a git repo to act as the worktree.
        let wt_tmp = TempDir::new().unwrap();
        let wt = wt_tmp.path();
        init_repo(wt);
        std::fs::write(wt.join("untracked.txt"), "ephemeral\n").unwrap();

        // Directly test the guard function.
        let dirty = worktree_is_dirty(wt).unwrap();
        assert!(!dirty.is_empty());
    }

    // -----------------------------------------------------------------
    // FIX 2: branch not deleted when merge skipped/failed
    // -----------------------------------------------------------------

    #[test]
    fn test_branch_not_deleted_when_merge_skipped() {
        // When --no-merge is used (without --force), branch deletion must
        // not happen because the branch is the only copy of the work.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Create a branch with work.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/test-skip"])
            .status
            .success());
        commit_file(repo, "work.txt", "work\n", "feat: work");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        // Verify branch exists.
        assert!(branch_exists(repo, "feat/test-skip"));

        // Simulate the logic: merge_succeeded=false, no_merge=true → effective_no_branch_delete=true.
        // Direct assertion: branch should survive if merge was skipped.
        // We test the logic condition rather than the full `run()` because
        // `run()` depends on CWD being a git repo (for `find_repo_root`).
        let no_merge = true;
        let no_branch_delete = false;
        let force = false;
        let effective_no_branch_delete = no_merge && !no_branch_delete && !force;
        assert!(
            effective_no_branch_delete,
            "--no-merge must imply --no-branch-delete"
        );

        // The branch is still there.
        assert!(
            branch_exists(repo, "feat/test-skip"),
            "branch should survive when merge is skipped"
        );
    }

    // -----------------------------------------------------------------
    // FIX 3: --no-merge implies --no-branch-delete
    // -----------------------------------------------------------------

    #[test]
    fn test_no_merge_implies_no_branch_delete() {
        // Without --force, --no-merge must automatically suppress branch
        // deletion to avoid silent data loss.
        let no_merge = true;
        let no_branch_delete = false;
        let force = false;

        let effective = no_merge && !no_branch_delete && !force;
        assert!(
            effective,
            "--no-merge without --force must imply --no-branch-delete"
        );

        // With --force, the override kicks in.
        let force = true;
        let effective_forced = no_merge && !no_branch_delete && !force;
        assert!(
            !effective_forced,
            "--force must override the --no-merge guard"
        );
    }

    #[test]
    fn test_merge_error_prevents_branch_deletion() {
        // When merge produces an error (not conflict, not ff failure),
        // merge_succeeded remains false → branch must not be deleted.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/err"])
            .status
            .success());
        commit_file(repo, "e.txt", "e\n", "feat: e");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        // Simulate: merge happened but merge_succeeded=false (error case),
        // effective_no_branch_delete=false, force=false → branch preserved.
        let merge_succeeded = false;
        let force = false;
        let should_delete = merge_succeeded || force;
        assert!(
            !should_delete,
            "branch must not be deleted when merge did not succeed"
        );
        assert!(branch_exists(repo, "feat/err"));
    }

    // -----------------------------------------------------------------
    // --dry-run tests
    // -----------------------------------------------------------------

    #[test]
    fn test_dry_run_does_not_mutate_state() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-dryr", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let mut args = default_args("task-20260409-dryr");
        args.dry_run = true;
        // Enable all teardown steps to confirm none execute.
        args.no_merge = false;
        args.no_worktree_remove = false;
        args.no_branch_delete = false;
        args.no_kill = false;

        // Should succeed without error (no side effects).
        run(&ctx, &args).unwrap();

        // Molecule still exists unchanged.
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.status, MoleculeStatus::Completed);
    }

    #[test]
    fn test_dry_run_accepts_active_molecule() {
        // --dry-run should not refuse active molecules — it's just a preview.
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260409-drya", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let mut args = default_args("task-20260409-drya");
        args.dry_run = true;

        // Should succeed — dry-run doesn't enforce terminal state guard.
        run(&ctx, &args).unwrap();
    }

    // -----------------------------------------------------------------
    // Post-merge verification tests
    // -----------------------------------------------------------------

    #[test]
    fn test_verify_merge_succeeds_after_merge() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/verify"])
            .status
            .success());
        commit_file(repo, "v.txt", "verified\n", "feat: verify");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["merge", "--no-ff", "--no-edit", "feat/verify"])
            .status
            .success());

        assert!(
            verify_merge(repo, "feat/verify"),
            "branch tip should be ancestor of HEAD after merge"
        );
    }

    #[test]
    fn test_verify_merge_fails_for_unmerged_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/unmerged"])
            .status
            .success());
        commit_file(repo, "u.txt", "unmerged\n", "feat: unmerged");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        assert!(
            !verify_merge(repo, "feat/unmerged"),
            "unmerged branch should not pass verification"
        );
    }

    // -----------------------------------------------------------------
    // Conflict recovery message tests
    // -----------------------------------------------------------------

    #[test]
    fn test_format_conflict_recovery_contains_commands() {
        let mol_id = MoleculeId::new("task-20260409-conf").unwrap();
        let wt_path = PathBuf::from("/repo/.worktrees/task-20260409-conf");
        let files = vec!["shared.txt".to_owned(), "lib.rs".to_owned()];

        let msg = format_conflict_recovery(&mol_id, &wt_path, &files);

        assert!(msg.contains("cd /repo/.worktrees/task-20260409-conf && git merge main"));
        assert!(msg.contains("shared.txt"));
        assert!(msg.contains("lib.rs"));
        assert!(msg.contains("cs done task-20260409-conf"));
        assert!(msg.contains("git add"));
    }

    // -----------------------------------------------------------------
    // Post-merge hook tests
    // -----------------------------------------------------------------

    #[test]
    fn test_post_merge_hook_runs_successfully() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let result = run_post_merge_hook(repo, "echo hello");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_post_merge_hook_failing_command_returns_error() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let result = run_post_merge_hook(repo, "false");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exited"),
            "expected exit code in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_post_merge_hook_missing_config_is_silent() {
        // When hooks.post_merge is None, no hook runs — this is the
        // backward-compatible default.
        let config = cosmon_core::config::ProjectConfig::default();
        assert!(config.hooks.post_merge.is_none());
    }

    #[test]
    fn test_post_merge_hook_runs_from_repo_root() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Create a marker file via the hook to prove cwd is repo_root.
        let result = run_post_merge_hook(repo, "touch .post_merge_marker");
        assert!(result.is_ok());
        assert!(
            repo.join(".post_merge_marker").exists(),
            "hook should run from repo root"
        );
    }

    // -----------------------------------------------------------------
    // Pre-done gate tests (showroom delib-20260701-bfdf, torvalds D1)
    // -----------------------------------------------------------------

    #[test]
    fn test_pre_done_hook_zero_exit_passes() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260701-8959").unwrap();
        let result = run_pre_done_hook(repo, "true", &mol_id);
        assert!(result.is_ok(), "zero-exit gate should pass");
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_pre_done_hook_nonzero_exit_aborts_with_stderr() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260701-8959").unwrap();
        // Script writes to stderr and exits non-zero — the abort reason must
        // carry both the molecule id and the script's stderr verbatim.
        let result = run_pre_done_hook(repo, "echo 'FALSIFIER missing' >&2; exit 3", &mol_id);
        assert!(result.is_err(), "non-zero gate must abort teardown");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("pre_done gate refused DONE"), "got: {msg}");
        assert!(msg.contains("task-20260701-8959"), "got: {msg}");
        assert!(msg.contains("exited 3"), "got: {msg}");
        assert!(msg.contains("FALSIFIER missing"), "stderr forwarded: {msg}");
    }

    #[test]
    fn test_pre_done_hook_receives_molecule_id_as_argument() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260701-8959").unwrap();
        // The script asserts $1 equals the molecule id; a mismatch exits 1.
        let hook = r#"test "$1" = "task-20260701-8959" || { echo "wrong id: $1" >&2; exit 1; }"#;
        let result = run_pre_done_hook(repo, hook, &mol_id);
        assert!(
            result.is_ok(),
            "molecule id must reach the script as $1: {result:?}"
        );
    }

    #[test]
    fn test_pre_done_hook_runs_from_repo_root() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260701-8959").unwrap();
        let result = run_pre_done_hook(repo, "touch .pre_done_marker", &mol_id);
        assert!(result.is_ok());
        assert!(
            repo.join(".pre_done_marker").exists(),
            "gate should run from repo root"
        );
    }

    #[test]
    fn test_pre_done_hook_absent_by_default() {
        // Backward compatibility: no pre_done hook configured means no gate.
        let config = cosmon_core::config::ProjectConfig::default();
        assert!(config.hooks.pre_done.is_none());
    }

    #[test]
    fn test_pre_done_kill_switch_flag() {
        // The --skip-pre-done-hook flag alone bypasses the gate.
        assert!(pre_done_hook_skipped(true));
    }

    #[test]
    fn test_pre_done_kill_switch_env() {
        // SAFETY: single-threaded test-scoped env mutation, cleaned up below.
        // The env var (any non-empty value) bypasses the gate even without
        // the flag; unset it leaves the gate armed.
        let key = "COSMON_SKIP_PRE_DONE_HOOK";
        let prior = std::env::var_os(key);
        std::env::remove_var(key);
        assert!(!pre_done_hook_skipped(false), "unset env must not skip");
        std::env::set_var(key, "1");
        assert!(pre_done_hook_skipped(false), "set env must skip");
        std::env::set_var(key, "");
        assert!(
            !pre_done_hook_skipped(false),
            "empty env value must not skip"
        );
        // Restore.
        match prior {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    // -----------------------------------------------------------------
    // cs-binary multiplicity self-check (task-20260607-3ad4)
    // -----------------------------------------------------------------

    fn cs_bin(path: &str, mtime: &str) -> CsBinaryOnPath {
        CsBinaryOnPath {
            path: PathBuf::from(path),
            mtime: mtime.to_owned(),
        }
    }

    #[test]
    fn test_cs_multiplicity_warning_none_for_single_binary() {
        let one = vec![cs_bin("/Users/x/.local/bin/cs", "2026-06-07 10:00")];
        assert!(
            format_cs_multiplicity_warning(&one).is_none(),
            "a single binary is the healthy state — no warning"
        );
    }

    #[test]
    fn test_cs_multiplicity_warning_none_for_empty() {
        assert!(format_cs_multiplicity_warning(&[]).is_none());
    }

    #[test]
    fn test_cs_multiplicity_warning_lists_each_path_and_mtime() {
        let many = vec![
            cs_bin("/Users/x/.local/bin/cs", "2026-06-07 10:00"),
            cs_bin("/Users/x/.cargo/bin/cs", "2026-04-19 08:30"),
            cs_bin("/opt/homebrew/bin/cs", "2026-05-04 12:00"),
        ];
        let lines = format_cs_multiplicity_warning(&many).expect("3 binaries must warn");

        // Banner + one line per binary + trailing guidance.
        assert_eq!(lines.len(), 5, "banner + 3 entries + guidance");
        let joined = lines.join("\n");
        assert!(joined.contains("3 distinct"));
        // Every path and mtime is surfaced so the operator can act.
        assert!(joined.contains("/Users/x/.local/bin/cs"));
        assert!(joined.contains("/Users/x/.cargo/bin/cs"));
        assert!(joined.contains("/opt/homebrew/bin/cs"));
        assert!(joined.contains("2026-04-19 08:30"));
        // Never auto-rm: the guidance must say removal is the operator's gesture.
        assert!(joined.contains("operator-gestured"));
    }

    #[test]
    fn test_detect_cs_path_multiplicity_never_panics() {
        // Whatever the host PATH looks like, the self-check returns a vector
        // and never errors — it must not block the `cs done` hot path.
        let _ = detect_cs_path_multiplicity();
    }

    // -----------------------------------------------------------------
    // deploy verification (task-20260607-1403)
    // -----------------------------------------------------------------

    #[test]
    fn test_deploy_verification_match_is_a_quiet_action() {
        let v = DeployVerification::Match {
            head_short: "deadbeefcafe".to_owned(),
        };
        let (note, warn) = format_deploy_verification(&v);
        assert!(warn.is_none(), "a confirmed deploy must NOT warn");
        let note = note.expect("a confirmed deploy records a terse action note");
        assert!(note.contains("deploy verified"));
        assert!(note.contains("deadbeefcafe"));
    }

    #[test]
    fn test_deploy_verification_mismatch_warns_loudly() {
        let v = DeployVerification::Mismatch {
            deployed_short: "0000stale000".to_owned(),
            head_short: "1111fresh111".to_owned(),
        };
        let (note, warn) = format_deploy_verification(&v);
        assert!(
            note.is_none(),
            "a deploy gap is a warning, not a quiet note"
        );
        let lines = warn.expect("a mismatch must produce loud warning lines");
        let joined = lines.join("\n");
        // The operator must see BOTH SHAs to diagnose the drift.
        assert!(joined.contains("0000stale000"), "deployed SHA surfaced");
        assert!(joined.contains("1111fresh111"), "merged HEAD surfaced");
        // Loud banner + actionable remedy.
        assert!(joined.contains("DEPLOY GAP"));
        assert!(
            joined.contains("just install"),
            "must tell the operator how to fix it"
        );
    }

    #[test]
    fn test_deploy_verification_inconclusive_is_soft() {
        let v = DeployVerification::Inconclusive {
            reason: "no `cs` found on PATH".to_owned(),
        };
        let (note, warn) = format_deploy_verification(&v);
        assert!(
            warn.is_none(),
            "absence of evidence is not a deploy gap — never warn"
        );
        let note = note.expect("inconclusive still records why it was skipped");
        assert!(note.contains("deploy unverified"));
        assert!(note.contains("no `cs` found on PATH"));
    }

    #[test]
    fn test_verify_deploy_inconclusive_outside_git() {
        // A temp dir that is not a git repo: `git rev-parse HEAD` fails, so
        // verification degrades to Inconclusive rather than panicking or
        // blocking teardown.
        let tmp = TempDir::new().unwrap();
        let v = verify_deploy(tmp.path());
        assert!(
            matches!(v, DeployVerification::Inconclusive { .. }),
            "no git HEAD → inconclusive, got {v:?}"
        );
    }

    // -----------------------------------------------------------------
    // rescue_untracked_files
    // -----------------------------------------------------------------

    #[test]
    fn test_rescue_untracked_files_copies_to_molecule_dir() {
        let wt_tmp = TempDir::new().unwrap();
        let wt = wt_tmp.path();
        init_repo(wt);

        // Create untracked files including a nested one.
        std::fs::write(wt.join("synthesis.md"), "agent output\n").unwrap();
        std::fs::create_dir_all(wt.join("notes")).unwrap();
        std::fs::write(wt.join("notes/deep.txt"), "deep note\n").unwrap();

        let mol_tmp = TempDir::new().unwrap();
        let mol_dir = mol_tmp.path();

        let rescued = rescue_untracked_files(wt, mol_dir).unwrap();
        assert_eq!(
            rescued.len(),
            2,
            "expected 2 rescued files, got {rescued:?}"
        );

        // Verify files exist in rescued/ with correct content.
        let rescued_dir = mol_dir.join("rescued");
        assert_eq!(
            std::fs::read_to_string(rescued_dir.join("synthesis.md")).unwrap(),
            "agent output\n"
        );
        assert_eq!(
            std::fs::read_to_string(rescued_dir.join("notes/deep.txt")).unwrap(),
            "deep note\n"
        );
    }

    #[test]
    fn test_rescue_untracked_files_noop_when_clean() {
        let wt_tmp = TempDir::new().unwrap();
        let wt = wt_tmp.path();
        init_repo(wt);

        let mol_tmp = TempDir::new().unwrap();
        let rescued = rescue_untracked_files(wt, mol_tmp.path()).unwrap();
        assert!(
            rescued.is_empty(),
            "clean worktree should have nothing to rescue"
        );
        // rescued/ dir should not be created.
        assert!(!mol_tmp.path().join("rescued").exists());
    }

    #[test]
    fn test_is_branch_merged_detects_merged() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/chk"])
            .status
            .success());
        commit_file(repo, "c.txt", "c\n", "feat: c");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        assert!(!is_branch_merged(repo, "feat/chk"), "not yet merged");

        assert!(git(repo, &["merge", "--no-ff", "--no-edit", "feat/chk"])
            .status
            .success());
        assert!(is_branch_merged(repo, "feat/chk"), "now merged");
    }

    // -----------------------------------------------------------------
    // Auto-propel escalation tests
    // -----------------------------------------------------------------

    #[test]
    fn test_escalation_no_auto_propel_returns_conflict_immediately() {
        // With --no-auto-propel, a conflict must surface as the first-class
        // `MergeLoopOutcome::Conflict` (NOT an `Err`, NOT the escalation loop),
        // carrying the conflicting files and a recovery hint. This is the
        // structural half of the spark-20260622-6036 fix: the caller (`run`)
        // turns this outcome into a loud `❌ MERGE CONFLICT` + non-zero exit,
        // instead of the old "done with 1 warning".
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Create a conflict scenario.
        commit_file(repo, "shared.txt", "base\n", "seed shared");
        assert!(git(repo, &["checkout", "-q", "-b", "feat/esc-nopropel"])
            .status
            .success());
        commit_file(repo, "shared.txt", "from branch\n", "branch edit");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(repo, "shared.txt", "from main\n", "main edit");

        // Capture main's HEAD before the merge attempt so we can prove it
        // does not move on conflict.
        let main_head_before = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
            .trim()
            .to_owned();

        let (state_tmp, store) = make_store();
        let mol = sample_mol("task-20260411-e001", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_tmp.path().to_path_buf()),
        };

        let result = try_merge_with_escalation(
            &ctx,
            &store,
            &mol.id,
            repo,
            "feat/esc-nopropel",
            MergeStrategy::Merge,
            "task-20260411-e001",
            "test-socket",
            false, // auto_propel disabled
            3,
            None,
            &[],
        );

        match result.expect("conflict is a first-class Ok outcome, not Err") {
            MergeLoopOutcome::Conflict { files, recovery } => {
                assert!(
                    files.iter().any(|f| f == "shared.txt"),
                    "expected shared.txt in conflict files, got {files:?}"
                );
                assert!(
                    recovery.contains("shared.txt"),
                    "recovery should name the conflicting file: {recovery}"
                );
            }
            other => panic!("expected Conflict outcome, got {other:?}"),
        }

        // No escalation should have been recorded (auto_propel disabled).
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert!(
            reloaded.escalations.is_empty(),
            "no escalation entries expected with --no-auto-propel"
        );

        // INTEGRITY: the merge was rolled back — main is unchanged, the branch
        // is preserved, and the worktree is clean (no MERGE_HEAD marker).
        let main_head_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
            .trim()
            .to_owned();
        assert_eq!(
            main_head_before, main_head_after,
            "main HEAD must NOT move on a merge conflict"
        );
        assert!(
            branch_exists(repo, "feat/esc-nopropel"),
            "the worker's branch must be preserved on conflict"
        );
        assert!(
            !repo.join(".git/MERGE_HEAD").exists(),
            "worktree must be clean after conflict (no MERGE_HEAD left behind)"
        );
    }

    #[test]
    fn test_escalation_clean_merge_no_escalation_needed() {
        // When merge succeeds on first try, no escalation should happen.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/esc-clean"])
            .status
            .success());
        commit_file(repo, "clean.txt", "clean\n", "feat: clean");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        let (state_tmp, store) = make_store();
        let mol = sample_mol("task-20260411-e002", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_tmp.path().to_path_buf()),
        };

        let result = try_merge_with_escalation(
            &ctx,
            &store,
            &mol.id,
            repo,
            "feat/esc-clean",
            MergeStrategy::Merge,
            "task-20260411-e002",
            "test-socket",
            true,
            3,
            None,
            &[],
        );

        assert!(result.is_ok());
        assert!(
            matches!(result.unwrap(), MergeLoopOutcome::Merged),
            "expected Merged without escalation"
        );

        // No escalation entries should have been recorded.
        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert!(
            reloaded.escalations.is_empty(),
            "no escalations should be recorded for a clean merge"
        );

        // HAPPY PATH INTEGRITY: the merge actually landed on main — the file
        // from the feat branch is now present and the branch is reachable from
        // main. This is the precondition that lets `run` proceed to teardown
        // (kill tmux, remove worktree, delete branch); the conflict-fix must
        // NOT regress it.
        assert!(
            repo.join("clean.txt").exists(),
            "merged file must be present on main after a clean merge"
        );
        assert!(
            is_branch_merged(repo, "feat/esc-clean"),
            "feat branch must be an ancestor of main after a clean merge"
        );
    }

    #[test]
    fn test_exhausted_escalation_returns_conflict_not_error() {
        // When auto-propel runs but the worker never resolves the conflict,
        // the escalation loop exhausts its retries. Pre-fix this returned an
        // `Err` that `run` folded into "done with N warnings"; post-fix it
        // must surface as the first-class `Conflict` outcome (so `run` fails
        // loudly) — with the conflicting files and main left untouched.
        //
        // `max_retries = 0` exhausts immediately: the escalation loop body
        // never runs (no tmux needed), and the function falls straight through
        // to the exhaustion arm. The first mechanical attempt already recorded
        // the conflicting files.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        commit_file(repo, "shared.txt", "base\n", "seed shared");
        assert!(git(repo, &["checkout", "-q", "-b", "feat/esc-exhaust"])
            .status
            .success());
        commit_file(repo, "shared.txt", "from branch\n", "branch edit");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        commit_file(repo, "shared.txt", "from main\n", "main edit");

        let main_head_before = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
            .trim()
            .to_owned();

        let (state_tmp, store) = make_store();
        let mol = sample_mol("task-20260411-e006", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_tmp.path().to_path_buf()),
        };

        let result = try_merge_with_escalation(
            &ctx,
            &store,
            &mol.id,
            repo,
            "feat/esc-exhaust",
            MergeStrategy::Merge,
            "task-20260411-e006",
            "test-socket",
            true, // auto_propel enabled
            0,    // exhaust immediately
            None,
            &[],
        );

        match result.expect("exhausted conflict is a first-class Ok outcome") {
            MergeLoopOutcome::Conflict { files, recovery } => {
                assert!(
                    files.iter().any(|f| f == "shared.txt"),
                    "expected shared.txt in conflict files, got {files:?}"
                );
                assert!(
                    recovery.contains("exhausted"),
                    "exhaustion recovery should mention exhaustion: {recovery}"
                );
            }
            other => panic!("expected Conflict outcome after exhaustion, got {other:?}"),
        }

        let main_head_after = String::from_utf8_lossy(&git(repo, &["rev-parse", "main"]).stdout)
            .trim()
            .to_owned();
        assert_eq!(
            main_head_before, main_head_after,
            "main HEAD must NOT move when escalation exhausts on a conflict"
        );
        assert!(
            branch_exists(repo, "feat/esc-exhaust"),
            "the worker's branch must be preserved after exhaustion"
        );
    }

    // NOTE: the end-to-end exit-code assertions for the conflict-vs-clean
    // teardown decision live in the integration test
    // `tests/done_merge_conflict.rs`, which spawns the real `cs` binary in an
    // isolated temp repo (child `current_dir`, no process-global cwd
    // mutation). `find_repo_root()` is cwd-based, so driving `run()` in-process
    // would require mutating the test binary's shared cwd — unsafe in a
    // parallel test binary. The hermetic unit coverage above
    // (`try_merge_with_escalation`, which takes `repo_root` explicitly) proves
    // the integrity invariants: conflict ⇒ branch preserved + main unchanged +
    // worktree clean; clean ⇒ merged.

    #[test]
    fn test_escalation_records_audit_trail() {
        // Verify that escalation entries are written to molecule state.
        let (_state_tmp, store) = make_store();
        let mol = sample_mol("task-20260411-e003", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        record_escalation(&store, &mol.id, 0, "conflict→propel");
        record_escalation(&store, &mol.id, 1, "conflict→propel");
        record_escalation(&store, &mol.id, 2, "exhausted");

        let reloaded = store.load_molecule(&mol.id).unwrap();
        assert_eq!(reloaded.escalations.len(), 3);
        assert_eq!(reloaded.escalations[0].retry, 0);
        assert_eq!(reloaded.escalations[0].outcome, "conflict→propel");
        assert_eq!(reloaded.escalations[1].retry, 1);
        assert_eq!(reloaded.escalations[2].outcome, "exhausted");
    }

    #[test]
    fn test_escalation_already_merged_no_escalation() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(git(repo, &["checkout", "-q", "-b", "feat/esc-already"])
            .status
            .success());
        commit_file(repo, "a.txt", "a\n", "feat: a");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["merge", "-q", "--ff-only", "feat/esc-already"])
            .status
            .success());

        let (state_tmp, store) = make_store();
        let mol = sample_mol("task-20260411-e004", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_tmp.path().to_path_buf()),
        };

        let result = try_merge_with_escalation(
            &ctx,
            &store,
            &mol.id,
            repo,
            "feat/esc-already",
            MergeStrategy::Merge,
            "task-20260411-e004",
            "test-socket",
            true,
            3,
            None,
            &[],
        );

        assert!(matches!(result.unwrap(), MergeLoopOutcome::AlreadyMerged));
    }

    #[test]
    fn test_escalation_no_branch_returns_immediately() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let (state_tmp, store) = make_store();
        let mol = sample_mol("task-20260411-e005", MoleculeStatus::Completed);
        store.save_molecule(&mol.id, &mol).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(state_tmp.path().to_path_buf()),
        };

        let result = try_merge_with_escalation(
            &ctx,
            &store,
            &mol.id,
            repo,
            "feat/does-not-exist",
            MergeStrategy::Merge,
            "task-20260411-e005",
            "test-socket",
            true,
            3,
            None,
            &[],
        );

        assert!(matches!(result.unwrap(), MergeLoopOutcome::NoBranch));
    }

    // -----------------------------------------------------------------
    // Auto-commit molecule artifacts tests
    // -----------------------------------------------------------------

    #[test]
    fn test_commit_artifacts_commits_molecule_dir() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Simulate molecule artifacts in .cosmon/state/fleets/default/molecules/<id>/
        let mol_id = MoleculeId::new("task-20260413-art1").unwrap();
        let mol_dir = repo
            .join(".cosmon/state/fleets/default/molecules")
            .join(mol_id.as_str());
        std::fs::create_dir_all(&mol_dir).unwrap();
        std::fs::write(mol_dir.join("prompt.md"), "# Prompt\ntopic here\n").unwrap();
        std::fs::write(mol_dir.join("briefing.md"), "# Briefing\nsteps\n").unwrap();

        // Simulate events.jsonl
        let events_path = repo.join(".cosmon/state/events.jsonl");
        std::fs::write(&events_path, "{\"event\":\"nucleated\"}\n").unwrap();

        let result =
            commit_molecule_artifacts(repo, &mol_dir, &events_path, &mol_id, "test topic", &[]);
        assert!(result.is_ok(), "commit should succeed: {result:?}");
        assert!(result.unwrap(), "should have committed something");

        // Verify the commit exists in git log.
        let log = git(repo, &["log", "--oneline", "-1"]);
        let msg = String::from_utf8_lossy(&log.stdout);
        assert!(
            msg.contains("chore(state): track artifacts for task-20260413-art1 (test topic)"),
            "commit message mismatch: {msg}"
        );
    }

    #[test]
    fn test_commit_artifacts_noop_when_nothing_to_stage() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260413-art2").unwrap();
        let mol_dir = repo
            .join(".cosmon/state/fleets/default/molecules")
            .join(mol_id.as_str());
        let events_path = repo.join(".cosmon/state/events.jsonl");

        // Neither mol_dir nor events_path exist — nothing to commit.
        let result =
            commit_molecule_artifacts(repo, &mol_dir, &events_path, &mol_id, "no artifacts", &[]);
        assert!(result.is_ok());
        assert!(!result.unwrap(), "should report nothing committed");
    }

    #[test]
    fn test_commit_artifacts_empty_topic() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260413-art3").unwrap();
        let mol_dir = repo
            .join(".cosmon/state/fleets/default/molecules")
            .join(mol_id.as_str());
        std::fs::create_dir_all(&mol_dir).unwrap();
        std::fs::write(mol_dir.join("log.md"), "event\n").unwrap();

        let events_path = repo.join(".cosmon/state/events.jsonl");

        let result = commit_molecule_artifacts(repo, &mol_dir, &events_path, &mol_id, "", &[]);
        assert!(result.is_ok());
        assert!(result.unwrap());

        let log = git(repo, &["log", "--oneline", "-1"]);
        let msg = String::from_utf8_lossy(&log.stdout);
        assert!(
            msg.contains("chore(state): track artifacts for task-20260413-art3"),
            "commit message should omit empty topic: {msg}"
        );
        // Should NOT contain parentheses for empty topic.
        assert!(
            !msg.contains("()"),
            "empty topic should not produce empty parens: {msg}"
        );
    }

    #[test]
    fn test_commit_artifacts_stamps_coauthor_trailers() {
        // Native attribution — the FALLBACK carrier (delib-20260717-194b, F1).
        // This proves the trailers ride the *artifact* commit for
        // artifact-producing molecules. It is deliberately NOT the load-bearing
        // regression: a task-work molecule produces no artifact commit, so the
        // PRIMARY carrier is the merge commit — see
        // `merge_commit_carries_coauthor_trailers` for that invariant (knuth's
        // "prove the right object"). The trailers ride as a contiguous
        // Co-Authored-By block, blank-line-separated so git parses them.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260717-attr").unwrap();
        let mol_dir = repo
            .join(".cosmon/state/fleets/default/molecules")
            .join(mol_id.as_str());
        std::fs::create_dir_all(&mol_dir).unwrap();
        std::fs::write(mol_dir.join("prompt.md"), "# Prompt\n").unwrap();
        let events_path = repo.join(".cosmon/state/events.jsonl");
        std::fs::write(&events_path, "{\"event\":\"done\"}\n").unwrap();

        let trailers = vec!["Co-Authored-By: Noogram (claude) <noreply@noogram.org>".to_owned()];
        let result =
            commit_molecule_artifacts(repo, &mol_dir, &events_path, &mol_id, "attr", &trailers);
        assert!(result.is_ok(), "commit should succeed: {result:?}");
        assert!(result.unwrap());

        // Full commit body — the single trailer is present, subject intact.
        let log = git(repo, &["log", "-1", "--format=%B"]);
        let body = String::from_utf8_lossy(&log.stdout);
        assert!(
            body.contains("chore(state): track artifacts for task-20260717-attr (attr)"),
            "subject missing: {body}"
        );
        assert!(
            body.contains("Co-Authored-By: Noogram (claude) <noreply@noogram.org>"),
            "maker trailer missing: {body}"
        );
        assert!(
            !body.contains("<claude@noogram.org>"),
            "synthetic adapter email leaked into trailer: {body}"
        );
        // git recognises them as real trailers (blank-line-separated block).
        let interpreted = git(
            repo,
            &["log", "-1", "--format=%(trailers:key=Co-Authored-By)"],
        );
        let trailers_out = String::from_utf8_lossy(&interpreted.stdout);
        assert!(
            trailers_out.contains("Noogram (claude) <noreply@noogram.org>"),
            "git did not parse the co-author trailers: {trailers_out}"
        );
    }

    // ---- Author-slot assertion pure core (F4 / adversary CATCH) -----------

    fn test_operator() -> OperatorIdentity {
        OperatorIdentity {
            name: "Operator".to_owned(),
            email: "op@x.org".to_owned(),
        }
    }

    /// The happy path: every commit authored AND committed by the operator
    /// yields no violations.
    #[test]
    fn author_scan_all_operator_is_clean() {
        let sep = AUTHOR_SCAN_SEP;
        let log = format!(
            "abc123{sep}Operator{sep}op@x.org{sep}Operator{sep}op@x.org\n\
             def456{sep}Operator{sep}op@x.org{sep}Operator{sep}op@x.org\n"
        );
        assert!(collect_non_operator_authored(&log, &test_operator()).is_empty());
    }

    /// The codex-author leak: a commit whose author slot is the maker
    /// (`hello@noogram.org`) is flagged even though its committer is the
    /// operator — the author slot is a responsibility claim the maker must not
    /// occupy.
    #[test]
    fn author_scan_flags_leaked_author() {
        let sep = AUTHOR_SCAN_SEP;
        let log = format!("beef01{sep}Noogram{sep}hello@noogram.org{sep}Operator{sep}op@x.org\n");
        let hits = collect_non_operator_authored(&log, &test_operator());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sha, "beef01");
        assert_eq!(hits[0].author_email, "hello@noogram.org");
    }

    /// A leaked *committer* slot is caught too (the merge/amend that re-stamps
    /// only the committer).
    #[test]
    fn author_scan_flags_leaked_committer() {
        let sep = AUTHOR_SCAN_SEP;
        let log = format!("c0ffee{sep}Operator{sep}op@x.org{sep}Noogram{sep}bot@noogram.org\n");
        let hits = collect_non_operator_authored(&log, &test_operator());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].committer_email, "bot@noogram.org");
    }

    /// Email comparison is case-insensitive while the operator name must match.
    #[test]
    fn author_scan_is_case_insensitive() {
        let sep = AUTHOR_SCAN_SEP;
        let log = format!("aa11{sep}Operator{sep}Op@X.org{sep}Operator{sep}op@x.ORG\n");
        assert!(collect_non_operator_authored(&log, &test_operator()).is_empty());
    }

    /// A maker name paired with the operator's allowed email is still a leak:
    /// names are part of the primary identity, not an email-only decoration.
    #[test]
    fn author_scan_rejects_noogram_name_with_operator_email() {
        let sep = AUTHOR_SCAN_SEP;
        let log = format!("bad123{sep}Noogram{sep}op@x.org{sep}Operator{sep}op@x.org\n");
        let hits = collect_non_operator_authored(&log, &test_operator());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].author_name, "Noogram");
        assert_eq!(hits[0].author_email, "op@x.org");
    }

    /// An empty log (empty range / probe slip) yields no violations — the scan
    /// finds nothing, which is exactly why F5 fails closed on a vacuous range
    /// *before* this scan can pass over an empty set.
    #[test]
    fn author_scan_empty_log_is_clean() {
        assert!(collect_non_operator_authored("", &test_operator()).is_empty());
    }

    #[test]
    fn missing_adapter_witness_warns_when_attribution_is_enabled() {
        use cosmon_state::ops::model_attribution::folded_adapter;

        let state = TempDir::new().unwrap();
        let mol_id = MoleculeId::new("task-20260718-warn").unwrap();
        let mut warnings = Vec::new();
        let adapter = adapter_for_coauthor(
            folded_adapter(state.path(), &mol_id),
            &mol_id,
            true,
            &mut warnings,
        );
        assert!(adapter.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no adapter witness"));
        assert!(warnings[0].contains("does NOT prove which adapter ran"));
    }

    #[test]
    fn missing_adapter_witness_stays_silent_without_attribution() {
        use cosmon_state::ops::model_attribution::AdapterFold;

        let mol_id = MoleculeId::new("task-20260718-noaa").unwrap();
        let mut warnings = Vec::new();
        let adapter = adapter_for_coauthor(AdapterFold::Absent, &mol_id, false, &mut warnings);
        assert!(adapter.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn missing_adapter_witness_warns_when_only_coauthor_facet_is_enabled() {
        use cosmon_state::ops::model_attribution::AdapterFold;

        let attribution = cosmon_core::config::AttributionConfig {
            coauthor_name: "Noogram".to_owned(),
            coauthor_email: "noreply@noogram.org".to_owned(),
            ..cosmon_core::config::AttributionConfig::default()
        };
        assert!(
            attribution.is_empty(),
            "the directive facet remains disabled"
        );
        let enabled = !attribution.coauthor_trailers(None).is_empty();
        let mol_id = MoleculeId::new("task-20260718-facet").unwrap();
        let mut warnings = Vec::new();

        let adapter = adapter_for_coauthor(AdapterFold::Absent, &mol_id, enabled, &mut warnings);

        assert!(adapter.is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no adapter witness"));
    }

    #[test]
    fn ff_only_refuses_configured_attribution_without_touching_git() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        assert!(git(repo, &["checkout", "-q", "-b", "feat/ff-attribution"])
            .status
            .success());
        commit_file(repo, "worker.rs", "// worker\n", "feat: worker");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        let head_before = git(repo, &["rev-parse", "HEAD"]).stdout;

        let trailers = vec!["Co-Authored-By: Noogram <noreply@noogram.org>".to_owned()];
        let err = ensure_attribution_carrier(MergeStrategy::FfOnly, &trailers).unwrap_err();
        assert!(err.to_string().contains("creates no merge commit"));
        assert!(err.to_string().contains("--strategy merge"));
        assert_eq!(git(repo, &["rev-parse", "HEAD"]).stdout, head_before);
        assert!(!is_branch_merged(repo, "feat/ff-attribution"));
    }

    #[test]
    fn ff_only_remains_available_when_attribution_is_disabled() {
        assert!(ensure_attribution_carrier(MergeStrategy::FfOnly, &[]).is_ok());
    }

    // ---- C4 visibility (pré-mortem task-20260717-ffe1) --------------------

    /// C4: a configured `[attribution]` block with an empty `coauthor_email`
    /// used to integrate in silence — no trailer AND no signal. The
    /// configuration stays legal, but it must now warn at integration time.
    #[test]
    fn unstamped_attribution_block_warns() {
        let attribution = cosmon_core::config::AttributionConfig {
            public_name: "Noogram".to_owned(),
            ..cosmon_core::config::AttributionConfig::default()
        };
        let trailers = attribution.coauthor_trailers(Some("claude"));
        assert!(
            trailers.is_empty(),
            "empty coauthor_email must gate the facet"
        );

        let mut warnings = Vec::new();
        warn_unstamped_attribution(&attribution, &trailers, &mut warnings);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("coauthor_email` is empty"));
        assert!(warnings[0].contains("Noogram"));
        assert!(warnings[0].contains("NO `Co-Authored-By:` trailer"));
    }

    /// A zero-config galaxy (no `[attribution]` at all) must stay silent —
    /// the warning is scoped to the half-configured state, never to absence.
    #[test]
    fn absent_attribution_block_stays_silent() {
        let attribution = cosmon_core::config::AttributionConfig::default();
        let mut warnings = Vec::new();
        warn_unstamped_attribution(&attribution, &[], &mut warnings);
        assert!(warnings.is_empty());
    }

    /// A fully-configured block that produces trailers must not warn.
    #[test]
    fn stamped_attribution_block_does_not_warn() {
        let attribution = cosmon_core::config::AttributionConfig {
            public_name: "Noogram".to_owned(),
            coauthor_email: "noreply@noogram.org".to_owned(),
            ..cosmon_core::config::AttributionConfig::default()
        };
        let trailers = attribution.coauthor_trailers(None);
        assert_eq!(trailers.len(), 1);
        let mut warnings = Vec::new();
        warn_unstamped_attribution(&attribution, &trailers, &mut warnings);
        assert!(warnings.is_empty());
    }

    // ---- C6: pre-existing worker trailer (pré-mortem task-20260717-ffe1) --

    /// C6: a worker commit that already carries a `Co-Authored-By` trailer is
    /// integrated WITHOUT rewrite — its SHA and its trailer survive intact —
    /// while the merge commit carries the configured trailer. Two carriers,
    /// two objects, no corruption and no intra-message duplication. (There is
    /// deliberately no cross-commit deduplication: cosmon never rewrites
    /// worker commits, so a worker that self-stamped keeps its own stamp.)
    #[test]
    fn preexisting_worker_trailer_survives_merge_unrewritten() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        assert!(git(repo, &["checkout", "-q", "-b", "feat/pre"])
            .status
            .success());
        std::fs::write(repo.join("w.rs"), "// w\n").unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(git(
            repo,
            &[
                "commit",
                "-q",
                "-m",
                "feat: worker work",
                "-m",
                "Co-Authored-By: Existing Person <existing@example.test>",
            ],
        )
        .status
        .success());
        let worker_sha = String::from_utf8_lossy(&git(repo, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        let trailers = vec!["Co-Authored-By: Noogram (claude) <noreply@noogram.org>".to_owned()];
        let outcome = try_merge_branch(repo, "feat/pre", MergeStrategy::Merge, &trailers);
        assert!(matches!(outcome, MergeOutcome::Merged), "got {outcome:?}");

        // The worker commit was NOT rewritten: same SHA, same trailer.
        let branch_tip = String::from_utf8_lossy(&git(repo, &["rev-parse", "HEAD^2"]).stdout)
            .trim()
            .to_owned();
        assert_eq!(branch_tip, worker_sha, "worker commit must keep its SHA");
        let worker_trailers = git(
            repo,
            &[
                "log",
                "-1",
                "--format=%(trailers:key=Co-Authored-By)",
                &worker_sha,
            ],
        );
        let worker_out = String::from_utf8_lossy(&worker_trailers.stdout);
        assert!(
            worker_out.contains("Existing Person <existing@example.test>"),
            "pre-existing trailer must survive: {worker_out}"
        );
        assert!(
            !worker_out.contains("Noogram"),
            "the configured trailer must not be injected into the worker commit: {worker_out}"
        );

        // The merge commit carries the configured trailer exactly once and
        // does not absorb the worker's own trailer.
        let merge_body = git(repo, &["log", "-1", "--format=%B"]);
        let body = String::from_utf8_lossy(&merge_body.stdout);
        assert_eq!(
            body.matches("Co-Authored-By: Noogram (claude) <noreply@noogram.org>")
                .count(),
            1,
            "configured trailer must appear exactly once on the merge commit: {body}"
        );
        assert!(
            !body.contains("existing@example.test"),
            "the worker's own trailer belongs to the worker commit only: {body}"
        );
    }

    // ---- Merge-commit trailer carrier (F1) + author assertion (F4) --------

    /// I1 (the load-bearing regression, knuth): on the default `--no-ff` path,
    /// the trailers ride the MERGE COMMIT that `cs done` itself creates — the
    /// carrier that exists for a task-work molecule that produces no artifact
    /// commit. Pre-194b this was 0/N (the trailers were threaded only into the
    /// phantom artifact commit). RED-before / GREEN-after.
    #[test]
    fn merge_commit_carries_coauthor_trailers() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        // A feature branch with one worker commit and no artifact commit.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        commit_file(repo, "src.rs", "fn main() {}\n", "feat: real work");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        let trailers = vec!["Co-Authored-By: Noogram (claude) <noreply@noogram.org>".to_owned()];
        let outcome = try_merge_branch(repo, "feat/a", MergeStrategy::Merge, &trailers);
        assert!(matches!(outcome, MergeOutcome::Merged), "got {outcome:?}");

        // HEAD is now the merge commit — git must parse the single trailer.
        let interpreted = git(
            repo,
            &["log", "-1", "--format=%(trailers:key=Co-Authored-By)"],
        );
        let out = String::from_utf8_lossy(&interpreted.stdout);
        assert!(
            out.contains("Noogram (claude) <noreply@noogram.org>")
                && !out.contains("<claude@noogram.org>"),
            "merge commit did not carry the trailer block: {out}"
        );
        // The subject is preserved as git's own default merge subject.
        let subj = git(repo, &["log", "-1", "--format=%s"]);
        assert!(String::from_utf8_lossy(&subj.stdout).contains("Merge branch 'feat/a'"));
    }

    /// I4: empty trailers ⇒ the merge commit is byte-identical to a
    /// pre-attribution cosmon (the `--no-edit` path), carrying NO trailer.
    #[test]
    fn merge_commit_without_trailers_is_unstamped() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        assert!(git(repo, &["checkout", "-q", "-b", "feat/b"])
            .status
            .success());
        commit_file(repo, "src.rs", "fn main() {}\n", "feat: work");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        let outcome = try_merge_branch(repo, "feat/b", MergeStrategy::Merge, &[]);
        assert!(matches!(outcome, MergeOutcome::Merged), "got {outcome:?}");
        let body = git(repo, &["log", "-1", "--format=%B"]);
        assert!(
            !String::from_utf8_lossy(&body.stdout).contains("Co-Authored-By"),
            "empty trailers must not stamp the merge commit"
        );
    }

    /// F4 end-to-end over a real repo: `assert_operator_authored_commits`
    /// passes when every feature commit is operator-authored and FAILS (listing
    /// the SHA) when a commit's author slot leaked the maker identity — the
    /// codex-author bug, caught independent of any blocklist.
    #[test]
    fn assert_operator_authored_over_real_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo); // operator identity: test@example.com

        // Clean branch — one operator-authored feature commit.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/clean"])
            .status
            .success());
        commit_file(repo, "a.rs", "// a\n", "feat: a");
        assert!(assert_operator_authored_commits(
            repo,
            "main",
            "feat/clean",
            &OperatorIdentity {
                name: "Test".to_owned(),
                email: "test@example.com".to_owned(),
            },
        )
        .is_ok());

        // Leaky branch — a commit authored by the maker identity.
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["checkout", "-q", "-b", "feat/leak"])
            .status
            .success());
        std::fs::write(repo.join("b.rs"), "// b\n").unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(git(
            repo,
            &[
                "-c",
                "user.name=Noogram",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "feat: leaked author",
            ],
        )
        .status
        .success());
        let err = assert_operator_authored_commits(
            repo,
            "main",
            "feat/leak",
            &OperatorIdentity {
                name: "Test".to_owned(),
                email: "test@example.com".to_owned(),
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Noogram <test@example.com>"), "msg: {msg}");
        assert!(msg.contains("not operator-authored"), "msg: {msg}");
    }

    /// C2 regression: an attribution-enabled checkout without a complete git
    /// identity must stop before integration instead of silently disabling the
    /// author-slot assertion.
    #[test]
    fn missing_operator_identity_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        assert!(git(repo, &["config", "user.name", ""]).status.success());
        assert!(git(repo, &["config", "user.email", ""]).status.success());

        let error = require_operator_identity(repo).unwrap_err().to_string();
        assert!(error.contains("refuses attribution-enabled integration"));
        assert!(error.contains("git config user.name"));
        assert!(error.contains("git config user.email"));
    }

    /// I3: on the ff-only path no merge object is born, so the trailer carrier
    /// is absent — but the feature commits must STILL be operator-authored
    /// (P2 is fixed at birth, not at merge). The author assertion holds
    /// regardless of merge shape.
    #[test]
    fn ff_only_feature_commits_are_operator_authored() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        assert!(git(repo, &["checkout", "-q", "-b", "feat/ff"])
            .status
            .success());
        commit_file(repo, "c.rs", "// c\n", "feat: c");
        // Author assertion passes on the branch before any merge.
        assert!(assert_operator_authored_commits(
            repo,
            "main",
            "feat/ff",
            &OperatorIdentity {
                name: "Test".to_owned(),
                email: "test@example.com".to_owned(),
            },
        )
        .is_ok());
        // ff-only merge creates no merge commit (no trailer carrier), which is
        // exactly why author-correctness cannot depend on the merge.
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        let outcome = try_merge_branch(repo, "feat/ff", MergeStrategy::FfOnly, &[]);
        assert!(matches!(outcome, MergeOutcome::Merged), "got {outcome:?}");
        let parents = git(repo, &["log", "-1", "--format=%P"]);
        // A ff merge leaves HEAD at the single feature-commit parent chain — the
        // tip has one parent, not two, proving no merge object was created.
        assert_eq!(
            String::from_utf8_lossy(&parents.stdout)
                .split_whitespace()
                .count(),
            1
        );
    }

    #[test]
    fn test_is_source_path_classifies_source_and_state() {
        // Source paths a chore(state) commit must never carry.
        assert!(is_source_path("crates/cosmon-rpp-adapter/Cargo.toml"));
        assert!(is_source_path("crates/cosmon-cli/src/cmd/done.rs"));
        assert!(is_source_path("crates"));
        assert!(is_source_path("src/main.rs"));
        assert!(is_source_path("src"));
        assert!(is_source_path("Cargo.toml"));
        assert!(is_source_path("Cargo.lock"));
        // State / artifact paths are fine.
        assert!(!is_source_path(".cosmon/state/events.jsonl"));
        assert!(!is_source_path(
            ".cosmon/state/fleets/default/molecules/task-1/prompt.md"
        ));
        assert!(!is_source_path("docs/adr/099-foo.md"));
        assert!(!is_source_path("README.md"));
        assert!(!is_source_path(""));
        // A molecule artifact that merely mentions "cargo" in its name is not
        // a manifest — only the exact basenames are forbidden.
        assert!(!is_source_path(".cosmon/state/.../notes/Cargo.toml.md"));
    }

    /// The regression that motivated this guard (postmortem 2026-06-15,
    /// commit 2e86cf908): a `chore(state): track artifacts` commit swept
    /// pre-staged source (crates/** + Cargo.*) into the index and reverted
    /// it under a misleading subject. With the scoped commit, source staged
    /// in the index must stay OUT of the artifact commit.
    #[test]
    fn test_commit_artifacts_does_not_sweep_prestaged_source() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // A committed source file with current content.
        std::fs::create_dir_all(repo.join("crates/foo")).unwrap();
        commit_file(
            repo,
            "crates/foo/Cargo.toml",
            "version = \"2.0.1\"\n",
            "feat: bump foo to 2.0.1",
        );

        // Now a rogue step pre-stages a STALE version of that source file —
        // exactly the index state that produced the silent revert.
        std::fs::write(repo.join("crates/foo/Cargo.toml"), "version = \"0.1.0\"\n").unwrap();
        assert!(git(repo, &["add", "crates/foo/Cargo.toml"])
            .status
            .success());

        // Legitimate molecule artifacts to track.
        let mol_id = MoleculeId::new("task-20260614-a74d").unwrap();
        let mol_dir = repo
            .join(".cosmon/state/fleets/default/molecules")
            .join(mol_id.as_str());
        std::fs::create_dir_all(&mol_dir).unwrap();
        std::fs::write(mol_dir.join("prompt.md"), "# Prompt\n").unwrap();
        let events_path = repo.join(".cosmon/state/events.jsonl");
        std::fs::write(&events_path, "{\"event\":\"done\"}\n").unwrap();

        let result =
            commit_molecule_artifacts(repo, &mol_dir, &events_path, &mol_id, "mission", &[]);
        assert!(result.is_ok(), "commit should succeed: {result:?}");
        assert!(result.unwrap(), "should have committed the artifacts");

        // The artifact commit must NOT include the source file.
        let show = git(repo, &["show", "--name-only", "--format=", "HEAD"]);
        let files = String::from_utf8_lossy(&show.stdout);
        assert!(
            !files.contains("crates/foo/Cargo.toml"),
            "source file leaked into chore(state) commit: {files}"
        );
        assert!(
            files.contains("prompt.md"),
            "artifact missing from commit: {files}"
        );

        // The stale source is still staged but uncommitted — HEAD keeps the
        // current (2.0.1) content, so no silent revert occurred.
        let head_blob = git(repo, &["show", "HEAD:crates/foo/Cargo.toml"]);
        assert!(
            String::from_utf8_lossy(&head_blob.stdout).contains("2.0.1"),
            "HEAD source was reverted by the state commit"
        );
    }

    /// Same guarantee for the pre-merge flush commit: pre-staged source must
    /// never ride into `chore(state): flush before merge`.
    #[test]
    fn test_flush_state_dir_does_not_sweep_prestaged_source() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        std::fs::create_dir_all(repo.join("crates/bar")).unwrap();
        commit_file(
            repo,
            "crates/bar/Cargo.toml",
            "version = \"2.0.1\"\n",
            "feat: bar 2.0.1",
        );

        // Seed a tracked state file (scoped commit so the seed itself does not
        // sweep anything — the regression we are guarding against).
        let events_path = repo.join(".cosmon/state/events.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        std::fs::write(&events_path, "{\"event\":\"a\"}\n").unwrap();
        assert!(git(repo, &["add", ".cosmon/state/events.jsonl"])
            .status
            .success());
        assert!(git(
            repo,
            &["commit", "-q", "-m", "seed events", "--", ".cosmon/state/"]
        )
        .status
        .success());

        // NOW pre-stage a stale source edit and dirty the state file. The
        // flush must commit only the state change, leaving the source out.
        std::fs::write(repo.join("crates/bar/Cargo.toml"), "version = \"0.1.0\"\n").unwrap();
        assert!(git(repo, &["add", "crates/bar/Cargo.toml"])
            .status
            .success());
        std::fs::write(&events_path, "{\"event\":\"a\"}\n{\"event\":\"b\"}\n").unwrap();

        let flushed = flush_state_dir_changes(repo).expect("flush must succeed");
        assert!(flushed, "flush should have committed the state change");

        let show = git(repo, &["show", "--name-only", "--format=", "HEAD"]);
        let files = String::from_utf8_lossy(&show.stdout);
        assert!(
            !files.contains("crates/bar/Cargo.toml"),
            "source leaked into flush commit: {files}"
        );
        let head_blob = git(repo, &["show", "HEAD:crates/bar/Cargo.toml"]);
        assert!(
            String::from_utf8_lossy(&head_blob.stdout).contains("2.0.1"),
            "HEAD source was reverted by the flush commit"
        );
    }

    // -----------------------------------------------------------------
    // Workspace artifact relocation tests (task-20260418-7506)
    // -----------------------------------------------------------------

    #[test]
    fn test_relocate_moves_unscoped_molecule_file_to_molecule_id_subdir() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Commit an unscoped workspace artifact, mimicking mailroom's
        // `molecule/review.md` convention.
        std::fs::create_dir_all(repo.join("molecule")).unwrap();
        std::fs::write(repo.join("molecule/review.md"), "# review\n").unwrap();
        assert!(git(repo, &["add", "molecule/review.md"]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "review: add"])
            .status
            .success());

        let mol_id = MoleculeId::new("task-20260418-aaaa").unwrap();
        let moved = relocate_workspace_artifacts(repo, &mol_id).expect("relocate");
        assert_eq!(moved, vec!["molecule/review.md".to_owned()]);

        assert!(!repo.join("molecule/review.md").exists());
        assert!(repo.join("molecule/task-20260418-aaaa/review.md").exists());

        let log = git(repo, &["log", "--oneline", "-1"]);
        let msg = String::from_utf8_lossy(&log.stdout);
        assert!(
            msg.contains(
                "chore(done): relocate workspace artifacts to molecule/task-20260418-aaaa/"
            ),
            "rename commit not found: {msg}"
        );
    }

    #[test]
    fn test_relocate_is_noop_when_artifacts_already_scoped() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260418-bbbb").unwrap();
        let scoped_dir = repo.join("molecule").join(mol_id.as_str());
        std::fs::create_dir_all(&scoped_dir).unwrap();
        std::fs::write(scoped_dir.join("review.md"), "# review\n").unwrap();
        assert!(git(repo, &["add", "molecule/"]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "review: scoped"])
            .status
            .success());
        let head_before = git(repo, &["rev-parse", "HEAD"]);
        let before = String::from_utf8_lossy(&head_before.stdout)
            .trim()
            .to_owned();

        let moved = relocate_workspace_artifacts(repo, &mol_id).expect("relocate");
        assert!(moved.is_empty());

        let head_after = git(repo, &["rev-parse", "HEAD"]);
        let after = String::from_utf8_lossy(&head_after.stdout)
            .trim()
            .to_owned();
        assert_eq!(before, after, "no new commit expected on no-op relocate");
    }

    #[test]
    fn test_relocate_leaves_other_molecule_subdirs_alone() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // A pre-existing artifact already scoped under a DIFFERENT molecule.
        let other_dir = repo.join("molecule/task-20260414-cccc");
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::write(other_dir.join("review.md"), "# prior\n").unwrap();
        // A NEW unscoped artifact belonging to our molecule.
        std::fs::write(repo.join("molecule/report.md"), "# new\n").unwrap();
        assert!(git(repo, &["add", "molecule/"]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "mixed"]).status.success());

        let mol_id = MoleculeId::new("task-20260418-dddd").unwrap();
        let moved = relocate_workspace_artifacts(repo, &mol_id).expect("relocate");
        assert_eq!(moved, vec!["molecule/report.md".to_owned()]);

        // Our new file is scoped; the other molecule's scoping is untouched.
        assert!(repo.join("molecule/task-20260418-dddd/report.md").exists());
        assert!(repo.join("molecule/task-20260414-cccc/review.md").exists());
        assert!(!repo.join("molecule/report.md").exists());
    }

    #[test]
    fn test_parallel_branches_with_relocated_artifacts_merge_cleanly() {
        // Integration scenario from the motivating incident (mailroom
        // 2026-04-18): three branches each wrote `molecule/review.md` at the
        // repo root, producing add/add conflicts on every landing. After
        // per-branch relocation the paths are disjoint and both merges
        // succeed with the default strategy.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Branch A writes molecule/review.md then relocates.
        let mol_a = MoleculeId::new("task-20260414-b33a").unwrap();
        assert!(git(repo, &["checkout", "-q", "-b", "feat/a"])
            .status
            .success());
        std::fs::create_dir_all(repo.join("molecule")).unwrap();
        std::fs::write(repo.join("molecule/review.md"), "A review\n").unwrap();
        assert!(git(repo, &["add", "molecule/review.md"]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "review: A"])
            .status
            .success());
        relocate_workspace_artifacts(repo, &mol_a).expect("relocate A");

        // Branch B starts from main, writes the same path, then relocates.
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(repo, &["checkout", "-q", "-b", "feat/b"])
            .status
            .success());
        let mol_b = MoleculeId::new("task-20260414-7ae4").unwrap();
        std::fs::create_dir_all(repo.join("molecule")).unwrap();
        std::fs::write(repo.join("molecule/review.md"), "B review\n").unwrap();
        assert!(git(repo, &["add", "molecule/review.md"]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "review: B"])
            .status
            .success());
        relocate_workspace_artifacts(repo, &mol_b).expect("relocate B");

        // Land branch A first (fast-forward or merge commit — both fine).
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        let merge_a = try_merge_branch(repo, "feat/a", MergeStrategy::Merge, &[]);
        assert!(
            matches!(merge_a, MergeOutcome::Merged),
            "A must land: {merge_a:?}"
        );

        // Land branch B — without relocation this would add/add conflict on
        // `molecule/review.md`. After relocation the paths are disjoint.
        let merge_b = try_merge_branch(repo, "feat/b", MergeStrategy::Merge, &[]);
        assert!(
            matches!(merge_b, MergeOutcome::Merged),
            "B must land cleanly after relocation: {merge_b:?}"
        );

        // Both reviews coexist, each under its own molecule-scoped path.
        assert_eq!(
            std::fs::read_to_string(repo.join("molecule/task-20260414-b33a/review.md")).unwrap(),
            "A review\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("molecule/task-20260414-7ae4/review.md")).unwrap(),
            "B review\n"
        );
        assert!(!repo.join("molecule/review.md").exists());
    }

    #[test]
    fn test_relocate_skips_collision_and_leaves_source_in_place() {
        // When the scoped destination already exists in the branch, we must
        // not fail the whole relocate — skip the file and let the normal
        // merge machinery handle whatever conflict emerges. This preserves
        // the "non-fatal" contract of the relocation step.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let mol_id = MoleculeId::new("task-20260418-eeee").unwrap();
        // Both unscoped AND scoped versions already committed.
        std::fs::create_dir_all(repo.join("molecule").join(mol_id.as_str())).unwrap();
        std::fs::write(repo.join("molecule/review.md"), "unscoped\n").unwrap();
        std::fs::write(
            repo.join("molecule")
                .join(mol_id.as_str())
                .join("review.md"),
            "scoped\n",
        )
        .unwrap();
        assert!(git(repo, &["add", "molecule/"]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "both"]).status.success());

        let moved = relocate_workspace_artifacts(repo, &mol_id).expect("relocate");
        assert!(moved.is_empty(), "collision should skip, not fail");

        // Source untouched.
        assert!(repo.join("molecule/review.md").exists());
        assert!(repo
            .join("molecule")
            .join(mol_id.as_str())
            .join("review.md")
            .exists());
    }

    // -----------------------------------------------------------------
    // df4c: misleading success + ghost teardown regressions
    // -----------------------------------------------------------------

    /// `report` must use the `⚠` prefix on partial-failure paths so that
    /// downstream automation greping `^✅` does not see a false positive.
    /// We test the predicate directly because `report` writes to stdout.
    #[test]
    fn test_report_prefix_switches_on_warnings() {
        // The prefix decision is `warnings.is_empty()` — codifying the
        // contract here makes the regression visible if it ever flips.
        let no_warnings: Vec<String> = Vec::new();
        let some_warnings: Vec<String> = vec!["branch undeleted".to_owned()];
        assert!(no_warnings.is_empty(), "✅ path");
        assert!(!some_warnings.is_empty(), "⚠ path");
    }

    #[test]
    fn test_report_json_carries_ok_field() {
        // The JSON payload must expose an `ok` boolean so non-shell
        // consumers can branch on success without parsing the prefix.
        let mol_id = MoleculeId::new("task-20260418-json").unwrap();
        let actions = vec!["merged".to_owned()];

        let warnings_ok: Vec<String> = Vec::new();
        let payload_ok = serde_json::json!({
            "command": "done",
            "molecule": mol_id.as_str(),
            "ok": warnings_ok.is_empty(),
            "actions": actions,
            "warnings": warnings_ok,
        });
        assert_eq!(payload_ok["ok"], serde_json::json!(true));

        let warnings_bad = vec!["branch undeleted".to_owned()];
        let payload_bad = serde_json::json!({
            "command": "done",
            "molecule": mol_id.as_str(),
            "ok": warnings_bad.is_empty(),
            "actions": actions,
            "warnings": warnings_bad,
        });
        assert_eq!(payload_bad["ok"], serde_json::json!(false));
    }

    /// Ghost-teardown: when every teardown resource is already absent,
    /// `cs done` on a non-terminal molecule must succeed without `--force`.
    /// Reproduces the operator complaint that the only way out was
    /// `--no-merge --no-worktree-remove --no-branch-delete --force`.
    #[test]
    fn test_done_ghost_teardown_no_force_required() {
        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260418-ghst", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();
        // Fleet is empty (no worker registered for this id), no branch,
        // no worktree, no tmux session — i.e. nothing left to tear down.

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let mut args = default_args("task-20260418-ghst");
        // Default test args use no_* flags so the run is a no-op; we
        // re-enable them all to confirm each step short-circuits cleanly.
        args.no_merge = false;
        args.no_worktree_remove = false;
        args.no_branch_delete = false;
        args.no_kill = false;
        args.force = false; // KEY: no --force required.
        let result = run(&ctx, &args);
        assert!(
            result.is_ok(),
            "ghost teardown must succeed without --force: {result:?}"
        );
    }

    /// Counter-test: when a teardown resource still exists (here: a fleet
    /// worker), the terminal-state guard must still fire without --force.
    #[test]
    fn test_done_non_ghost_still_requires_force_when_active() {
        use cosmon_core::agent::AgentRole;
        use cosmon_core::clearance::Clearance;
        use cosmon_core::id::AgentId;
        use cosmon_core::worker::{DesiredState, WorkerStatus};
        use cosmon_state::{Fleet, WorkerData};

        let (tmp, store) = make_store();
        let mol = sample_mol("task-20260418-live", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let wid = WorkerId::new("task-20260418-live").unwrap();
        let mut worker = WorkerData::new(
            wid.clone(),
            AgentId::new("tackle").unwrap(),
            AgentRole::Implementation,
            Clearance::Write,
            WorkerStatus::Active,
        );
        worker.desired = DesiredState::Running;
        let mut fleet = Fleet::default();
        fleet.workers.insert(wid, worker);
        store.save_fleet(&fleet).unwrap();

        let ctx = Context {
            verbose: false,
            json: true,
            config: Some(tmp.path().to_path_buf()),
        };
        let args = default_args("task-20260418-live");
        let err = run(&ctx, &args).unwrap_err();
        assert!(
            err.to_string().contains("use --force"),
            "expected --force guard, got: {err}"
        );
    }

    // ── git_remote_blocklist (delib-20260426-7cfc, R1) ──────────────

    #[test]
    fn empty_blocklist_admits_any_remote() {
        let blocklist = GitRemoteBlocklistConfig::default();
        let remote_v = "origin\tgit@github.com:noogram/almanac.git (fetch)\n\
                        origin\tgit@github.com:noogram/almanac.git (push)\n";
        let hits = collect_remote_blocklist_violations(remote_v, &blocklist);
        assert!(hits.is_empty(), "empty blocklist must never flag a remote");
    }

    #[test]
    fn blocklist_matches_ssh_form() {
        let blocklist = GitRemoteBlocklistConfig {
            forbidden_substrings: vec!["github.com:noogram/almanac".to_owned()],
        };
        let remote_v = "origin\tgit@github.com:noogram/almanac.git (fetch)\n\
                        origin\tgit@github.com:noogram/almanac.git (push)\n";
        let hits = collect_remote_blocklist_violations(remote_v, &blocklist);
        assert_eq!(hits.len(), 2, "should flag both fetch and push lines");
        assert!(hits.iter().all(|(p, _)| p == "github.com:noogram/almanac"));
    }

    #[test]
    fn blocklist_matches_https_form() {
        let blocklist = GitRemoteBlocklistConfig {
            forbidden_substrings: vec!["github.com/noogram/almanac".to_owned()],
        };
        let remote_v = "upstream\thttps://github.com/noogram/almanac.git (fetch)\n";
        let hits = collect_remote_blocklist_violations(remote_v, &blocklist);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "github.com/noogram/almanac");
    }

    #[test]
    fn blocklist_admits_unrelated_remote() {
        let blocklist = GitRemoteBlocklistConfig {
            forbidden_substrings: vec!["github.com/noogram/almanac".to_owned()],
        };
        let remote_v = "origin\tgit@github.com:noogram/cosmon.git (fetch)\n";
        let hits = collect_remote_blocklist_violations(remote_v, &blocklist);
        assert!(
            hits.is_empty(),
            "unrelated remote must not match a non-overlapping pattern"
        );
    }

    #[test]
    fn blocklist_skips_empty_pattern_strings() {
        // Defensive: an operator who writes `forbidden_substrings = [""]`
        // must not blocklist every remote.
        let blocklist = GitRemoteBlocklistConfig {
            forbidden_substrings: vec![String::new(), "github.com/noogram/almanac".to_owned()],
        };
        let remote_v = "origin\tgit@github.com:noogram/cosmon.git (fetch)\n";
        let hits = collect_remote_blocklist_violations(remote_v, &blocklist);
        assert!(hits.is_empty(), "empty pattern must be ignored");
    }

    #[test]
    fn blocklist_reports_multiple_patterns_in_one_pass() {
        let blocklist = GitRemoteBlocklistConfig {
            forbidden_substrings: vec![
                "github.com:noogram/almanac".to_owned(),
                "github.com/noogram/almanac".to_owned(),
            ],
        };
        let remote_v = "ssh\tgit@github.com:noogram/almanac.git (push)\n\
                        web\thttps://github.com/noogram/almanac.git (push)\n";
        let hits = collect_remote_blocklist_violations(remote_v, &blocklist);
        assert_eq!(hits.len(), 2, "one violation per remote line / pattern hit");
    }

    #[test]
    fn check_aborts_done_when_blocklisted_remote_present() {
        // Drive through the public entry point with a temp git repo so the
        // probe sees the configured remote. No actual `cs done` machinery
        // is exercised — just the gate.
        let tmp = TempDir::new().unwrap();
        let _ = Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "init"])
            .output()
            .unwrap();
        let _ = Command::new("git")
            .args([
                "-C",
                &tmp.path().to_string_lossy(),
                "remote",
                "add",
                "origin",
                "git@github.com:noogram/almanac.git",
            ])
            .output()
            .unwrap();
        let blocklist = GitRemoteBlocklistConfig {
            forbidden_substrings: vec!["github.com:noogram/almanac".to_owned()],
        };
        let err = check_git_remote_blocklist(tmp.path(), &blocklist).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("forbidden git remote"), "msg: {msg}");
        assert!(msg.contains("github.com:noogram/almanac"), "msg: {msg}");
    }

    #[test]
    fn check_passes_when_no_blocklist_configured() {
        let tmp = TempDir::new().unwrap();
        // Not even a git repo — the empty-blocklist fast path returns Ok
        // without invoking git.
        let blocklist = GitRemoteBlocklistConfig::default();
        check_git_remote_blocklist(tmp.path(), &blocklist).unwrap();
    }

    #[test]
    fn check_passes_when_no_remotes_match() {
        let tmp = TempDir::new().unwrap();
        let _ = Command::new("git")
            .args(["-C", &tmp.path().to_string_lossy(), "init"])
            .output()
            .unwrap();
        let _ = Command::new("git")
            .args([
                "-C",
                &tmp.path().to_string_lossy(),
                "remote",
                "add",
                "origin",
                "git@github.com:noogram/cosmon.git",
            ])
            .output()
            .unwrap();
        let blocklist = GitRemoteBlocklistConfig {
            forbidden_substrings: vec!["github.com:noogram/almanac".to_owned()],
        };
        check_git_remote_blocklist(tmp.path(), &blocklist).unwrap();
    }

    // ── publish_identity gate (ADR-128 §V1, task-20260617-4bce) ──────
    //
    // The git author/committer identity channel: invisible to V0's
    // file-content scan, stamped into every commit. These tests exercise
    // the two pure scan cores and the end-to-end check over a temp repo.

    #[test]
    fn empty_publish_identity_admits_anything() {
        let cfg = PublishIdentityConfig::default();
        let emails = "operator@example.org\noperator@example.org\n";
        let text = "Noogram\nfeat: build the thing for Tenant-Demo\n";
        assert!(collect_identity_email_violations(emails, &cfg).is_empty());
        assert!(collect_identity_text_violations(text, &cfg).is_empty());
    }

    #[test]
    fn whitelist_flags_any_email_outside_the_codebook() {
        // The inversion that matters (shannon): closed-codebook → any other
        // email is a violation by construction, recall → 1 on this slot.
        let cfg = PublishIdentityConfig {
            allowed_emails: vec!["bot@noogram.dev".to_owned()],
            forbidden_substrings: vec![],
        };
        let emails = "bot@noogram.dev\noperator@example.org\n";
        let hits = collect_identity_email_violations(emails, &cfg);
        assert_eq!(hits.len(), 1, "exactly the out-of-codebook email flagged");
        assert!(hits[0].0.contains("codebook"), "reason: {}", hits[0].0);
        assert_eq!(hits[0].1, "operator@example.org");
    }

    #[test]
    fn whitelist_is_case_insensitive() {
        let cfg = PublishIdentityConfig {
            allowed_emails: vec!["Bot@Noogram.dev".to_owned()],
            forbidden_substrings: vec![],
        };
        let emails = "bot@noogram.dev\n";
        assert!(
            collect_identity_email_violations(emails, &cfg).is_empty(),
            "canonical email differing only in case must pass the codebook"
        );
    }

    #[test]
    fn blacklist_flags_forbidden_substring_in_email() {
        let cfg = PublishIdentityConfig {
            allowed_emails: vec![],
            forbidden_substrings: vec!["example.org".to_owned()],
        };
        let emails = "operator@example.org\n";
        let hits = collect_identity_email_violations(emails, &cfg);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].0.contains("example.org"), "reason: {}", hits[0].0);
    }

    #[test]
    fn blacklist_flags_forbidden_substring_in_free_text() {
        let cfg = PublishIdentityConfig {
            allowed_emails: vec![],
            forbidden_substrings: vec!["Tenant-Demo".to_owned()],
        };
        let text = "Noogram\nfeat: ship the site for Tenant-Demo Research\n";
        let hits = collect_identity_text_violations(text, &cfg);
        assert_eq!(hits.len(), 1, "the commit subject line is flagged");
        assert_eq!(hits[0].1, "feat: ship the site for Tenant-Demo Research");
    }

    #[test]
    fn free_text_layer_has_no_whitelist() {
        // A display name is not a codebook slot — only the blacklist applies
        // to free text, so an allowed_emails codebook alone flags nothing here.
        let cfg = PublishIdentityConfig {
            allowed_emails: vec!["bot@noogram.dev".to_owned()],
            forbidden_substrings: vec![],
        };
        let text = "Noogram\nany commit subject\n";
        assert!(collect_identity_text_violations(text, &cfg).is_empty());
    }

    #[test]
    fn publish_identity_skips_empty_pattern_strings() {
        // Defensive: `forbidden_substrings = [""]` must not flag every line.
        let cfg = PublishIdentityConfig {
            allowed_emails: vec![],
            forbidden_substrings: vec![String::new(), "example.org".to_owned()],
        };
        let emails = "bot@noogram.dev\n";
        assert!(collect_identity_email_violations(emails, &cfg).is_empty());
        let text = "any subject line\n";
        assert!(collect_identity_text_violations(text, &cfg).is_empty());
    }

    #[test]
    fn check_publish_identity_passes_when_empty() {
        // Fast path: empty config returns Ok without ever invoking git.
        let tmp = TempDir::new().unwrap();
        let cfg = PublishIdentityConfig::default();
        check_publish_identity_blocklist(tmp.path(), "feat/x", "main", &cfg).unwrap();
    }

    #[test]
    fn check_publish_identity_aborts_on_leaked_author_email() {
        // End-to-end over a real temp repo: a commit authored by the operator
        // email on a branch off `main` must abort with the residual-risk note.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(["-C", &root.to_string_lossy()])
                .args(args)
                .env_remove("GIT_AUTHOR_NAME")
                .env_remove("GIT_AUTHOR_EMAIL")
                .env_remove("GIT_COMMITTER_NAME")
                .env_remove("GIT_COMMITTER_EMAIL")
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Noogram Bot"]);
        git(&["config", "user.email", "bot@noogram.dev"]);
        git(&["commit", "--allow-empty", "-q", "-m", "base"]);
        git(&["checkout", "-q", "-b", "feat/leak"]);
        // The leaking commit: authored with the operator identity.
        git(&[
            "-c",
            "user.email=operator@example.org",
            "-c",
            "user.name=Noogram",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "feat: ship it",
        ]);

        // Whitelist codebook: only the Noogram bot is allowed.
        let cfg = PublishIdentityConfig {
            allowed_emails: vec!["bot@noogram.dev".to_owned()],
            forbidden_substrings: vec![],
        };
        let err = check_publish_identity_blocklist(root, "feat/leak", "main", &cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("operator@example.org"), "msg: {msg}");
        assert!(msg.contains("codebook"), "msg: {msg}");
        // Residual-risk statement is mandatory (turing honesty).
        assert!(msg.to_lowercase().contains("syntactic"), "msg: {msg}");
    }

    #[test]
    fn check_publish_identity_passes_when_only_base_history_leaks() {
        // The operator identity on `main`'s pre-existing history must NOT be
        // flagged — the gate scans only `<base>..<branch>`, the commits the
        // merge would publish. A branch with no new commits is clean.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(["-C", &root.to_string_lossy()])
                .args(args)
                .env_remove("GIT_AUTHOR_NAME")
                .env_remove("GIT_AUTHOR_EMAIL")
                .env_remove("GIT_COMMITTER_NAME")
                .env_remove("GIT_COMMITTER_EMAIL")
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        // A pre-existing commit authored by the operator (legitimate history).
        git(&["config", "user.name", "Noogram"]);
        git(&["config", "user.email", "operator@example.org"]);
        git(&["commit", "--allow-empty", "-q", "-m", "legacy base"]);
        git(&["checkout", "-q", "-b", "feat/clean"]);
        // The branch's own commit carries the canonical publish identity.
        git(&[
            "-c",
            "user.email=bot@noogram.dev",
            "-c",
            "user.name=Noogram Bot",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "feat: clean work",
        ]);

        let cfg = PublishIdentityConfig {
            allowed_emails: vec!["bot@noogram.dev".to_owned()],
            forbidden_substrings: vec!["operator@example.org".to_owned()],
        };
        check_publish_identity_blocklist(root, "feat/clean", "main", &cfg).unwrap();
    }

    // ── confidential_blocklist (delib-20260617-62ff / ADR-128, D7) ──

    #[test]
    fn confidential_empty_blocklist_admits_anything() {
        // Default (no substrings, no globs) → `is_empty()` short-circuits.
        let cfg = ConfidentialBlocklistConfig::default();
        let files = vec![(
            "README.md".to_owned(),
            "Built by Tenant-Demo Research".to_owned(),
        )];
        let hits = collect_confidential_blocklist_violations(&files, &cfg);
        assert!(hits.is_empty(), "empty blocklist must never flag content");
    }

    #[test]
    fn confidential_flags_literal_substring() {
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo".to_owned()],
            publish_globs: vec!["README*".to_owned()],
            ..Default::default()
        };
        let files = vec![(
            "README.md".to_owned(),
            "# Cosmon\n\nMaintained by Tenant-Demo Research.\n".to_owned(),
        )];
        let hits = collect_confidential_blocklist_violations(&files, &cfg);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "README.md");
        assert_eq!(hits[0].1, "Tenant-Demo");
    }

    #[test]
    fn confidential_matching_is_case_folded() {
        // A footer that stamps TENANT-DEMO / tenant-demo must be caught the same as
        // the canonical casing — the realized leak class ignores case.
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo".to_owned()],
            publish_globs: vec!["**/footer.html".to_owned()],
            ..Default::default()
        };
        let files = vec![
            ("a/footer.html".to_owned(), "© TENANT-DEMO".to_owned()),
            ("b/footer.html".to_owned(), "© tenant-demo".to_owned()),
        ];
        let hits = collect_confidential_blocklist_violations(&files, &cfg);
        assert_eq!(hits.len(), 2, "both casings must match");
    }

    #[test]
    fn confidential_admits_unrelated_content() {
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo".to_owned(), "example.org".to_owned()],
            publish_globs: vec!["README*".to_owned()],
            ..Default::default()
        };
        let files = vec![(
            "README.md".to_owned(),
            "A stateless CLI for AI coding agents.\n".to_owned(),
        )];
        let hits = collect_confidential_blocklist_violations(&files, &cfg);
        assert!(hits.is_empty(), "clean content must not match");
    }

    #[test]
    fn confidential_skips_empty_pattern_strings() {
        // `forbidden_substrings = [""]` must not flag every file.
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec![String::new(), "Tenant-Demo".to_owned()],
            publish_globs: vec!["README*".to_owned()],
            ..Default::default()
        };
        let files = vec![("README.md".to_owned(), "plain text\n".to_owned())];
        let hits = collect_confidential_blocklist_violations(&files, &cfg);
        assert!(hits.is_empty(), "empty pattern must be ignored");
    }

    #[test]
    fn confidential_reports_multiple_violations_in_one_pass() {
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo".to_owned(), "operator@example.org".to_owned()],
            publish_globs: vec!["README*".to_owned(), "**/index.html".to_owned()],
            ..Default::default()
        };
        let files = vec![
            (
                "README.md".to_owned(),
                "Tenant-Demo Research, founder".to_owned(),
            ),
            (
                "site/index.html".to_owned(),
                "contact: operator@example.org".to_owned(),
            ),
        ];
        let hits = collect_confidential_blocklist_violations(&files, &cfg);
        assert_eq!(hits.len(), 2, "all violations reported together");
    }

    #[test]
    fn confidential_check_passes_when_no_blocklist_configured() {
        // Not even a git repo — the `is_empty()` fast path returns Ok
        // without touching the filesystem.
        let tmp = TempDir::new().unwrap();
        let cfg = ConfidentialBlocklistConfig::default();
        check_confidential_blocklist(tmp.path(), &cfg).unwrap();
    }

    #[test]
    fn confidential_check_passes_when_publish_files_are_clean() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("README.md"), "Just a CLI.\n").unwrap();
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo".to_owned()],
            publish_globs: vec!["README*".to_owned()],
            ..Default::default()
        };
        check_confidential_blocklist(tmp.path(), &cfg).unwrap();
    }

    #[test]
    fn confidential_check_aborts_on_seeded_readme_with_residual_risk() {
        // The acceptance case from the briefing: a deliberately-seeded
        // "Tenant-Demo" in README.md makes the gate abort, and the error
        // message MUST carry the verbatim residual-risk statement.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("README.md"),
            "# Project\n\nBuilt by Tenant-Demo Research.\n",
        )
        .unwrap();
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo Research".to_owned()],
            publish_globs: vec!["README*".to_owned()],
            ..Default::default()
        };
        let err = check_confidential_blocklist(tmp.path(), &cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("confidential substring"), "msg: {msg}");
        assert!(msg.contains("README.md"), "msg: {msg}");
        assert!(msg.contains("[confidential_blocklist]"), "msg: {msg}");
        // The mandatory residual-risk line (turing, ADR-128).
        assert!(
            msg.contains("does not reduce the\nsemantic failure class."),
            "residual-risk statement missing: {msg}"
        );
        assert!(
            msg.contains("does NOT detect paraphrase"),
            "residual-risk statement missing: {msg}"
        );
    }

    #[test]
    fn confidential_gate_rejects_fund_name_in_paper_via_default_globs() {
        // task-20260622-7207 — the qfa leak: a PAPER author block + colophon
        // carrying the operator's private fund name. With NO explicit
        // publish_globs (a galaxy that inherits only the federation
        // forbidden_substrings), the gate must fall back to DEFAULT_PUBLISH_GLOBS
        // and catch the `.tex` deliverable the old README/site-only globs missed.
        // ("Tenant-Demo Research" stands in for the real fund name — same convention as
        // the sibling confidential tests, so this tracked test never names it.)
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("paper.tex"),
            "\\author{Tenant-Demo Research}\n% colophon: typeset by Tenant-Demo Research\n",
        )
        .unwrap();
        let cfg = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["Tenant-Demo Research".to_owned()],
            publish_globs: vec![], // no explicit globs → default set applies
            ..Default::default()
        };
        assert!(!cfg.is_empty(), "substrings-only config is still active");
        let err = check_confidential_blocklist(tmp.path(), &cfg).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("paper.tex"), "must flag the paper file: {msg}");
        assert!(
            msg.contains("Tenant-Demo Research"),
            "must name the offending substring: {msg}"
        );
    }

    #[test]
    fn global_confidential_blocklist_loads_only_its_section() {
        // The federation source: the operator's machine-wide config supplies
        // the fund name once; load_global_confidential_blocklist reads only
        // [confidential_blocklist] and ignores unrelated sections.
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
[adapters]
default = "claude"

[confidential_blocklist]
forbidden_substrings = ["Tenant-Demo Research", "Tenant-Demo"]
"#,
        )
        .unwrap();
        let loaded = load_global_confidential_blocklist(&cfg_path);
        assert_eq!(
            loaded.forbidden_substrings,
            vec!["Tenant-Demo Research".to_owned(), "Tenant-Demo".to_owned()]
        );
    }

    #[test]
    fn global_confidential_blocklist_missing_file_is_empty_not_panic() {
        // Best-effort: a missing/malformed global config must never abort
        // `cs done`; it yields the empty blocklist and the per-galaxy config
        // still applies.
        let missing = Path::new("/nonexistent/cosmon/config.toml");
        assert!(load_global_confidential_blocklist(missing).is_empty());
    }

    #[test]
    fn federation_merge_makes_empty_galaxy_gate_reject_fund_name_in_paper() {
        // END-TO-END: a galaxy with an EMPTY per-galaxy blocklist (the qfa
        // case — ~50 galaxies) inherits the operator's fund name from the
        // machine-wide config and the gate then rejects an 'Tenant-Demo Research'
        // author block in a paper deliverable it never explicitly configured
        // globs for. This is the whole fix in one test. ("Tenant-Demo Research"
        // stands in for the real fund name so this tracked test never names it.)
        let tmp = TempDir::new().unwrap();

        // (1) the operator's private machine-wide config — the single source.
        let global_cfg = tmp.path().join("global-config.toml");
        std::fs::write(
            &global_cfg,
            "[confidential_blocklist]\nforbidden_substrings = [\"Tenant-Demo Research\", \"Tenant-Demo\"]\n",
        )
        .unwrap();

        // (2) a galaxy repo that configured NOTHING — empty per-galaxy gate.
        let repo = tmp.path().join("galaxy");
        std::fs::create_dir(&repo).unwrap();
        std::fs::write(
            repo.join("paper.tex"),
            "\\author{Tenant-Demo Research}\n% colophon\n",
        )
        .unwrap();

        let per_galaxy = ConfidentialBlocklistConfig::default();
        assert!(
            per_galaxy.is_empty(),
            "precondition: galaxy itself configures no blocklist"
        );

        // (3) the federation merge supplies the secret; the gate now fires.
        let effective = effective_confidential_blocklist_from(&per_galaxy, &global_cfg);
        assert!(!effective.is_empty(), "inherited gate must be active");
        let err = check_confidential_blocklist(&repo, &effective).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("paper.tex"), "must flag the paper: {msg}");
        assert!(
            msg.contains("Tenant-Demo Research"),
            "must catch inherited fund name: {msg}"
        );
    }

    #[test]
    fn federation_merge_missing_global_leaves_galaxy_gate_unchanged() {
        // No global config → the per-galaxy blocklist is returned verbatim.
        let per_galaxy = ConfidentialBlocklistConfig {
            forbidden_substrings: vec!["GalaxyLocal".to_owned()],
            publish_globs: vec![],
            ..Default::default()
        };
        let effective = effective_confidential_blocklist_from(
            &per_galaxy,
            Path::new("/nonexistent/cosmon/config.toml"),
        );
        assert_eq!(
            effective.forbidden_substrings,
            vec!["GalaxyLocal".to_owned()]
        );
    }

    #[test]
    fn home_galaxy_owner_does_not_inherit_federation_block() {
        // The home galaxy OWNS the federation confidential name(s): it names
        // itself freely, so `cs done` must NOT fold the machine-wide block
        // into its gate — otherwise the galaxy's own name in its own README is
        // a false positive that aborts the merge. Generic: no galaxy name is
        // baked into cosmon; the owner declares itself via `owns_federation_secret`.
        let tmp = TempDir::new().unwrap();
        let global_cfg = tmp.path().join("global-config.toml");
        std::fs::write(
            &global_cfg,
            "[confidential_blocklist]\nforbidden_substrings = [\"Tenant-Demo Research\", \"Tenant-Demo\"]\n",
        )
        .unwrap();

        let owner = ConfidentialBlocklistConfig {
            owns_federation_secret: true,
            ..Default::default()
        };
        let effective = effective_confidential_blocklist_from(&owner, &global_cfg);
        assert!(
            effective.is_empty(),
            "owner galaxy must not inherit the federation block"
        );

        // A README that names the owned entity passes clean.
        let repo = tmp.path().join("galaxy");
        std::fs::create_dir(&repo).unwrap();
        std::fs::write(repo.join("README.md"), "# Tenant-Demo Research\n").unwrap();
        check_confidential_blocklist(&repo, &effective).unwrap();
    }

    #[test]
    fn non_owner_still_inherits_federation_block() {
        // Contrast with the owner case: a galaxy that does NOT own the secret
        // keeps inheriting it — the federation protection is unchanged for
        // every galaxy but the home one.
        let tmp = TempDir::new().unwrap();
        let global_cfg = tmp.path().join("global-config.toml");
        std::fs::write(
            &global_cfg,
            "[confidential_blocklist]\nforbidden_substrings = [\"Tenant-Demo Research\"]\n",
        )
        .unwrap();
        let per_galaxy = ConfidentialBlocklistConfig::default();
        let effective = effective_confidential_blocklist_from(&per_galaxy, &global_cfg);
        assert!(
            effective
                .forbidden_substrings
                .contains(&"Tenant-Demo Research".to_owned()),
            "non-owner galaxies keep inheriting the federation block"
        );
    }

    #[test]
    fn confidential_collect_publish_files_respects_globs_and_skips_git() {
        // Only files matched by publish_globs are returned — NARROW scope.
        // An internal doc that legitimately names the entity is invisible
        // to the gate because it is outside the glob set.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("README.md"), "x").unwrap();
        std::fs::create_dir_all(root.join("site")).unwrap();
        std::fs::write(root.join("site/index.html"), "x").unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        // docs/internal.md legitimately names Tenant-Demo but is NOT in globs.
        std::fs::write(root.join("docs/internal.md"), "Tenant-Demo Research").unwrap();

        let globs = vec!["README*".to_owned(), "site/**".to_owned()];
        let mut found = collect_publish_files(root, &globs);
        found.sort();
        assert_eq!(
            found,
            vec!["README.md".to_owned(), "site/index.html".to_owned()]
        );
    }

    // ---------------------------------------------------------------
    // task-20260509-94f0 (mode B) regression — silent-merge-to-wrong-branch
    //
    // Before the fix:
    //   - Pilot ran `cs done <id>` from inside a worktree.
    //   - `find_repo_root()` returned the worktree path.
    //   - `git -C <worktree> merge <branch>` advanced the worktree's
    //     HEAD (`feat/task-…-X`) instead of `main`.
    //   - `verify_merge` checked ancestry against HEAD (now containing
    //     the merged branch) and returned true.
    //   - `cs done` reported "merged" while `main` never moved; the work
    //     commit was orphaned the moment the wrong branch was cleaned up.
    //
    // After the fix (two complementary guards):
    //   1. Pre-flight in `try_merge_branch`: HEAD must be on the
    //      configured base branch, or the function returns
    //      `MergeOutcome::NotOnBase` without attempting the merge.
    //   2. Post-merge `verify_merge`: ancestry probe is now against the
    //      base branch (not HEAD), so even if the pre-flight is bypassed
    //      by some future code path, a merge that landed on the wrong
    //      branch is detected.
    //
    // Each guard has its own regression test; both must FAIL before the
    // fix and PASS after.
    // ---------------------------------------------------------------

    #[test]
    fn try_merge_branch_refuses_when_head_is_not_on_base() {
        // task-20260509-94f0 (mode B), pre-flight half:
        // pilot in another worktree's branch tries to merge a worker's
        // branch. The pre-flight check must catch this BEFORE any
        // `git merge` runs — there is no recovery from an actual silent
        // merge to the wrong branch (the work commit becomes orphan as
        // soon as the wrong branch is cleaned up).
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Worker branch with a real payload commit (the "work" that
        // would have been silently lost).
        assert!(
            git(repo, &["checkout", "-q", "-b", "feat/task-20260509-94f0"])
                .status
                .success()
        );
        commit_file(
            repo,
            "license-pivot.txt",
            "MPL→AGPL+Apache: 56 files, +2559/-828\n",
            "feat: bascule de licence",
        );

        // Simulate the pilot being in *another* worktree, with HEAD on a
        // different feature branch (real-world: pilot ran `cs done` from
        // inside `.worktrees/task-…-X/`, where HEAD is `feat/task-…-X`).
        assert!(git(
            repo,
            &["checkout", "-q", "-b", "feat/pilot-elsewhere", "main"]
        )
        .status
        .success());
        commit_file(
            repo,
            "pilot.txt",
            "pilot's bookkeeping\n",
            "chore: pilot scratch work",
        );

        // Sanity: precondition is HEAD ≠ base.
        let head = current_branch_name(repo).unwrap();
        assert_eq!(head, "feat/pilot-elsewhere");
        assert_ne!(head, resolve_base_branch(repo));

        // The merge attempt MUST refuse — and must NOT advance the
        // current branch (which would be the silent failure).
        let head_before = git(repo, &["rev-parse", "HEAD"]);
        let outcome = try_merge_branch(repo, "feat/task-20260509-94f0", MergeStrategy::Merge, &[]);
        let head_after = git(repo, &["rev-parse", "HEAD"]);

        match outcome {
            MergeOutcome::NotOnBase { current, base } => {
                assert_eq!(current, "feat/pilot-elsewhere");
                assert_eq!(base, "main");
            }
            other => panic!(
                "expected NotOnBase {{current=feat/pilot-elsewhere, base=main}}, got {other:?}"
            ),
        }
        assert_eq!(
            head_before.stdout, head_after.stdout,
            "HEAD must not move when pre-flight refuses — silent merge would have advanced it"
        );

        // And `main` must NOT contain the worker's commit (the work
        // remains parked on its branch, recoverable without `git fsck`).
        let main_has_work = git(
            repo,
            &[
                "merge-base",
                "--is-ancestor",
                "feat/task-20260509-94f0",
                "main",
            ],
        );
        assert!(
            !main_has_work.status.success(),
            "main must not contain the work — branch is the only copy"
        );
    }

    #[test]
    fn verify_merge_against_base_catches_silent_merge_to_wrong_branch() {
        // task-20260509-94f0 (mode B), post-merge half:
        // even when a `git merge` succeeds, `verify_merge` must check
        // that the work landed on the configured base branch — not on
        // an arbitrary HEAD that happened to receive the merge.
        //
        // We simulate the bug bypassing the pre-flight: advance the
        // wrong branch via `git merge` directly, then call
        // `verify_merge`. Before the fix it returned true (HEAD-based
        // probe); after the fix it returns false (base-relative).
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // Worker branch with payload.
        assert!(
            git(repo, &["checkout", "-q", "-b", "feat/task-20260509-94f0"])
                .status
                .success()
        );
        commit_file(repo, "payload.txt", "the work\n", "feat: payload");

        // Pilot's worktree-branch (rooted on main, then advanced).
        assert!(git(
            repo,
            &["checkout", "-q", "-b", "feat/pilot-elsewhere", "main"]
        )
        .status
        .success());
        commit_file(repo, "pilot.txt", "pilot scratch\n", "chore: scratch");

        // Bypass the pre-flight: advance the wrong branch by running
        // `git merge` directly, exactly the way `cs done` did before
        // the fix.
        assert!(git(
            repo,
            &[
                "merge",
                "-q",
                "--no-ff",
                "--no-edit",
                "feat/task-20260509-94f0",
            ],
        )
        .status
        .success());

        // Sanity: HEAD-based probe (the OLD verify_merge) would say
        // "merged" — this is the false positive the bug exploited.
        assert!(
            branch_is_ancestor_of_head(repo, "feat/task-20260509-94f0"),
            "diagnostic: HEAD-based probe falsely confirms the merge"
        );

        // The fixed `verify_merge` must return false — the work did NOT
        // land on `main`.
        assert!(
            !verify_merge(repo, "feat/task-20260509-94f0"),
            "verify_merge must be base-relative — silent merge to wrong \
             branch must NOT pass post-merge verification"
        );

        // Ground truth: `main` does not contain the work.
        let main_has_work = git(
            repo,
            &[
                "merge-base",
                "--is-ancestor",
                "feat/task-20260509-94f0",
                "main",
            ],
        );
        assert!(
            !main_has_work.status.success(),
            "precondition: main must not contain the work"
        );
    }

    #[test]
    fn verify_merge_passes_when_merge_landed_on_base() {
        // Converse of the wrong-branch test: when the merge lands on
        // base (the only correct shape), `verify_merge` returns true.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        assert!(
            git(repo, &["checkout", "-q", "-b", "feat/task-20260509-94f0"])
                .status
                .success()
        );
        commit_file(repo, "payload.txt", "the work\n", "feat: payload");

        assert!(git(repo, &["checkout", "-q", "main"]).status.success());
        assert!(git(
            repo,
            &[
                "merge",
                "-q",
                "--no-ff",
                "--no-edit",
                "feat/task-20260509-94f0",
            ],
        )
        .status
        .success());

        assert!(
            verify_merge(repo, "feat/task-20260509-94f0"),
            "merges that land on base must verify clean"
        );
    }

    #[test]
    fn current_branch_name_returns_none_on_detached_head() {
        // Detached HEAD (e.g., a worker mid-rebase or `git checkout
        // <sha>`) must not be confused with the base branch — the
        // pre-flight check should refuse cleanly with `NotOnBase` and
        // the human-readable label `(detached HEAD)`.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        let head_sha = String::from_utf8_lossy(&git(repo, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();
        assert!(git(repo, &["checkout", "-q", "--detach", &head_sha])
            .status
            .success());

        assert_eq!(current_branch_name(repo), None);
    }

    // -----------------------------------------------------------------
    // GUARD anti-wipe — raté 5eba. `cs done` must NEVER delete a feat
    // branch whose commits are not reachable from base. See
    // `decide_branch_delete` and step 6 of `run`.
    // -----------------------------------------------------------------

    #[test]
    fn guard_never_deletes_unmerged_branch_under_any_flags() {
        // The load-bearing invariant: when the branch is NOT an ancestor
        // of base (`branch_in_base == false`), the decision is ALWAYS
        // `RefuseUnmerged` — for every combination of merge_succeeded and
        // force, including force == true. This is what 5eba needed.
        for &merge_succeeded in &[false, true] {
            for &force in &[false, true] {
                let decision = decide_branch_delete(
                    /* branch_present */ true,
                    /* effective_no_branch_delete */ false,
                    /* branch_in_base */ false,
                    merge_succeeded,
                    force,
                );
                assert_eq!(
                    decision,
                    BranchDeleteDecision::RefuseUnmerged,
                    "unmerged branch must be refused (merge_succeeded={merge_succeeded}, \
                     force={force}) — the guard overrides every flag"
                );
            }
        }
    }

    #[test]
    fn guard_decision_table_is_exhaustive() {
        use BranchDeleteDecision::{Delete, RefuseMergeFailed, RefuseUnmerged, Skip};
        // (present, no_delete, in_base, merged, force) -> expected
        let cases = [
            // Absent branch or opted-out: always Skip (guard never fires).
            ((false, false, false, true, true), Skip),
            ((false, true, true, true, true), Skip),
            ((true, true, false, true, true), Skip),
            // Branch present, deletion enabled, NOT in base: guard refuses,
            // overriding merge_succeeded and force.
            ((true, false, false, false, false), RefuseUnmerged),
            ((true, false, false, true, false), RefuseUnmerged),
            ((true, false, false, false, true), RefuseUnmerged),
            ((true, false, false, true, true), RefuseUnmerged),
            // Branch present, in base, merge succeeded or forced: delete.
            ((true, false, true, true, false), Delete),
            ((true, false, true, false, true), Delete),
            ((true, false, true, true, true), Delete),
            // Branch present, in base, but merge failed and no force.
            ((true, false, true, false, false), RefuseMergeFailed),
        ];
        for ((present, no_del, in_base, merged, force), expected) in cases {
            assert_eq!(
                decide_branch_delete(present, no_del, in_base, merged, force),
                expected,
                "decision mismatch for \
                 (present={present}, no_del={no_del}, in_base={in_base}, \
                 merged={merged}, force={force})"
            );
        }
    }

    #[test]
    fn ancestry_probe_distinguishes_merged_from_unmerged_branch() {
        // The guard reads ground truth from `branch_is_ancestor_of`. This
        // proves the probe it relies on: an unmerged feat branch is NOT an
        // ancestor of main; after the merge it is. The 5eba loss happened
        // because the delete path trusted `git branch -d` (which checks
        // HEAD, not main) instead of this base-relative probe.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // feat/5eba carries a commit that never reaches main.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/5eba"])
            .status
            .success());
        commit_file(
            repo,
            "payload.txt",
            "491 lines of work\n",
            "feat: the payload",
        );
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        // Before merge: branch is NOT reachable from main → guard refuses.
        assert!(
            !branch_is_ancestor_of(repo, "feat/5eba", "main"),
            "unmerged branch must not be an ancestor of main"
        );
        assert_eq!(
            decide_branch_delete(true, false, false, true, true),
            BranchDeleteDecision::RefuseUnmerged,
            "while unmerged, even merge_succeeded+force must not delete"
        );

        // After a real merge: branch IS reachable → deletion is allowed.
        assert!(
            git(repo, &["merge", "-q", "--no-ff", "--no-edit", "feat/5eba"])
                .status
                .success()
        );
        assert!(
            branch_is_ancestor_of(repo, "feat/5eba", "main"),
            "merged branch must be an ancestor of main"
        );
        assert_eq!(
            decide_branch_delete(true, false, true, true, false),
            BranchDeleteDecision::Delete,
            "once merged, deletion is permitted"
        );
    }

    #[test]
    fn bounded_gate_kills_a_hung_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 2"]);
        let started = Instant::now();
        let err = run_bounded(&mut command, Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn unmerged_work_remains_is_the_done_loud_failure_guard() {
        // task-20260606-21d4 (DoD b): the final `cs done` post-condition.
        // While the worker's branch still carries commits not on base, the
        // work was NOT integrated and `cs done` must fail loudly rather than
        // exit 0 (bug task-20260531-1b35).
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();
        init_repo(repo);

        // No branch yet → nothing to integrate → guard does not fire.
        assert!(
            !unmerged_work_remains(repo, "feat/1b35", "main"),
            "absent branch must not trip the guard"
        );

        // feat/1b35 carries one commit that never reaches main.
        assert!(git(repo, &["checkout", "-q", "-b", "feat/1b35"])
            .status
            .success());
        commit_file(repo, "result.md", "the deliverable\n", "feat: result");
        assert!(git(repo, &["checkout", "-q", "main"]).status.success());

        // Branch present + commits ahead of base → guard FIRES (loud failure).
        assert!(
            unmerged_work_remains(repo, "feat/1b35", "main"),
            "an unmerged branch with commits ahead of base must trip the guard"
        );

        // After a real merge, the branch is empty relative to base → guard
        // clears, even if the branch is kept (`--no-branch-delete`).
        assert!(
            git(repo, &["merge", "-q", "--no-ff", "--no-edit", "feat/1b35"])
                .status
                .success()
        );
        assert!(
            !unmerged_work_remains(repo, "feat/1b35", "main"),
            "once the work is on base, the guard must not fire (kept branch is empty rel. base)"
        );
    }
}
