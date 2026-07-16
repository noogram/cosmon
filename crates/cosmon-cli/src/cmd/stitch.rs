// SPDX-License-Identifier: AGPL-3.0-only

//! `cs stitch <root-id>` — fleet-locked sequential merge of a mission's
//! children into the base branch.
//!
//! Option D' of the single-writer-trunk orchestration ADR. When several
//! workers complete in parallel under one mission, the manual "for each
//! leaf: `git switch main && git merge --no-ff <branch>`" dance is
//! replaced by one locked batch that respects the DAG.
//!
//! # Distinct perimeter
//!
//! Different from `cs done`:
//!
//! - operates on a **root** molecule, not a single leaf
//! - merges the full DAG closure in **topological order** (leaves → root)
//! - holds the **trunk lock** for the entire batch (single-writer-trunk)
//! - no teardown — only merges; worktrees, branches, tmux sessions stay
//! - on a textual conflict or a **failed `--cargo-check` gate**, the
//!   offending molecule's lineage is dropped but **independent DAG
//!   branches keep stitching** (no global halt — see [`upstream_poisoned`])
//!
//! # The gate is a gate (delib post-mortem, 2026-06-11)
//!
//! `--cargo-check` is a *blocking* gate, not a label. When the check is
//! red after a merge, the merge is **rolled back to its pre-merge SHA**
//! (`git reset --hard`), the push is withheld, and the run exits
//! non-zero. The earlier behaviour — keep the merge, push anyway, note
//! `cargo check failed (merge kept)` — pushed a broken trunk on the
//! `avatar-surface` wave-1 stitch (E0277 on `MoleculeLink`); a gate that
//! does not block is not a gate. Green merges that already landed stay;
//! only the failing lineage unwinds, so trunk only ever carries
//! compile-clean commits.
//!
//! # Untracked debris ≠ conflict
//!
//! A worker may leave an untracked file in the base worktree (e.g.
//! `run_bounds.rs` debris) that collides with a tracked file the merging
//! branch introduces. Git refuses the merge with *"untracked working
//! tree files would be overwritten"* — this is **not** a content
//! conflict (no `<<<<<<<` markers, nothing in `diff --diff-filter=U`).
//! `cs stitch` distinguishes the two: if the untracked file is a
//! byte-identical duplicate of the branch's version it is discarded and
//! the merge retried; otherwise the molecule is reported as
//! [`StitchStatus::UntrackedOverwrite`] for the operator to resolve.
//!
//! Reuses [`compile_plan`] + [`toposort`] for the DAG walk and
//! [`FileStore::with_trunk_lock`] for the mutex window. `cs stitch` is the
//! canonical trunk writer (ADR-110) and rewrites **no** fleet/molecule state,
//! so it holds the trunk lock **alone** — never the fleet lock. That is the
//! deadlock-free fix (option 1) proven by `smithy/docs/formal/MCStitch.tla`:
//! since the stitcher never takes the fleet lock, it can never be the
//! fleet-side of a `fleet ⊃ trunk` inversion against `cs done`'s
//! `trunk ⊃ fleet`. The global order stays a single total order.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_graph::toposort;
use cosmon_runtime::compile_plan;
use cosmon_state::StateStore;

use super::Context;

/// Arguments for the `stitch` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Root molecule id whose DAG closure should be stitched into the
    /// base branch (`main` by default; override via `COSMON_BASE_BRANCH`).
    pub root: String,

    /// Run a `cargo check` after every successful merge — minimal gate
    /// that the tree still compiles before moving to the next branch.
    /// Off by default because not every cosmon-tracked repo is a cargo
    /// workspace; opt in when the perimeter is Rust.
    #[arg(long)]
    pub cargo_check: bool,

    /// Push to `origin <base>` after each successful merge. Off by
    /// default — many cosmon repos run without a remote. Failures are
    /// surfaced as warnings on the row but do not abort the batch.
    #[arg(long)]
    pub push: bool,

    /// Compute the merge plan without touching git. Reports what
    /// `cs stitch` would do, in what order, and which branches it
    /// considers ready (completed + branch present + not already merged).
    #[arg(long)]
    pub dry_run: bool,
}

/// One row of the `cs stitch` output table.
///
/// Field order is the column order. Stable for human and JSON
/// renderings — operators grep both, and the JSON payload is consumed
/// downstream by `cs evolve` callers in the orchestration tree.
#[derive(Debug, serde::Serialize)]
pub struct StitchRow {
    /// Molecule id (full form).
    pub molecule: String,
    /// Per-molecule outcome — see [`StitchStatus`].
    pub status_merge: StitchStatus,
    /// Conflicting files when `status_merge = "conflict"`, else empty.
    pub conflict_files: Vec<String>,
    /// Human-readable note (skip reason, push warning, …). Empty on
    /// the happy path.
    pub note: String,
}

/// Per-molecule stitch outcome.
///
/// `serde` rendering is lower-snake so the JSON payload is stable for
/// downstream parsers; `Display` matches it for human renderings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StitchStatus {
    /// Branch was merged into base (fresh `git merge --no-ff` commit).
    Merged,
    /// Branch tip is already reachable from base (prior merge, or empty
    /// branch — both shapes look identical to git topology).
    AlreadyMerged,
    /// Molecule has no `feat/<id>` branch — typically a deliberation,
    /// idea, or a mission whose work landed in children only.
    NoBranch,
    /// Molecule is not in a terminal state — skipped without merging.
    NotCompleted,
    /// `git merge` reported a textual conflict; the merge has been
    /// aborted (`git merge --abort`). This molecule's downstream lineage
    /// is skipped, but independent branches keep stitching.
    Conflict,
    /// `git merge` was refused because untracked working-tree files
    /// would be overwritten, and at least one of them is **not** a
    /// byte-identical duplicate of the branch's version — so it cannot be
    /// safely discarded. Distinct from [`Self::Conflict`]: there are no
    /// merge markers, nothing in `diff --diff-filter=U`. The operator
    /// must move or remove the listed debris and rerun.
    UntrackedOverwrite,
    /// The post-merge `--cargo-check` gate failed; the merge was rolled
    /// back to its pre-merge SHA (`git reset --hard`) and the push (if
    /// any) was withheld. The gate blocks — trunk stays compile-clean.
    CheckFailed,
    /// Skipped because an upstream dependency in the DAG failed
    /// (conflict, untracked-overwrite, or check-failure). Merging this
    /// molecule would land work that builds on an unmerged predecessor.
    SkippedUpstreamFailed,
    /// Plan-only (set in `--dry-run` for entries that *would* merge).
    PlannedMerge,
    /// Plan-only (would skip).
    PlannedSkip,
}

impl StitchStatus {
    /// Whether this outcome poisons the molecule's downstream lineage.
    ///
    /// A molecule that conflicted, was blocked by untracked debris, or
    /// failed the compile gate did **not** land on trunk. Anything that
    /// depends on it (transitively, via `BlockedBy`) must not be merged
    /// either — it would build on an absent predecessor. Independent
    /// branches are unaffected. `SkippedUpstreamFailed` is itself a
    /// downstream consequence and propagates further.
    fn is_failure(self) -> bool {
        matches!(
            self,
            Self::Conflict
                | Self::UntrackedOverwrite
                | Self::CheckFailed
                | Self::SkippedUpstreamFailed
        )
    }
}

impl std::fmt::Display for StitchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Merged => "merged",
            Self::AlreadyMerged => "already_merged",
            Self::NoBranch => "no_branch",
            Self::NotCompleted => "not_completed",
            Self::Conflict => "conflict",
            Self::UntrackedOverwrite => "untracked_overwrite",
            Self::CheckFailed => "check_failed",
            Self::SkippedUpstreamFailed => "skipped_upstream_failed",
            Self::PlannedMerge => "planned_merge",
            Self::PlannedSkip => "planned_skip",
        })
    }
}

/// Execute the `stitch` subcommand.
///
/// # Errors
///
/// Surfaces every disruption that should stop the operator's flow:
/// invalid root id, DAG cycle, base-branch mismatch, fleet-lock
/// acquisition failure, or a textual conflict on a merge. Soft
/// failures (push fails, `cargo check` fails) are recorded on the row
/// and continue.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = FileStore::new(&state_dir);

    let root_id = MoleculeId::new(&args.root)?;
    // Compile the DAG closure rooted at `root_id` and walk it
    // leaves → root via Kahn toposort. `compile_plan` returns
    // `(dep, dependent)` edges; toposort hands back the dependency-
    // first ordering — exactly what we want for stitching.
    let (_plan, edges) = compile_plan(&store, std::slice::from_ref(&root_id))?;
    let order: Vec<MoleculeId> = if edges.is_empty() {
        // Single-node DAG (mission with no children, or root has no
        // sibling links yet). The root is still a legitimate merge
        // candidate.
        vec![root_id.clone()]
    } else {
        toposort(&edges).map_err(|e| anyhow::anyhow!("dependency cycle in DAG: {e:?}"))?
    };

    let repo_root = find_repo_root()?;

    if args.dry_run {
        let rows = plan_rows(&store, &repo_root, &order);
        emit_report(ctx, &rows);
        return Ok(());
    }

    // Single-writer-trunk: hold the **trunk lock** for the whole batch so
    // no concurrent `cs done` or sibling `cs stitch` slips a merge commit
    // between two iterations. `cs stitch` takes the trunk lock ALONE (it
    // rewrites no fleet state) — the deadlock-free option-1 fix from the
    // TLA+ finding (`smithy/docs/formal/MCStitch.tla`). Because both
    // `cs done` and `cs stitch` now serialise on the SAME trunk lock, the
    // batch is genuinely exclusive against concurrent single-molecule
    // landings — which the old `with_fleet_lock` did NOT guarantee, since
    // `cs done`'s merge never ran under the fleet lock.
    // ADR-131 Decision 2: RAII trunk guard replaces the lock-bounding closure.
    // `_g` holds the trunk lock for the whole batch and releases at end of
    // block (before `emit_report`); `s` keeps the port-only `stitch_one` call
    // byte-identical. An early `return Err` propagates from `run` exactly as
    // the old `result?` did.
    let rows = {
        let _g = store.lock_trunk(&format!("cs stitch {root_id}"))?;
        let s = &store;
        // HEAD must be on the configured base branch — git merge
        // advances the *current* branch, never one by name. Refuse
        // before we touch anything; same discipline as `cs done`.
        let base = resolve_base_branch(&repo_root);
        let current =
            current_branch_name(&repo_root).unwrap_or_else(|| "(detached HEAD)".to_owned());
        if current != base {
            return Err(anyhow::anyhow!(
                "cs stitch refuses: HEAD is on `{current}` but the configured base branch is `{base}`.\n\
                 Run `cs stitch` from the base checkout (e.g. `git switch {base}` first)."
            ));
        }

        // Predecessor map: dependent → its direct dependencies. Lets us
        // skip only the lineage downstream of a failure while letting
        // independent DAG branches keep stitching (delib post-mortem
        // 2026-06-11: bc13 was independent of e5f6 and could have merged,
        // but the old `break` halted the whole batch on the first
        // conflict). Toposort order guarantees every predecessor is
        // processed before its dependents, so a single forward pass with
        // a `poisoned` set is sufficient.
        let preds = predecessor_map(&edges);
        let mut poisoned: HashSet<MoleculeId> = HashSet::new();

        let mut rows: Vec<StitchRow> = Vec::with_capacity(order.len());
        for mol_id in &order {
            // If any upstream dependency failed, skip without touching git.
            let upstream_failed = upstream_poisoned(mol_id, &preds, &poisoned);
            let row = if upstream_failed {
                StitchRow {
                    molecule: mol_id.as_str().to_owned(),
                    status_merge: StitchStatus::SkippedUpstreamFailed,
                    conflict_files: Vec::new(),
                    note: "upstream dependency did not merge".to_owned(),
                }
            } else {
                stitch_one(s, &repo_root, mol_id, args, &base)
            };
            if row.status_merge.is_failure() {
                poisoned.insert(mol_id.clone());
            }
            rows.push(row);
        }
        rows
    };

    emit_report(ctx, &rows);

    // Surface a non-zero exit when any row failed (conflict, untracked
    // debris, or a red compile gate) so wrapping scripts (justfile, cron)
    // can branch on the failure. A failed gate that exited zero is what
    // let the broken-trunk push slip through on 2026-06-11.
    if rows.iter().any(|r| r.status_merge.is_failure()) {
        return Err(anyhow::anyhow!(
            "stitch incomplete — one or more molecules failed (conflict / untracked debris / cargo check); \
             green merges were kept, failed lineages were rolled back. Resolve and rerun."
        ));
    }
    Ok(())
}

/// Build a dependent → direct-dependencies map from `(dep, dependent)`
/// edges. Used by [`run`] to propagate a failure to its downstream
/// lineage while sparing independent branches.
fn predecessor_map(edges: &[(MoleculeId, MoleculeId)]) -> HashMap<MoleculeId, Vec<MoleculeId>> {
    let mut map: HashMap<MoleculeId, Vec<MoleculeId>> = HashMap::new();
    for (dep, dependent) in edges {
        map.entry(dependent.clone()).or_default().push(dep.clone());
    }
    map
}

/// Whether `mol` has any direct predecessor in the `poisoned` set.
///
/// Because the batch walks the DAG in toposort order, every predecessor
/// is processed before its dependents, and a failed molecule poisons
/// itself — so a single forward pass with this check propagates a
/// failure down the whole lineage while leaving independent branches
/// untouched.
fn upstream_poisoned(
    mol: &MoleculeId,
    preds: &HashMap<MoleculeId, Vec<MoleculeId>>,
    poisoned: &HashSet<MoleculeId>,
) -> bool {
    preds
        .get(mol)
        .is_some_and(|deps| deps.iter().any(|d| poisoned.contains(d)))
}

// ---------------------------------------------------------------------------
// Per-molecule stitch
// ---------------------------------------------------------------------------

/// Merge one molecule's branch into base, recording the outcome.
///
/// Side effects are git-only: no fleet/molecule state is rewritten
/// here (that is `cs done`'s job — `cs stitch` is the merge half of
/// the symmetry, intentionally narrow).
fn stitch_one(
    store: &FileStore,
    repo_root: &Path,
    mol_id: &MoleculeId,
    args: &Args,
    base: &str,
) -> StitchRow {
    // 1. Status gate — we only merge work that completed cleanly.
    let mol = match store.load_molecule(mol_id) {
        Ok(m) => m,
        Err(e) => {
            return StitchRow {
                molecule: mol_id.as_str().to_owned(),
                status_merge: StitchStatus::NotCompleted,
                conflict_files: Vec::new(),
                note: format!("load failed: {e}"),
            };
        }
    };
    if mol.status != MoleculeStatus::Completed {
        return StitchRow {
            molecule: mol_id.as_str().to_owned(),
            status_merge: StitchStatus::NotCompleted,
            conflict_files: Vec::new(),
            note: format!("status={}", mol.status),
        };
    }

    let branch_name = format!("feat/{mol_id}");

    // 2. Branch existence — silent skip when the molecule produced no
    //    code (deliberation, idea, mission with only meta-output).
    if !branch_exists(repo_root, &branch_name) {
        return StitchRow {
            molecule: mol_id.as_str().to_owned(),
            status_merge: StitchStatus::NoBranch,
            conflict_files: Vec::new(),
            note: String::new(),
        };
    }

    // 3. Already-merged short-circuit — topology check against base.
    if is_branch_ancestor_of(repo_root, &branch_name, base) {
        return StitchRow {
            molecule: mol_id.as_str().to_owned(),
            status_merge: StitchStatus::AlreadyMerged,
            conflict_files: Vec::new(),
            note: String::new(),
        };
    }

    // 4. Capture the pre-merge SHA so a failed gate can roll the merge
    //    fully back. The trunk lock guarantees nobody else advances HEAD
    //    between here and the (possible) reset.
    let Some(pre_merge_sha) = capture_head(repo_root) else {
        return fail_row(
            mol_id,
            StitchStatus::Conflict,
            "could not resolve HEAD before merge",
        );
    };

    // 5. Merge — with untracked-debris recovery. `attempt_merge` returns
    //    the outcome of `git merge`, distinguishing a real content
    //    conflict from a merge that git refused because untracked
    //    working-tree files would be overwritten. `note` carries any
    //    debris-discard breadcrumb forward onto the final merged row.
    let commit_msg = format!("stitch({mol_id}): merge into {base}");
    let note = match classify_merge(repo_root, mol_id, &branch_name, &commit_msg) {
        Ok(note) => note,
        Err(row) => return row,
    };

    // 6-7. Post-merge gates (blocking `--cargo-check`, soft `--push`).
    finalize_merge(repo_root, mol_id, args, base, &pre_merge_sha, note)
}

/// Build a terminal failure [`StitchRow`] with no conflict files.
fn fail_row(mol_id: &MoleculeId, status: StitchStatus, note: &str) -> StitchRow {
    StitchRow {
        molecule: mol_id.as_str().to_owned(),
        status_merge: status,
        conflict_files: Vec::new(),
        note: note.to_owned(),
    }
}

/// Run the merge and translate its [`MergeOutcome`] into either a note
/// breadcrumb to carry forward (`Ok`) or a terminal failure row (`Err`).
fn classify_merge(
    repo_root: &Path,
    mol_id: &MoleculeId,
    branch_name: &str,
    commit_msg: &str,
) -> Result<String, StitchRow> {
    match attempt_merge(repo_root, branch_name, commit_msg) {
        MergeOutcome::Merged => Ok(String::new()),
        MergeOutcome::DiscardedThenMerged(discarded) => Ok(format!(
            "auto-discarded duplicate debris: {}; ",
            discarded.join(", ")
        )),
        MergeOutcome::Conflict(files) => {
            // Roll the merge back so the operator inherits a clean
            // worktree — same hygiene as `cs done`.
            let _ = Command::new("git")
                .args(["-C", &repo_root.to_string_lossy(), "merge", "--abort"])
                .output();
            Err(StitchRow {
                molecule: mol_id.as_str().to_owned(),
                status_merge: StitchStatus::Conflict,
                conflict_files: files,
                note: "merge --abort issued; resolve and rerun".to_owned(),
            })
        }
        MergeOutcome::UntrackedOverwrite {
            blockers,
            discarded,
        } => {
            // Some debris was auto-discarded (byte-identical to the
            // branch) but at least one blocker differs — the operator
            // must decide. No merge is in progress, so no abort needed.
            let prefix = if discarded.is_empty() {
                String::new()
            } else {
                format!("auto-discarded debris: {}; ", discarded.join(", "))
            };
            Err(StitchRow {
                molecule: mol_id.as_str().to_owned(),
                status_merge: StitchStatus::UntrackedOverwrite,
                conflict_files: blockers,
                note: format!(
                    "{prefix}untracked files differ from branch — move or remove and rerun"
                ),
            })
        }
        MergeOutcome::Failed(msg) => Err(fail_row(
            mol_id,
            StitchStatus::Conflict,
            &format!("git merge failed: {msg}"),
        )),
    }
}

/// Apply the post-merge gates to a committed merge.
///
/// `--cargo-check` is a BLOCKING gate: a red check rolls the merge back
/// to `pre_merge_sha` (`git reset --hard`) and withholds the push — a
/// gate that does not block is not a gate (broken-trunk push,
/// 2026-06-11). `--push` is a soft warning: a missing remote or network
/// hiccup should not look like a code failure.
fn finalize_merge(
    repo_root: &Path,
    mol_id: &MoleculeId,
    args: &Args,
    base: &str,
    pre_merge_sha: &str,
    mut note: String,
) -> StitchRow {
    if args.cargo_check {
        let check = Command::new("cargo")
            .arg("check")
            .current_dir(repo_root)
            .output();
        let cargo_ok = matches!(&check, Ok(o) if o.status.success());
        if !cargo_ok {
            // Roll the merge back to pre-merge — trunk stays compile-clean.
            let reset_note = if reset_hard(repo_root, pre_merge_sha) {
                "merge rolled back to pre-merge SHA"
            } else {
                "WARNING: rollback `git reset --hard` failed — inspect trunk manually"
            };
            return fail_row(
                mol_id,
                StitchStatus::CheckFailed,
                &format!("cargo check failed (gate); {reset_note}"),
            );
        }
    }

    if args.push {
        let push = Command::new("git")
            .args(["-C", &repo_root.to_string_lossy(), "push", "origin", base])
            .output();
        let push_ok = matches!(&push, Ok(o) if o.status.success());
        if !push_ok {
            note.push_str("push failed (commit kept locally); ");
        }
    }

    StitchRow {
        molecule: mol_id.as_str().to_owned(),
        status_merge: StitchStatus::Merged,
        conflict_files: Vec::new(),
        note: note.trim_end_matches("; ").to_owned(),
    }
}

/// Outcome of a single `git merge --no-ff` attempt, including the
/// untracked-debris recovery path.
enum MergeOutcome {
    /// Merge committed cleanly on the first try.
    Merged,
    /// Untracked debris was discarded (byte-identical to the branch's
    /// version) and the retried merge then committed cleanly.
    DiscardedThenMerged(Vec<String>),
    /// Real textual conflict; `conflict_files` are the unmerged paths.
    Conflict(Vec<String>),
    /// Merge refused because untracked files would be overwritten and at
    /// least one differs from the branch version (cannot auto-discard).
    UntrackedOverwrite {
        blockers: Vec<String>,
        discarded: Vec<String>,
    },
    /// Git could not be invoked / unexpected failure.
    Failed(String),
}

/// Run `git merge --no-ff <branch>`, recovering from untracked-debris
/// refusals when the debris is a byte-identical duplicate of the
/// branch's tracked version.
fn attempt_merge(repo_root: &Path, branch_name: &str, commit_msg: &str) -> MergeOutcome {
    match run_merge(repo_root, branch_name, commit_msg) {
        Ok(o) if o.status.success() => MergeOutcome::Merged,
        Ok(o) => {
            // A real content conflict leaves unmerged entries.
            let unmerged = list_unmerged_files(repo_root);
            if !unmerged.is_empty() {
                return MergeOutcome::Conflict(unmerged);
            }
            // No merge markers — is this the untracked-overwrite refusal?
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            let untracked = parse_untracked_overwrite(&combined);
            if untracked.is_empty() {
                // Unknown failure — surface the stderr for the operator.
                let msg = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                return MergeOutcome::Failed(if msg.is_empty() {
                    "merge failed with no diagnostic".to_owned()
                } else {
                    msg
                });
            }
            // Partition debris into auto-discardable duplicates vs blockers.
            let mut discarded = Vec::new();
            let mut blockers = Vec::new();
            for f in untracked {
                if working_tree_matches_branch(repo_root, branch_name, &f) {
                    if remove_untracked(repo_root, &f) {
                        discarded.push(f);
                    } else {
                        blockers.push(f);
                    }
                } else {
                    blockers.push(f);
                }
            }
            if !blockers.is_empty() {
                return MergeOutcome::UntrackedOverwrite {
                    blockers,
                    discarded,
                };
            }
            // All debris discarded — retry the merge once.
            match run_merge(repo_root, branch_name, commit_msg) {
                Ok(o2) if o2.status.success() => MergeOutcome::DiscardedThenMerged(discarded),
                Ok(_) => {
                    let unmerged = list_unmerged_files(repo_root);
                    let _ = Command::new("git")
                        .args(["-C", &repo_root.to_string_lossy(), "merge", "--abort"])
                        .output();
                    MergeOutcome::Conflict(unmerged)
                }
                Err(e) => MergeOutcome::Failed(format!("retry after debris discard: {e}")),
            }
        }
        Err(e) => MergeOutcome::Failed(e.to_string()),
    }
}

/// The raw `git merge --no-ff --no-edit -m <msg> <branch>` invocation.
fn run_merge(
    repo_root: &Path,
    branch_name: &str,
    commit_msg: &str,
) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .env("LC_ALL", "C")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            commit_msg,
            branch_name,
        ])
        .output()
}

/// Dry-run companion: compute what each molecule's row *would* be
/// without touching git mutating state. We still probe topology
/// (branch presence, ancestry) since those reads are side-effect-free.
fn plan_rows(store: &FileStore, repo_root: &Path, order: &[MoleculeId]) -> Vec<StitchRow> {
    let base = resolve_base_branch(repo_root);
    order
        .iter()
        .map(|mol_id| {
            let Ok(mol) = store.load_molecule(mol_id) else {
                return StitchRow {
                    molecule: mol_id.as_str().to_owned(),
                    status_merge: StitchStatus::PlannedSkip,
                    conflict_files: Vec::new(),
                    note: "load failed".to_owned(),
                };
            };
            if mol.status != MoleculeStatus::Completed {
                return StitchRow {
                    molecule: mol_id.as_str().to_owned(),
                    status_merge: StitchStatus::PlannedSkip,
                    conflict_files: Vec::new(),
                    note: format!("status={}", mol.status),
                };
            }
            let branch = format!("feat/{mol_id}");
            if !branch_exists(repo_root, &branch) {
                return StitchRow {
                    molecule: mol_id.as_str().to_owned(),
                    status_merge: StitchStatus::NoBranch,
                    conflict_files: Vec::new(),
                    note: String::new(),
                };
            }
            if is_branch_ancestor_of(repo_root, &branch, &base) {
                return StitchRow {
                    molecule: mol_id.as_str().to_owned(),
                    status_merge: StitchStatus::AlreadyMerged,
                    conflict_files: Vec::new(),
                    note: String::new(),
                };
            }
            StitchRow {
                molecule: mol_id.as_str().to_owned(),
                status_merge: StitchStatus::PlannedMerge,
                conflict_files: Vec::new(),
                note: String::new(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn emit_report(ctx: &Context, rows: &[StitchRow]) {
    if ctx.json {
        let payload = serde_json::json!({
            "command": "stitch",
            "rows": rows,
        });
        println!("{}", serde_json::to_string(&payload).unwrap_or_default());
        return;
    }
    if rows.is_empty() {
        println!("cs stitch: nothing to do (empty DAG closure)");
        return;
    }
    // Width-based table — keeps the binary dependency-free; the
    // shape is grep-friendly and stable across rows.
    let mol_w = rows
        .iter()
        .map(|r| r.molecule.len())
        .max()
        .unwrap_or(0)
        .max("molecule".len());
    let status_w = rows
        .iter()
        .map(|r| r.status_merge.to_string().len())
        .max()
        .unwrap_or(0)
        .max("status".len());
    println!(
        "{:<mol_w$}  {:<status_w$}  details",
        "molecule",
        "status",
        mol_w = mol_w,
        status_w = status_w
    );
    println!("{}  {}  -------", "-".repeat(mol_w), "-".repeat(status_w),);
    for row in rows {
        let detail = if row.conflict_files.is_empty() {
            row.note.clone()
        } else {
            format!("conflict: {}", row.conflict_files.join(", "))
        };
        println!(
            "{:<mol_w$}  {:<status_w$}  {}",
            row.molecule,
            row.status_merge.to_string(),
            detail,
            mol_w = mol_w,
            status_w = status_w
        );
    }
}

// ---------------------------------------------------------------------------
// Git helpers — narrow re-impl on purpose (no cross-module coupling)
// ---------------------------------------------------------------------------

fn find_repo_root() -> anyhow::Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !out.status.success() {
        return Err(anyhow::anyhow!("not in a git repository"));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

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
        None
    } else {
        Some(s)
    }
}

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

fn is_branch_ancestor_of(repo_root: &Path, branch: &str, target: &str) -> bool {
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

/// Resolve `HEAD` to a full SHA so a failed gate can roll back precisely.
fn capture_head(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `git reset --hard <sha>` — used to unwind a merge whose compile gate
/// went red. Returns whether the reset succeeded.
fn reset_hard(repo_root: &Path, sha: &str) -> bool {
    let out = Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "reset", "--hard", sha])
        .output();
    matches!(out, Ok(o) if o.status.success())
}

/// Parse git's *"untracked working tree files would be overwritten"*
/// refusal, returning the listed file paths. Git emits a header line,
/// then one tab-indented path per file, then a `Please move or remove`
/// trailer. We collect the indented paths between the two markers.
fn parse_untracked_overwrite(output: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut in_block = false;
    for line in output.lines() {
        if line.contains("untracked working tree files would be overwritten") {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        // The block ends at the "Please move or remove" / "Aborting" trailer.
        let trimmed_start = line.trim_start();
        if line.starts_with("Please ") || trimmed_start == "Aborting" || trimmed_start.is_empty() {
            break;
        }
        // File lines are indented (tab or spaces); anything else ends it.
        if line.starts_with('\t') || line.starts_with(' ') {
            files.push(trimmed_start.to_owned());
        } else {
            break;
        }
    }
    files
}

/// Whether the untracked working-tree file `rel` is byte-identical to
/// the branch's version (`git show <branch>:<rel>`). Only such exact
/// duplicates are safe to auto-discard — the operator's debris is then
/// provably the same content the merge would have written anyway.
fn working_tree_matches_branch(repo_root: &Path, branch: &str, rel: &str) -> bool {
    let Ok(on_disk) = std::fs::read(repo_root.join(rel)) else {
        return false;
    };
    let show = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "show",
            &format!("{branch}:{rel}"),
        ])
        .output();
    match show {
        Ok(o) if o.status.success() => o.stdout == on_disk,
        _ => false,
    }
}

/// Remove an untracked debris file from the worktree. Returns whether
/// the file is gone afterwards (already-absent counts as success).
fn remove_untracked(repo_root: &Path, rel: &str) -> bool {
    let path = repo_root.join(rel);
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_core::interaction::MoleculeLink;
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    // -----------------------------------------------------------------
    // Pure shape tests
    // -----------------------------------------------------------------

    #[test]
    fn stitch_status_renders_snake_case_in_json() {
        let row = StitchRow {
            molecule: "task-x".into(),
            status_merge: StitchStatus::AlreadyMerged,
            conflict_files: Vec::new(),
            note: String::new(),
        };
        let s = serde_json::to_string(&row).unwrap();
        assert!(s.contains("\"already_merged\""), "got: {s}");
    }

    #[test]
    fn stitch_status_display_matches_serde() {
        assert_eq!(StitchStatus::Merged.to_string(), "merged");
        assert_eq!(StitchStatus::Conflict.to_string(), "conflict");
        assert_eq!(StitchStatus::NoBranch.to_string(), "no_branch");
        assert_eq!(StitchStatus::PlannedMerge.to_string(), "planned_merge");
    }

    // -----------------------------------------------------------------
    // DAG ordering — the load-bearing invariant for `cs stitch`.
    //
    // Built directly on `compile_plan + toposort` because that is the
    // exact pair `run()` uses; any drift between this test and the
    // production path would mean the test is lying.
    // -----------------------------------------------------------------

    /// Build a cosmon state with three chained molecules under the
    /// given `state_dir`. The state directory MUST be kept distinct
    /// from the git worktree — `git switch` deletes untracked files
    /// on branch transitions where they collide with the target
    /// branch's tracked tree, and cosmon state files are untracked by
    /// design, so colocating them under the git root silently wipes
    /// them on the first `make_feat_branch` call.
    fn make_chain_store(state_dir: &Path) -> (FileStore, MoleculeId, MoleculeId, MoleculeId) {
        std::fs::create_dir_all(state_dir).unwrap();
        let store = FileStore::new(state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

        // Chain: a blocks b blocks c. `c` is the mission root; `a` is
        // the deepest leaf. `compile_plan(&store, &[c])` should walk
        // back through `BlockedBy` to pick up b and a, and `toposort`
        // should hand them back in `[a, b, c]` order.
        let a = MoleculeId::new("task-20260523-aaa1").unwrap();
        let b = MoleculeId::new("task-20260523-bbb2").unwrap();
        let c = MoleculeId::new("task-20260523-ccc3").unwrap();

        let mut mol_a = sample_mol(&a, MoleculeStatus::Completed);
        mol_a.typed_links = vec![MoleculeLink::Blocks { target: b.clone() }];
        store.save_molecule(&a, &mol_a).unwrap();

        let mut mol_b = sample_mol(&b, MoleculeStatus::Completed);
        mol_b.typed_links = vec![
            MoleculeLink::BlockedBy { source: a.clone() },
            MoleculeLink::Blocks { target: c.clone() },
        ];
        store.save_molecule(&b, &mol_b).unwrap();

        let mut mol_c = sample_mol(&c, MoleculeStatus::Completed);
        mol_c.typed_links = vec![MoleculeLink::BlockedBy { source: b.clone() }];
        store.save_molecule(&c, &mol_c).unwrap();

        (store, a, b, c)
    }

    #[test]
    fn topo_order_is_leaves_then_root_for_chain() {
        let tmp = TempDir::new().unwrap();
        let (store, a, b, c) = make_chain_store(&tmp.path().join("state"));

        let (_plan, edges) = compile_plan(&store, std::slice::from_ref(&c)).unwrap();
        let order = toposort(&edges).unwrap();

        assert_eq!(order, vec![a, b, c], "leaves must come before root");
    }

    // -----------------------------------------------------------------
    // Real-git tests for `stitch_one` and the locked batch.
    //
    // Direct exercise of the merge half so the test catches any
    // regression in the per-branch git contract without needing a full
    // `Context` + CWD juggling.
    // -----------------------------------------------------------------

    fn git(repo: &Path, args: &[&str]) -> std::process::Output {
        let mut full: Vec<&str> = vec!["-C"];
        let repo_str = repo.to_str().unwrap();
        full.push(repo_str);
        full.extend_from_slice(args);
        Command::new("git")
            .args(&full)
            .output()
            .expect("git command failed to spawn")
    }

    fn init_repo(repo: &Path) {
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

    fn make_feat_branch(repo: &Path, mol: &MoleculeId, file: &str, contents: &str) {
        let branch = format!("feat/{mol}");
        assert!(git(repo, &["switch", "-q", "-c", &branch, "main"])
            .status
            .success());
        std::fs::write(repo.join(file), contents).unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(
            git(repo, &["commit", "-q", "-m", &format!("work on {mol}")])
                .status
                .success()
        );
        assert!(git(repo, &["switch", "-q", "main"]).status.success());
    }

    fn sample_mol(id: &MoleculeId, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: id.clone(),
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

    fn default_args(_root: &str) -> Args {
        Args {
            root: "ignored".into(),
            cargo_check: false,
            push: false,
            dry_run: false,
        }
    }

    #[test]
    fn stitch_one_chain_merges_in_topological_order() {
        // The full Phase 1 / Commit 2 contract: 3 chained molecules,
        // stitch in toposort order, observe that main collects the
        // three branches in the expected sequence (a, then b, then c).
        // Worktree shared between cosmon-state and the git repo so
        // we exercise the same `repo_root` shape the operator hits.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        let (store, a, b, c) = make_chain_store(&tmp.path().join("state"));

        // One feat branch per molecule, each touching a disjoint file
        // so the merges cannot textually conflict — that is a separate
        // test below.
        make_feat_branch(repo, &a, "a.txt", "from a\n");
        make_feat_branch(repo, &b, "b.txt", "from b\n");
        make_feat_branch(repo, &c, "c.txt", "from c\n");

        // Compute the order the production path would compute.
        let (_plan, edges) = compile_plan(&store, std::slice::from_ref(&c)).unwrap();
        let order = toposort(&edges).unwrap();
        assert_eq!(order, vec![a.clone(), b.clone(), c.clone()]);

        // Run the per-molecule stitch step, the same loop body
        // `run()` executes inside `with_trunk_lock`.
        let args = default_args(c.as_str());
        let base = "main".to_owned();
        let rows: Vec<StitchRow> = order
            .iter()
            .map(|mol_id| stitch_one(&store, repo, mol_id, &args, &base))
            .collect();

        // All three should report merged.
        for row in &rows {
            assert_eq!(
                row.status_merge,
                StitchStatus::Merged,
                "expected merged for {}, got {:?} (note: {})",
                row.molecule,
                row.status_merge,
                row.note
            );
        }

        // All three files should now live on main.
        assert!(repo.join("a.txt").exists());
        assert!(repo.join("b.txt").exists());
        assert!(repo.join("c.txt").exists());

        // The merge-commit subjects must appear in toposort order in
        // `git log --first-parent main` — proves the lock-window loop
        // did not reorder them.
        let log = git(repo, &["log", "--first-parent", "--format=%s", "main"]);
        let log_out = String::from_utf8_lossy(&log.stdout).into_owned();
        let pos_a = log_out
            .find(&format!("stitch({a}): merge into main"))
            .expect("merge commit for a should be on main");
        let pos_b = log_out
            .find(&format!("stitch({b}): merge into main"))
            .expect("merge commit for b should be on main");
        let pos_c = log_out
            .find(&format!("stitch({c}): merge into main"))
            .expect("merge commit for c should be on main");
        // `git log` is newest-first, so the most recent merge has the
        // smallest byte offset. Topological order [a, b, c] means c
        // landed last, so its subject appears first in the log.
        assert!(
            pos_c < pos_b && pos_b < pos_a,
            "stitch order must be a → b → c (oldest → newest):\n{log_out}"
        );
    }

    #[test]
    fn stitch_one_aborts_on_conflict_and_leaves_clean_tree() {
        // Two branches that both touch the same file with different
        // contents — `git merge --no-ff` cannot resolve textually,
        // `cs stitch` must call `--abort` and surface the conflict.
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();

        let mol_a = MoleculeId::new("task-20260523-cf1a").unwrap();
        let mol_b = MoleculeId::new("task-20260523-cf2b").unwrap();
        store
            .save_molecule(&mol_a, &sample_mol(&mol_a, MoleculeStatus::Completed))
            .unwrap();
        store
            .save_molecule(&mol_b, &sample_mol(&mol_b, MoleculeStatus::Completed))
            .unwrap();

        // Both branches edit `same.txt` with different content.
        make_feat_branch(repo, &mol_a, "same.txt", "branch a\n");
        make_feat_branch(repo, &mol_b, "same.txt", "branch b\n");

        let args = default_args("conflict-a");
        let base = "main".to_owned();

        let row_a = stitch_one(&store, repo, &mol_a, &args, &base);
        assert_eq!(row_a.status_merge, StitchStatus::Merged);

        // Second merge must conflict.
        let row_b = stitch_one(&store, repo, &mol_b, &args, &base);
        assert_eq!(row_b.status_merge, StitchStatus::Conflict);
        assert!(
            row_b.conflict_files.iter().any(|f| f == "same.txt"),
            "expected same.txt in conflict files, got {:?}",
            row_b.conflict_files
        );

        // After abort the worktree must be clean.
        let status = git(repo, &["status", "--porcelain"]);
        assert!(status.status.success());
        assert!(
            status.stdout.is_empty(),
            "expected clean tree after --abort, got: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    #[test]
    fn stitch_one_skips_already_merged_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = MoleculeId::new("task-20260523-a1b2").unwrap();
        store
            .save_molecule(&mol, &sample_mol(&mol, MoleculeStatus::Completed))
            .unwrap();
        make_feat_branch(repo, &mol, "a.txt", "x\n");

        // Pre-merge it directly so `cs stitch` sees an already-merged
        // branch on its way through.
        let branch = format!("feat/{mol}");
        assert!(git(
            repo,
            &["merge", "-q", "--no-ff", "--no-edit", "-m", "pre", &branch]
        )
        .status
        .success());

        let args = default_args(mol.as_str());
        let row = stitch_one(&store, repo, &mol, &args, "main");
        assert_eq!(row.status_merge, StitchStatus::AlreadyMerged);
    }

    #[test]
    fn stitch_one_reports_no_branch_for_pure_decision() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = MoleculeId::new("decision-20260523-d3c4").unwrap();
        store
            .save_molecule(&mol, &sample_mol(&mol, MoleculeStatus::Completed))
            .unwrap();

        let args = default_args(mol.as_str());
        let row = stitch_one(&store, repo, &mol, &args, "main");
        assert_eq!(row.status_merge, StitchStatus::NoBranch);
    }

    // -----------------------------------------------------------------
    // The gate is a gate — `--cargo-check` failure rolls back the merge.
    // -----------------------------------------------------------------

    /// A feat branch that introduces a file which does NOT compile as
    /// rust, merged with `--cargo-check`, must be rolled back to the
    /// pre-merge SHA and reported as `check_failed` — never `merged`.
    #[test]
    fn cargo_check_failure_rolls_back_merge() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        // Make `main` a tiny but valid cargo crate so `cargo check` runs.
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"stitchtest\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"stitchtest\"\npath = \"main.rs\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "crate"]).status.success());
        let pre_sha = String::from_utf8_lossy(&git(repo, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = MoleculeId::new("task-20260611-bad1").unwrap();
        store
            .save_molecule(&mol, &sample_mol(&mol, MoleculeStatus::Completed))
            .unwrap();

        // Branch introduces a file that does not compile.
        make_feat_branch(repo, &mol, "main.rs", "fn main() { this is not rust }\n");

        let args = Args {
            root: "ignored".into(),
            cargo_check: true,
            push: false,
            dry_run: false,
        };
        let row = stitch_one(&store, repo, &mol, &args, "main");

        assert_eq!(
            row.status_merge,
            StitchStatus::CheckFailed,
            "red cargo check must report check_failed, got note: {}",
            row.note
        );
        // HEAD must be back at pre-merge — the gate blocked.
        let now_sha = String::from_utf8_lossy(&git(repo, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();
        assert_eq!(
            now_sha, pre_sha,
            "merge must be rolled back to pre-merge SHA"
        );
        // No stitch merge commit survives on the branch (cargo itself
        // leaves Cargo.lock / target/ untracked — those are build noise,
        // not the merge; what matters is no *tracked* change persists).
        let tracked = git(repo, &["status", "--porcelain", "--untracked-files=no"]);
        assert!(
            tracked.stdout.is_empty(),
            "expected no tracked changes after rollback, got: {}",
            String::from_utf8_lossy(&tracked.stdout)
        );
        let log = git(repo, &["log", "--format=%s", "HEAD"]);
        assert!(
            !String::from_utf8_lossy(&log.stdout).contains("stitch("),
            "no stitch merge commit should survive rollback"
        );
    }

    /// When `--cargo-check` is green the merge is kept and reported merged.
    #[test]
    fn cargo_check_pass_keeps_merge() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"stitchok\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"stitchok\"\npath = \"main.rs\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();
        assert!(git(repo, &["add", "."]).status.success());
        assert!(git(repo, &["commit", "-q", "-m", "crate"]).status.success());

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = MoleculeId::new("task-20260611-ok02").unwrap();
        store
            .save_molecule(&mol, &sample_mol(&mol, MoleculeStatus::Completed))
            .unwrap();
        // Branch adds a separate, valid file.
        make_feat_branch(repo, &mol, "extra.rs", "// valid\n");

        let args = Args {
            root: "ignored".into(),
            cargo_check: true,
            push: false,
            dry_run: false,
        };
        let row = stitch_one(&store, repo, &mol, &args, "main");
        assert_eq!(row.status_merge, StitchStatus::Merged);
        assert!(repo.join("extra.rs").exists());
    }

    // -----------------------------------------------------------------
    // Untracked debris ≠ conflict.
    // -----------------------------------------------------------------

    /// A worker left an untracked file in the base worktree that is a
    /// byte-identical duplicate of the file the branch introduces. Git
    /// refuses the merge; `cs stitch` must discard the duplicate and
    /// merge cleanly — reporting `merged`, not `conflict`.
    #[test]
    fn untracked_duplicate_debris_is_discarded_and_merged() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = MoleculeId::new("task-20260611-dbr1").unwrap();
        store
            .save_molecule(&mol, &sample_mol(&mol, MoleculeStatus::Completed))
            .unwrap();
        make_feat_branch(repo, &mol, "run_bounds.rs", "// the real content\n");

        // Operator debris: same content, untracked, sitting on main.
        std::fs::write(repo.join("run_bounds.rs"), "// the real content\n").unwrap();

        let args = default_args(mol.as_str());
        let row = stitch_one(&store, repo, &mol, &args, "main");

        assert_eq!(
            row.status_merge,
            StitchStatus::Merged,
            "duplicate debris must not look like a conflict; note: {}",
            row.note
        );
        assert!(
            row.note.contains("auto-discarded"),
            "note should record the discard, got: {}",
            row.note
        );
        assert!(repo.join("run_bounds.rs").exists());
    }

    /// Same shape, but the untracked file DIFFERS from the branch
    /// version — it cannot be safely discarded. Report
    /// `untracked_overwrite` (distinct from `conflict`) and leave it
    /// for the operator.
    #[test]
    fn untracked_differing_debris_reports_untracked_overwrite() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = MoleculeId::new("task-20260611-dbr2").unwrap();
        store
            .save_molecule(&mol, &sample_mol(&mol, MoleculeStatus::Completed))
            .unwrap();
        make_feat_branch(repo, &mol, "run_bounds.rs", "// branch content\n");

        // Debris with DIFFERENT content — unsafe to discard.
        std::fs::write(repo.join("run_bounds.rs"), "// operator's own edits\n").unwrap();

        let args = default_args(mol.as_str());
        let row = stitch_one(&store, repo, &mol, &args, "main");

        assert_eq!(row.status_merge, StitchStatus::UntrackedOverwrite);
        assert!(
            row.conflict_files.iter().any(|f| f == "run_bounds.rs"),
            "blocker file should be listed, got {:?}",
            row.conflict_files
        );
        // The operator's file is untouched.
        assert_eq!(
            std::fs::read_to_string(repo.join("run_bounds.rs")).unwrap(),
            "// operator's own edits\n"
        );
    }

    // -----------------------------------------------------------------
    // Independent branches keep stitching past a failure.
    // -----------------------------------------------------------------

    /// `predecessor_map` inverts `(dep, dependent)` edges into a
    /// dependent → deps lookup.
    #[test]
    fn predecessor_map_inverts_edges() {
        let a = MoleculeId::new("task-20260611-aa01").unwrap();
        let b = MoleculeId::new("task-20260611-bb02").unwrap();
        let c = MoleculeId::new("task-20260611-cc03").unwrap();
        // a → b, a → c (a blocks both b and c).
        let edges = vec![(a.clone(), b.clone()), (a.clone(), c.clone())];
        let map = predecessor_map(&edges);
        assert_eq!(map.get(&b).unwrap(), std::slice::from_ref(&a));
        assert_eq!(map.get(&c).unwrap(), std::slice::from_ref(&a));
        assert!(!map.contains_key(&a), "root has no predecessors");
    }

    /// The full poison-propagation contract, mirroring `run()`'s loop:
    /// a failed molecule poisons its dependents but NOT independent
    /// branches. DAG: `a → b` (b depends on a), `c` independent. If `a`
    /// fails, `b` is skipped, `c` still merges.
    #[test]
    fn failure_poisons_lineage_but_spares_independent_branch() {
        let a = MoleculeId::new("task-20260611-poa1").unwrap();
        let b = MoleculeId::new("task-20260611-pob2").unwrap();
        let c = MoleculeId::new("task-20260611-poc3").unwrap();
        // a → b ; c independent.
        let edges = vec![(a.clone(), b.clone())];
        let preds = predecessor_map(&edges);

        // Replicate run()'s forward pass with a injected as a failure.
        let order = [a.clone(), b.clone(), c.clone()];
        let mut poisoned: HashSet<MoleculeId> = HashSet::new();
        let mut skipped: Vec<MoleculeId> = Vec::new();
        let mut merged: Vec<MoleculeId> = Vec::new();
        for (i, mol) in order.iter().enumerate() {
            if upstream_poisoned(mol, &preds, &poisoned) {
                skipped.push(mol.clone());
                poisoned.insert(mol.clone());
                continue;
            }
            // `a` (index 0) is our injected failure; b/c "merge".
            if i == 0 {
                poisoned.insert(mol.clone());
            } else {
                merged.push(mol.clone());
            }
        }

        assert_eq!(skipped, vec![b.clone()], "b is downstream of failed a");
        assert_eq!(merged, vec![c.clone()], "c is independent and must merge");
    }

    #[test]
    fn stitch_one_skips_pending_molecule() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.as_path();
        init_repo(repo);

        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(&state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = MoleculeId::new("task-20260523-p9e5").unwrap();
        // Pending — the gate must refuse without touching git.
        store
            .save_molecule(&mol, &sample_mol(&mol, MoleculeStatus::Pending))
            .unwrap();
        make_feat_branch(repo, &mol, "a.txt", "x\n");

        let args = default_args(mol.as_str());
        let row = stitch_one(&store, repo, &mol, &args, "main");
        assert_eq!(row.status_merge, StitchStatus::NotCompleted);
        // Branch still exists; nothing landed on main.
        assert!(branch_exists(repo, &format!("feat/{mol}")));
        let log = git(repo, &["log", "--format=%s", "main"]);
        let log_out = String::from_utf8_lossy(&log.stdout).into_owned();
        assert!(!log_out.contains("stitch("), "no stitch commit expected");
    }
}
