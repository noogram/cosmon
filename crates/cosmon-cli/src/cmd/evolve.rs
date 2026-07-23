// SPDX-License-Identifier: AGPL-3.0-only

//! `cs evolve` — advance a molecule to its next lifecycle state.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use chrono::Utc;
use cosmon_core::event::{Envelope, Event};
use cosmon_core::event_v2::EventV2;
use cosmon_core::evolve::{self, EvolveRequest, NewState};
use cosmon_core::formula::Formula;
use cosmon_core::id::MoleculeId;
use cosmon_filestore::FileStore;
use cosmon_state::{event_log, BriefingSeal, StateStore};

use super::Context;

/// Outcome of an attempted per-step auto-commit.
///
/// Carnot D1 resolution: re-running a step costs ~30k tokens + 5-15 worker-min;
/// a commit costs sub-50ms and a few KB — an asymmetry of ~1000×. Per-step
/// commits drop the worst-case crash replay from a whole molecule back to the
/// last step boundary.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutoCommitOutcome {
    /// Working tree had changes; a commit was created with this SHA.
    Committed(String),
    /// Working tree was clean; no commit (zero-exergy commits are forbidden).
    SkippedClean,
    /// Directory is not a git worktree; silently skipped.
    NotAGitRepo,
}

/// Run `git add -A && git commit` in `project_root` if the working tree is
/// dirty. The commit message follows the convention
/// `evolve(<mol_id>): step <N>/<M> — <step_name>` so `git log --first-parent`
/// reads as a step-by-step diary of the molecule's evolution.
///
/// `project_root` MUST be the worker's own worktree (typically the cwd of
/// the `cs evolve` invocation), not the main repo root. When `cs evolve`
/// runs inside `.worktrees/<mol>/`, the state store may have been
/// redirected to the main `.cosmon/state/` (worktree state-host pattern,
/// see [`cosmon_filestore::walk_up_find_cosmon_dir_from`]), but git
/// operations must still happen in the worker's own worktree — otherwise
/// `git add -A` cross-contaminates the auto-commit with uncommitted edits
/// from the main worktree (regression: a pilot's hot-patch in main was
/// hijacked into the worker's evolve commit).
/// Use [`worker_worktree_root`] to derive the right path from cwd.
///
/// Returns `Committed(sha)` on success, `SkippedClean` on an empty diff, and
/// `NotAGitRepo` when the directory is not under git (e.g. unit tests that
/// don't bootstrap a repo). Errors from `git` itself bubble up.
pub(crate) fn auto_commit_step(
    project_root: &Path,
    mol_id: &str,
    step_one_based: usize,
    total: usize,
    step_name: &str,
) -> anyhow::Result<AutoCommitOutcome> {
    // Is this a git worktree at all?
    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project_root)
        .output();
    match toplevel {
        Ok(out) if !out.status.success() => return Ok(AutoCommitOutcome::NotAGitRepo),
        Err(_) => return Ok(AutoCommitOutcome::NotAGitRepo),
        _ => {}
    }

    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .output()
        .map_err(|e| anyhow::anyhow!("git status failed: {e}"))?;
    if !status.status.success() {
        return Ok(AutoCommitOutcome::NotAGitRepo);
    }
    if status.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(AutoCommitOutcome::SkippedClean);
    }

    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(project_root)
        .output()
        .map_err(|e| anyhow::anyhow!("git add failed: {e}"))?;
    if !add.status.success() {
        anyhow::bail!(
            "git add -A failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
    }

    let message = format!("evolve({mol_id}): step {step_one_based}/{total} — {step_name}");
    let commit = Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(project_root)
        .output()
        .map_err(|e| anyhow::anyhow!("git commit failed: {e}"))?;
    if !commit.status.success() {
        anyhow::bail!(
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    }

    let sha_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .map_err(|e| anyhow::anyhow!("git rev-parse HEAD failed: {e}"))?;
    let sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_owned();
    Ok(AutoCommitOutcome::Committed(sha))
}

/// Resolve the git worktree root containing the current working directory.
///
/// `cs evolve` is invoked from the worker's worktree (cwd =
/// `.worktrees/<mol>/`). The state store path (`ops_dir`) however may have
/// been redirected by walk-up discovery to the **main** repo's
/// `.cosmon/state/` (worktree state-host pattern,
/// see [`cosmon_filestore::walk_up_find_cosmon_dir_from`]). Deriving the
/// "project root" as `ops_dir.parent().parent()` therefore yields the
/// **main worktree path**, not the worker's worktree — and `git add -A`
/// from there cross-contaminates the auto-commit with main's uncommitted
/// edits (regression: a pilot's hot-patch in main was hijacked into the
/// worker's evolve commit and silently shipped via the merge-on-done).
///
/// This helper queries `git rev-parse --show-toplevel` from the current
/// working directory to recover the actual per-worker worktree, falling
/// back to `fallback` if cwd is not inside a git repo (e.g. tests using
/// a tempdir without `git init`).
fn worker_worktree_root(fallback: &Path) -> PathBuf {
    let Ok(cwd) = std::env::current_dir() else {
        return fallback.to_path_buf();
    };
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if s.is_empty() {
                fallback.to_path_buf()
            } else {
                PathBuf::from(s)
            }
        }
        _ => fallback.to_path_buf(),
    }
}

/// Return `true` when `project_root` is a **shared main working tree** rather
/// than a dedicated linked git worktree.
///
/// `cs tackle` (without `--no-worktree`) isolates each molecule in a *linked*
/// worktree under `.worktrees/<mol>/`; there, git's `--git-dir`
/// (`.git/worktrees/<mol>`) differs from its `--git-common-dir` (`.git`). A
/// `--no-worktree` tackle instead parks the worker on the galaxy's **main
/// checkout**, where the two resolve to the same directory.
///
/// The distinction is load-bearing for the per-step auto-commit. `git add -A`
/// is only safe in a dedicated worktree, whose *entire* dirty tree **is** the
/// molecule's work. In a shared main checkout it sweeps up every unrelated
/// dirty file in the host repo — precisely the `delib-20260704-f676` leak,
/// where a deliberation's evolve auto-commit hijacked 30+ unrelated vault
/// notes (and would have carried fleet state) into the tracked `knowledge`
/// repo, tripping its anti-leak push guard. The `state/` `.gitignore` rule of
/// the host is *not* a sufficient backstop: `git add -A` also stages every
/// non-ignored working file the operator happened to be editing. The only
/// structural fix is to refuse the blanket add when we are not in an isolated
/// worktree. See CHRONICLES 2026-07-05.
///
/// Total: any git failure (not a repo, git absent, malformed output) degrades
/// to `false` — *not* a shared checkout — so legacy/test repos and the
/// dedicated-worktree happy path keep committing. In those degraded cases the
/// complementary [`evolve_worktree_mismatch`] guard still covers the
/// wrong-repo axis. The bias is deliberate: a false `false` at worst permits a
/// commit the mismatch guard already vets; a false `true` would silently drop
/// a legitimate per-step anchor.
fn is_shared_main_checkout(project_root: &Path) -> bool {
    let out = Command::new("git")
        .args(["rev-parse", "--git-dir", "--git-common-dir"])
        .current_dir(project_root)
        .output();
    let Ok(o) = out else {
        return false;
    };
    if !o.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let mut lines = text.lines();
    let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return false;
    };
    // `git` may print either relative (`.git`) or absolute paths depending on
    // the version and cwd; resolve both against `project_root` (an absolute
    // right-hand side wins the join) and canonicalize so `/var` ↔ `/private/var`
    // and symlinked worktrees compare equal.
    canonical_or(&project_root.join(git_dir)) == canonical_or(&project_root.join(common_dir))
}

/// Canonicalize `p`, degrading to a lexical copy when the path does not exist
/// on disk (`std::fs::canonicalize` requires the path to exist).
///
/// The worktree guard must be **total** — it can never error — because the
/// auto-commit it protects is a defensive convenience that must not block the
/// molecule lifecycle. Canonicalizing also resolves symlinks so a worktree
/// reached through a symlinked path still matches its recorded target.
pub(crate) fn canonical_or(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Resolve the worktree `cs tackle` recorded for `mol`, as an absolute path.
///
/// `cs tackle` stamps the worker's worktree into the bound worker's `repo`
/// field (stored relative to the galaxy root for portability — see
/// [`cosmon_filestore::make_relative`]). We read it back from the fleet and
/// resolve it against `galaxy_root`.
///
/// Returns `None` for molecules with no bound worker or no recorded repo —
/// the legacy / test shapes that must keep behaving as before (no
/// worktree-mismatch regression). This is the *recorded-path* source the
/// guard compares against, which is why the guard is Cell-B-safe: it never
/// hard-codes a `~/galaxies` prefix.
pub(crate) fn recorded_worktree_for(
    store: &FileStore,
    mol: &cosmon_state::MoleculeData,
    galaxy_root: &Path,
) -> Option<PathBuf> {
    let wid = mol.assigned_worker.as_ref()?;
    let fleet = store.load_fleet().ok()?;
    let repo = fleet.workers.get(wid)?.repo.as_deref()?;
    Some(cosmon_filestore::resolve_repo_path(repo, galaxy_root))
}

/// Decide whether `cs evolve`'s per-step auto-commit may run in `cwd_toplevel`.
///
/// Returns `Some((recorded, actual))` — both canonicalized — when the git
/// toplevel of the worker's cwd does **not** match the molecule's recorded
/// worktree, so the caller must SKIP the `git add -A` commit and warn naming
/// both paths. Returns `None` when the commit is safe: either the paths match
/// (the normal worker-in-its-worktree case, including `--no-worktree` where
/// both resolve to the galaxy root), or no worktree was recorded (legacy /
/// tests).
///
/// Motivation: a worker in a release clone *outside* the galaxy saw
/// `cs evolve` auto-commit into that unrelated repo. The guard refuses to
/// `git add -A` into a repo the molecule was not tackled in. This is the
/// spatial twin of the worktree-mismatch guard above, which fixed the
/// *which-tree* axis (worker vs main); this fixes the *which-repo*
/// axis (galaxy vs foreign clone).
pub(crate) fn evolve_worktree_mismatch(
    recorded_worktree: Option<&Path>,
    cwd_toplevel: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let recorded = recorded_worktree?;
    let rec = canonical_or(recorded);
    let act = canonical_or(cwd_toplevel);
    (rec != act).then_some((rec, act))
}

/// Decide whether `cs done`'s artifact commit may run in `commit_root`.
///
/// Unlike [`evolve_worktree_mismatch`], `cs done` commits the molecule's
/// durable artifacts from the **galaxy root** (the worktree is torn down
/// first), so the safety question is *containment*, not equality: the
/// recorded worktree must live inside the galaxy we are about to commit into.
/// When it does not, `cs done` is running in a foreign repo (the genericize
/// ghost-commit — a release clone outside the galaxy) and must SKIP + warn.
///
/// Returns `Some((recorded, root))` on mismatch, `None` when safe (contained,
/// or no recorded worktree).
pub(crate) fn done_worktree_mismatch(
    recorded_worktree: Option<&Path>,
    commit_root: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let recorded = recorded_worktree?;
    let rec = canonical_or(recorded);
    let root = canonical_or(commit_root);
    (!rec.starts_with(&root)).then_some((rec, root))
}

/// Guard + run a per-step auto-commit, reporting the outcome.
///
/// Wraps [`auto_commit_step`] with [`evolve_worktree_mismatch`]: when the cwd
/// git toplevel does not match the molecule's recorded worktree, the commit is
/// skipped and a loud warning naming both paths is printed — but the caller
/// still advances the step (the guard never blocks the lifecycle). `label`
/// distinguishes the ordinary step commit (`"step"`) from the gate commit
/// (`"gate"`) in the operator-facing messages.
// One argument per piece of the commit subject plus the guard inputs —
// splitting them into a struct would add boilerplate without clarifying the
// single call shape, so we accept the count here (mirrors `run`'s own allow).
#[allow(clippy::too_many_arguments)]
fn guarded_auto_commit(
    ctx: &Context,
    recorded_worktree: Option<&Path>,
    project_root: &Path,
    mol_id: &str,
    step_one_based: usize,
    total: usize,
    step_name: &str,
    label: &str,
) {
    if let Some((recorded, actual)) = evolve_worktree_mismatch(recorded_worktree, project_root) {
        eprintln!(
            "⚠️  cs evolve: SKIPPING {label} auto-commit — the cwd git toplevel does \
             not match this molecule's recorded worktree.\n     \
             recorded worktree: {}\n     cwd git toplevel:  {}\n     \
             (refusing to `git add -A` into a repo {mol_id} was not tackled in)",
            recorded.display(),
            actual.display(),
        );
        return;
    }
    // Shared-main-checkout guard (delib-20260704-f676). When the molecule is
    // parked on a galaxy's main checkout (a `--no-worktree` tackle) rather than
    // an isolated `.worktrees/<mol>/`, `git add -A` would sweep the host repo's
    // *entire* working tree — unrelated notes, the operator's WIP, and any
    // non-ignored fleet-state file — into this evolve commit. That is the leak
    // that dumped 30+ files into the `knowledge` vault and blocked its push.
    // The molecule's own durable artifacts live under `.cosmon/state/` (which
    // every host galaxy gitignores), so there is nothing here we could
    // legitimately blanket-commit: refuse and let the worker stage deliberate
    // paths itself.
    if is_shared_main_checkout(project_root) {
        eprintln!(
            "⚠️  cs evolve: SKIPPING {label} auto-commit — {mol_id} is running on a \
             shared main checkout, not a dedicated worktree.\n     \
             checkout: {}\n     \
             (refusing to `git add -A` the host repo's whole working tree — a \
             blanket commit here leaks unrelated files and fleet state into a \
             tracked host repo; see delib-20260704-f676). Stage deliberate paths \
             yourself if this step must persist tracked changes.",
            project_root.display(),
        );
        return;
    }
    match auto_commit_step(project_root, mol_id, step_one_based, total, step_name) {
        Ok(AutoCommitOutcome::Committed(sha)) => {
            if !ctx.json {
                eprintln!("  📸 {label} commit {}", &sha[..sha.len().min(12)]);
            }
        }
        Ok(AutoCommitOutcome::SkippedClean | AutoCommitOutcome::NotAGitRepo) => {}
        Err(e) => eprintln!("warning: {label} auto-commit failed: {e}"),
    }
}

/// Advance a molecule to its next lifecycle step.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule ID to evolve.
    molecule: String,

    /// Evidence documenting why the current step is complete.
    #[arg(long)]
    evidence: String,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<PathBuf>,

    /// Path to the formula TOML file.
    #[arg(long)]
    formula: PathBuf,
}

/// Execute the `evolve` command.
#[allow(clippy::unnecessary_wraps, clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let mol_id = MoleculeId::new(&args.molecule)?;
    let ops_dir = cosmon_filestore::resolve_state_dir(args.ops_dir.as_deref());
    let store = FileStore::new(&ops_dir);

    // Worktree-isolated project root for git ops, shell verification, and
    // gate execution. `ops_dir` may have been redirected to the main repo's
    // `.cosmon/state/` (worktree state-host pattern); deriving the project
    // root from it would point at the **main worktree** and let `git add -A`
    // (and `cargo` invocations) leak across worker boundaries. We resolve
    // from cwd via `git rev-parse --show-toplevel` instead. Computed once
    // and reused at each call site below. Falls back to the ops_dir-based
    // path when cwd is not in a git repo (legacy / test environments).
    let main_root_buf = ops_dir
        .parent()
        .and_then(|p| p.parent())
        .map_or_else(|| ops_dir.clone(), Path::to_path_buf);
    let project_root_buf = worker_worktree_root(&main_root_buf);

    // Load formula (read-only, no lock needed).
    let formula_text = fs::read_to_string(&args.formula)
        .map_err(|e| anyhow::anyhow!("failed to read formula {}: {e}", args.formula.display()))?;
    let formula = Formula::parse(&formula_text)?;

    // Canonical molecule directory — resolved once and reused both inside the
    // lock (artifact-presence guard) and after it (log.md / briefing.md /
    // seals). This is the path `cs done` tears down, so it is the only place
    // a step's acceptance artifacts can durably live.
    let mol_dir = store.molecule_dir(&mol_id);

    // Hold the fleet lock for the load → evolve → save cycle (ADR-131
    // Decision 2: RAII guard — `_g` releases the flock at end of block).
    // This prevents concurrent evolves from clobbering each other's state.
    let updated = 'lock: {
        let _g = store.lock_fleet()?;
        // Load molecule state.
        let mol_data = store.load_molecule(&mol_id)?;

        // Energy circuit breaker (THESIS Part XI). When the per-molecule
        // [`StepBudget`] is exhausted, refuse the step and park the
        // molecule in `Frozen` with reason `"energy-exhausted"` so a
        // human can decide whether to bump the cap, collapse, or split
        // the work. Decrementing happens once we know the step will
        // proceed (i.e. after `evolve::evolve` validates state) so a
        // budget slot is never burnt on a no-op call.
        if let Some(budget) = mol_data.energy_budget {
            if budget.is_exhausted() {
                let reason = format!("energy-exhausted (cap={}, remaining=0)", budget.cap);
                let mut stuck_mol = mol_data;
                stuck_mol.status = cosmon_core::molecule::MoleculeStatus::Frozen;
                stuck_mol.updated_at = Utc::now();
                store.save_molecule(&stuck_mol.id.clone(), &stuck_mol)?;

                let events_path = ops_dir.join("events.jsonl");
                let _ = event_log::emit_one(
                    &events_path,
                    EventV2::MoleculeStuck {
                        molecule_id: mol_id.clone(),
                        reason: cosmon_core::event_v2::StuckReason::EnergyExhausted,
                    },
                    None,
                );

                if ctx.json {
                    let json_out = serde_json::json!({
                        "molecule": stuck_mol.id.as_str(),
                        "stuck": true,
                        "reason": reason,
                        "energy_exhausted": true,
                        "energy_cap": stuck_mol
                            .energy_budget
                            .map_or(0, |b| b.cap),
                    });
                    println!("{}", serde_json::to_string(&json_out).unwrap());
                } else {
                    eprintln!("error: {reason}");
                    println!("❄️  {mol_id} stuck: {reason}");
                }
                anyhow::bail!("{reason}");
            }
        }

        // Run the evolve logic.
        let request = EvolveRequest {
            evidence: args.evidence.clone(),
            timestamp: Utc::now(),
        };
        let outcome = evolve::evolve(
            mol_data.status,
            mol_data.current_step,
            &mol_data.completed_steps,
            &formula,
            &request,
        )?;

        // Print warnings.
        for warning in &outcome.warnings {
            eprintln!("warning: {warning}");
        }

        // Run verification gate if the completed step has a VerificationSpec.
        let completed_step_spec = &formula.steps[outcome.completed_step.index];
        if let Some(ref verification) = completed_step_spec.verification {
            if !verification.criteria.is_empty() {
                // Trust gate (B5, RCE-by-clone): `verification.criteria` is a
                // repo-supplied shell string. Refuse to run it unless the
                // operator has vouched for this repository (`cs trust`). This
                // runs BEFORE any state mutation, so a refusal leaves the
                // molecule untouched.
                cosmon_cli::trust::ensure_trusted(&project_root_buf)?;

                // Run in the worker's own worktree (see `worker_worktree_root`),
                // not the main-repo path derived from `ops_dir` — otherwise
                // verification would shell out against the main worktree's
                // tree state instead of the worker's WIP code.
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(&verification.criteria)
                    .current_dir(&project_root_buf)
                    .output()
                    .map_err(|e| anyhow::anyhow!("failed to run verification command: {e}"))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let combined = if stderr.is_empty() {
                        stdout.to_string()
                    } else {
                        format!("{stdout}{stderr}")
                    };
                    let combined = combined.trim().to_owned();

                    if verification.max_retries > 0 {
                        eprintln!(
                            "warning: verification failed for step \"{}\": {}, {} retries remaining",
                            outcome.completed_step.id, combined, verification.max_retries
                        );
                        // Do NOT persist the step advancement — the agent can fix and retry.
                        if ctx.json {
                            let json_out = serde_json::json!({
                                "molecule": mol_data.id.as_str(),
                                "verification_failed": true,
                                "step": outcome.completed_step.id,
                                "output": combined,
                                "retries_remaining": verification.max_retries,
                            });
                            println!("{}", serde_json::to_string(&json_out).unwrap());
                        }
                        break 'lock None;
                    }
                    // Retries exhausted — mark molecule as stuck.
                    let reason = format!(
                        "verification failed after {} retries: {}",
                        verification.max_retries, combined
                    );
                    let mut stuck_mol = mol_data;
                    stuck_mol.status = cosmon_core::molecule::MoleculeStatus::Frozen;
                    stuck_mol.updated_at = Utc::now();
                    store.save_molecule(&stuck_mol.id.clone(), &stuck_mol)?;

                    let events_path = ops_dir.join("events.jsonl");
                    let _ = event_log::emit_one(
                        &events_path,
                        EventV2::MoleculeStuck {
                            molecule_id: mol_id.clone(),
                            reason: cosmon_core::event_v2::StuckReason::from(reason.clone()),
                        },
                        None,
                    );

                    if ctx.json {
                        let json_out = serde_json::json!({
                            "molecule": stuck_mol.id.as_str(),
                            "verification_failed": true,
                            "stuck": true,
                            "reason": reason,
                        });
                        println!("{}", serde_json::to_string(&json_out).unwrap());
                    } else {
                        eprintln!("error: {reason}");
                        println!("❄️ {mol_id} stuck: {reason}");
                    }
                    break 'lock None;
                }
            }
        }

        // ── Artifact-presence guard (HARD gate, Lever B) ───────────────────
        // After the worker claims the step done, but BEFORE any committed
        // state mutation, verify every artifact the step declared via
        // `acceptance_artifacts` actually landed under the canonical
        // `mol_dir`. A missing acceptance artifact is a *contemporaneous,
        // irreversible* data-loss-before-`cs done`-teardown: the worktree is
        // about to be destroyed and any misplaced copy vanishes with it.
        //
        // This is the gate-step family (FAIL on missing — like `Step::command`
        // / `Step::native` failing on non-zero exit), NOT the defensive
        // briefing-seal family. The seal model (CLAUDE.md §8b) deliberately
        // *proposes* verification: a seal failure is logged and swallowed and
        // never blocks the hot path, because seals guard against *retrospective
        // tampering* a later audit can still catch. This guard blocks instead,
        // because blocking the advance *is* the recovery window — the molecule
        // stays on its current step (no state mutation below), so the worker
        // can move the misplaced file and re-run `cs evolve`. A reviewer
        // should read this as a gate step, not a §8b breach.
        // Lever C (task-20260504-33f6): before the gate fails, try to
        // auto-repatriate any declared artifact the worker wrote to the
        // worktree root instead of the canonical `mol_dir`. This is the
        // recurring deep-think failure (worker writes synthesis.md/frame.md/
        // responses/ to $WORKTREE_ROOT). Recovery is silent on the happy
        // path; the read-only gate is then re-evaluated so anything that
        // could NOT be recovered still fails the advance loudly (Lever B).
        let mut missing =
            missing_expected_artifacts(&mol_dir, &completed_step_spec.expected_artifacts);
        if !missing.is_empty() {
            let repatriated = repatriate_misplaced_artifacts(&mol_dir, &project_root_buf, &missing);
            if !repatriated.is_empty() {
                eprintln!(
                    "warning: auto-repatriated misplaced acceptance artifact(s) into the molecule directory: {}\n  \
the worker wrote them to the worktree root ({}) instead of the canonical molecule_dir ({}); \
they would have been destroyed at `cs done` teardown.",
                    repatriated.join(", "),
                    project_root_buf.display(),
                    mol_dir.display(),
                );
                // Re-evaluate the gate now that the files have been moved.
                missing =
                    missing_expected_artifacts(&mol_dir, &completed_step_spec.expected_artifacts);
            }
        }
        if !missing.is_empty() {
            let reason = format!(
                "step \"{}\" declares acceptance_artifacts that are missing from the molecule directory.\n  \
canonical molecule_dir: {}\n  missing: {}\n\n\
Move the misplaced artifact(s) into the canonical directory above, then re-run `cs evolve`. \
The molecule has NOT advanced — its state is unchanged, so this is recoverable until `cs done` tears down the worktree.",
                outcome.completed_step.id,
                mol_dir.display(),
                missing.join(", "),
            );
            if ctx.json {
                let json_out = serde_json::json!({
                    "molecule": mol_id.as_str(),
                    "artifact_guard_failed": true,
                    "advanced": false,
                    "step": outcome.completed_step.id,
                    "molecule_dir": mol_dir.display().to_string(),
                    "missing_artifacts": missing,
                });
                println!("{}", serde_json::to_string(&json_out).unwrap());
            } else {
                eprintln!("error: {reason}");
            }
            anyhow::bail!("{reason}");
        }

        // Update molecule data.
        let step_id = cosmon_core::id::StepId::new(&outcome.completed_step.id)?;
        let mut updated = mol_data;
        updated.completed_steps.push(step_id);
        // Decrement the step-budget circuit breaker (THESIS Part XI). The
        // exhaustion check above guaranteed `remaining > 0`, so `consume`
        // succeeds. Persisted via `save_molecule` below — a crash between
        // here and that write replays through the same `is_exhausted`
        // gate, so a slot is never silently lost.
        if let Some(mut b) = updated.energy_budget {
            b.consume();
            updated.energy_budget = Some(b);
        }
        let now = Utc::now();
        updated.updated_at = now;
        // Step completion is observable forward motion — stamp the
        // inference-stall signal so `cs peek` can derive a `Stalled`
        // health state without introspecting tmux.
        updated.last_progress_at = Some(now);
        // A completed step is a durable work product (artifacts and/or its
        // commit have landed). Heartbeats deliberately never touch this clock.
        updated.last_output_at = Some(now);
        match &outcome.new_state {
            NewState::Active { current_step, .. } => {
                updated.current_step = *current_step;
                // Promote Queued → Running on first evolve.
                updated.status = cosmon_core::molecule::MoleculeStatus::Running;
            }
            NewState::Completed => {
                // Respect freeze_on_last_step: if the molecule was nucleated with
                // this flag (from the formula), freeze instead of completing.
                if updated.freeze_on_last_step {
                    updated.status = cosmon_core::molecule::MoleculeStatus::Frozen;
                } else {
                    updated.status = cosmon_core::molecule::MoleculeStatus::Completed;
                }
                // Set current_step = total_steps so observers report "N/N"
                // instead of "N-1/N". Symmetric with cs complete (4bcbcff).
                updated.current_step = updated.total_steps;
            }
            _ => {}
        }

        // Intent record (godel's intent+receipt pattern, ADR-036): before
        // advancing committed state, stamp a `pending_step` intent. If the
        // process crashes after this save but before the receipt clear
        // below, a future replay can inspect `pending_step` and know that
        // artifact writes may not have landed.
        updated.pending_step = Some(cosmon_state::PendingStep {
            target_step: updated.current_step,
            started_at: Utc::now(),
            commit_sha: None,
        });

        // Save updated molecule (intent + state advance, atomic rename).
        store.save_molecule(&updated.id.clone(), &updated)?;

        Some((updated, outcome))
    };

    // Early return if verification failed or molecule was stuck (no state to advance).
    let Some((mut updated, outcome)) = updated else {
        return Ok(());
    };

    // Worktree guard (idea-20260531-1e1b). The auto-commit below must land in
    // the worktree `cs tackle` assigned to this molecule, never in an
    // unrelated repo the worker happens to be cwd'd in (the genericize
    // ghost-committer). Resolved from the bound worker's recorded repo;
    // `None` for legacy / test molecules, which then behave as today.
    let recorded_worktree = recorded_worktree_for(&store, &updated, &main_root_buf);

    // Per-step auto-commit — give each step its own restore anchor.
    // See `auto_commit_step` for the Carnot rationale.
    // `project_root_buf` is the worker's worktree (resolved at the top of
    // `run` via `worker_worktree_root`), NOT the main repo path — see the
    // helper docs for the cross-contamination regression this prevents.
    {
        let completed_spec = &formula.steps[outcome.completed_step.index];
        let step_name = if completed_spec.title.is_empty() {
            completed_spec.id.clone()
        } else {
            completed_spec.title.clone()
        };
        guarded_auto_commit(
            ctx,
            recorded_worktree.as_deref(),
            &project_root_buf,
            mol_id.as_str(),
            outcome.completed_step.index + 1,
            formula.steps.len(),
            &step_name,
            "step",
        );
    }

    // Auto-execute gate step: if the new current step is a shell gate, run it
    // inline so the worker doesn't have to call `cs tackle` again.  This makes
    // mixed agent→gate sequences seamless (e.g. verify → auto-freeze).
    if let NewState::Active { current_step, .. } = &outcome.new_state {
        if let Some(next_step) = formula.steps.get(*current_step) {
            if next_step.is_gate() {
                let gate_cmd = next_step.command.as_deref().unwrap_or("");
                let timeout_secs = next_step.gate_timeout_secs();

                if !ctx.json {
                    eprintln!("⚙️  Auto-executing gate step \"{}\" …", next_step.id);
                }

                // Trust gate (B5, RCE-by-clone): the gate step's `command` is
                // a repo-supplied shell string. Refuse unless this repository
                // is trusted (`cs trust`).
                cosmon_cli::trust::ensure_trusted(&project_root_buf)?;

                // Run the gate command outside the lock (shell execution),
                // in the worker's own worktree — gates such as `cargo test`
                // must observe the worker's WIP code, not the main repo.
                let gate_result = Command::new("sh")
                    .arg("-c")
                    .arg(gate_cmd)
                    .current_dir(&project_root_buf)
                    .output();

                // Lock only for the state mutation after gate execution.
                match gate_result {
                    Ok(output) if output.status.success() => {
                        // Gate succeeded — advance past it under lock.
                        {
                            let _g = store.lock_fleet()?;
                            let gate_step_id = cosmon_core::id::StepId::new(&next_step.id)?;
                            let gate_request = EvolveRequest {
                                evidence: format!(
                                    "gate step \"{}\" auto-executed (exit 0)",
                                    next_step.id
                                ),
                                timestamp: Utc::now(),
                            };
                            let gate_outcome = evolve::evolve(
                                updated.status,
                                updated.current_step,
                                &updated.completed_steps,
                                &formula,
                                &gate_request,
                            )?;
                            updated.completed_steps.push(gate_step_id);
                            let gate_now = Utc::now();
                            updated.updated_at = gate_now;
                            updated.last_progress_at = Some(gate_now);
                            updated.last_output_at = Some(gate_now);
                            match &gate_outcome.new_state {
                                NewState::Active { current_step, .. } => {
                                    updated.current_step = *current_step;
                                }
                                NewState::Completed => {
                                    if updated.freeze_on_last_step {
                                        updated.status =
                                            cosmon_core::molecule::MoleculeStatus::Frozen;
                                    } else {
                                        updated.status =
                                            cosmon_core::molecule::MoleculeStatus::Completed;
                                    }
                                    updated.current_step = updated.total_steps;
                                }
                                _ => {}
                            }
                            store.save_molecule(&updated.id.clone(), &updated)?;

                            // Emit gate step-completed event.
                            let gate_events_path = ops_dir.join("events.jsonl");
                            let _ = event_log::emit_one(
                                &gate_events_path,
                                EventV2::MoleculeStepCompleted {
                                    molecule_id: mol_id.clone(),
                                    step: gate_outcome.completed_step.index,
                                    total: formula.steps.len(),
                                    duration_ms: None,
                                    step_hash: None,
                                },
                                None,
                            );

                            if matches!(gate_outcome.new_state, NewState::Completed) {
                                let _ = event_log::emit_one(
                                    &gate_events_path,
                                    EventV2::MoleculeCompleted {
                                        molecule_id: mol_id.clone(),
                                        duration_ms: None,
                                        reason: format!(
                                            "gate step \"{}\" auto-executed, all steps done",
                                            next_step.id
                                        ),
                                    },
                                    None,
                                );
                            }
                        }

                        if !ctx.json {
                            eprintln!("  ✅ gate \"{}\" passed", next_step.id);
                        }

                        // Per-step auto-commit for the gate step (gates often
                        // touch files — e.g. fmt fixes — and deserve their own
                        // restore anchor).
                        let gate_name = if next_step.title.is_empty() {
                            next_step.id.clone()
                        } else {
                            next_step.title.clone()
                        };
                        let gate_step_one_based =
                            updated.completed_steps.len().min(formula.steps.len());
                        guarded_auto_commit(
                            ctx,
                            recorded_worktree.as_deref(),
                            &project_root_buf,
                            mol_id.as_str(),
                            gate_step_one_based,
                            formula.steps.len(),
                            &gate_name,
                            "gate",
                        );
                    }
                    Ok(output) => {
                        // Gate failed — collapse the molecule under lock.
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let reason = format!(
                            "gate step \"{}\" failed (exit {}): {}{}",
                            next_step.id,
                            output.status.code().unwrap_or(-1),
                            stdout.trim(),
                            stderr.trim()
                        );
                        {
                            let _g = store.lock_fleet()?;
                            updated.status = cosmon_core::molecule::MoleculeStatus::Collapsed;
                            updated.collapse_reason = Some(reason.clone());
                            updated.updated_at = Utc::now();
                            store.save_molecule(&updated.id.clone(), &updated)?;
                        }
                        if !ctx.json {
                            eprintln!("  💥 gate \"{}\" failed: {reason}", next_step.id);
                        }
                    }
                    Err(e) => {
                        let reason = format!("gate step \"{}\" spawn error: {e}", next_step.id);
                        {
                            let _g = store.lock_fleet()?;
                            updated.status = cosmon_core::molecule::MoleculeStatus::Collapsed;
                            updated.collapse_reason = Some(reason.clone());
                            updated.updated_at = Utc::now();
                            store.save_molecule(&updated.id.clone(), &updated)?;
                        }
                        if !ctx.json {
                            eprintln!("  💥 {reason}");
                        }
                    }
                }

                let _ = timeout_secs; // used by tackle; informational here
            }
        }
    }

    // Emit legacy molecule_evolved event.
    let _ = cosmon_filestore::event::append(
        &ops_dir.join("events.jsonl"),
        &Envelope::now(Event::MoleculeEvolved {
            molecule_id: mol_id.clone(),
            step: outcome.completed_step.index,
            total: updated.total_steps,
        }),
    );

    // Emit EventV2 records.
    let events_path = ops_dir.join("events.jsonl");
    let step_seq = event_log::emit_one(
        &events_path,
        EventV2::MoleculeStepCompleted {
            molecule_id: mol_id.clone(),
            step: outcome.completed_step.index,
            total: updated.total_steps,
            duration_ms: None,
            step_hash: None,
        },
        None,
    )
    .ok();

    if matches!(outcome.new_state, NewState::Completed) {
        let _ = event_log::emit_one(
            &events_path,
            EventV2::MoleculeCompleted {
                molecule_id: mol_id.clone(),
                duration_ms: None,
                reason: "all steps completed".to_owned(),
            },
            step_seq,
        );
    }

    // Append to log.md in the molecule directory. `mol_dir` was resolved
    // before the lock (fleet-scoped path) and reused here.
    let log_path = mol_dir.join("log.md");
    let existing_log = fs::read_to_string(&log_path).unwrap_or_default();
    let new_log = if existing_log.is_empty() {
        format!("# Evolution Log\n\n{}", outcome.log_entry)
    } else {
        format!("{existing_log}{}", outcome.log_entry)
    };
    fs::write(&log_path, new_log).map_err(|e| anyhow::anyhow!("failed to write log.md: {e}"))?;

    // Write briefing.md (or remove if completed).
    let briefing_path = mol_dir.join("briefing.md");
    if let Some(ref briefing) = outcome.briefing {
        fs::write(&briefing_path, briefing)
            .map_err(|e| anyhow::anyhow!("failed to write briefing.md: {e}"))?;
    } else {
        // Molecule completed — write a final briefing.
        fs::write(
            &briefing_path,
            "# Molecule Briefing\n\n**Status:** COMPLETED\n\nAll steps have been completed.\n",
        )
        .map_err(|e| anyhow::anyhow!("failed to write briefing.md: {e}"))?;
    }

    // Committee-posture survival: the briefing we just (re)wrote is a fresh
    // projection of the formula step and carries NO adversarial contract. If
    // this molecule is a committee seat — i.e. the durable, regeneration-stable
    // `committee-posture.md` exists in its directory — re-establish the stable
    // pointer to it so the seat's persona witness stays satisfied across every
    // step advance (committee-20260723-c0a1, witness 2 = `BriefingNotInjected`).
    // Runs before the seal below so the seal covers the delivered pointer.
    reinstate_committee_posture_reference(&mol_dir, &briefing_path)?;

    // Soft-contract seal: hash the briefing we just wrote and append it
    // to `MoleculeData::briefing_seals`. Defensive — any failure is
    // logged and swallowed; seal emission must never block step advance.
    // `step` is the zero-based index of the step the worker is now on
    // (i.e. the step this briefing introduces), which matches the
    // briefing regeneration that just happened.
    let seal_step = u32::try_from(updated.current_step).unwrap_or(u32::MAX);
    if let Some(seal) = try_seal_briefing(&briefing_path, seal_step) {
        // Defensive: seal persistence is best-effort and never blocks step
        // advance, so the guard's lock-acquisition / load / save errors are
        // captured into a local `Result` rather than propagated (ADR-131
        // Decision 2 — lexical guard, explicit error capture).
        let persisted: Result<(), cosmon_core::error::CosmonError> = 'seal: {
            let _g = match store.lock_fleet() {
                Ok(g) => g,
                Err(e) => break 'seal Err(e),
            };
            let mut mol = match store.load_molecule(&mol_id) {
                Ok(m) => m,
                Err(e) => break 'seal Err(e),
            };
            mol.briefing_seals.push(seal.clone());
            mol.updated_at = Utc::now();
            store.save_molecule(&mol_id, &mol)
        };
        if let Err(e) = persisted {
            eprintln!("warning: could not persist briefing seal: {e}");
        } else {
            let _ = event_log::emit_one(
                &events_path,
                EventV2::BriefingSealed {
                    molecule_id: mol_id.clone(),
                    step: seal.step,
                    hash: seal.hash.clone(),
                    sealed_at: seal.sealed_at,
                    bytes: seal.briefing_bytes,
                    canonical_version: seal.canonical_version,
                },
                step_seq,
            );
        }
    }

    // Bootstrap-context seal (W2 of delib-20260519-e6db, adversary
    // F2.1+F2.2). The agent-harness bootstrap walk surfaces every
    // `AGENTS.md` / `CLAUDE.md` from `project_root_buf` up to the
    // enclosing `.git/` as fenced `<bootstrap_context>` blocks. We
    // hash that exact byte stream so a later `cs verify` can re-run
    // the walk and detect cross-worktree poisoning — a peer worker
    // dropping an AGENTS.md between this advance and the audit.
    //
    // Same defensive discipline as briefing_seal — any failure is
    // logged and swallowed, never blocks step advance. The walk
    // itself is refuse-above-`.git/`; outside a git checkout it
    // returns an empty string and we seal an empty buffer (so the
    // verifier observes "no bootstrap content" rather than "no seal
    // recorded").
    let bootstrap_bytes =
        cosmon_agent_harness::bootstrap::collect_bootstrap_context(&project_root_buf);
    // Snapshot the walk output into the molecule. The bootstrap walk
    // covers the operator's ambient `AGENTS.md` / `CLAUDE.md`, which live
    // OUTSIDE the molecule and drift legitimately (the operator edits
    // their own instructions; the worktree is torn down and `cs verify`
    // runs from a different cwd). Re-walking at verify time therefore
    // fires on ambient drift, not tampering. Sealing an immutable
    // snapshot of the walk-as-it-was-at-this-step preserves genuine
    // tamper-evidence (a rewrite of the recorded snapshot is still
    // caught) without alarming on honest ambient evolution.
    let bootstrap_seal =
        BriefingSeal::of_text(seal_step, &bootstrap_bytes).with_snapshot(&bootstrap_bytes);
    // Same defensive capture as the briefing seal above (ADR-131 Decision 2).
    let persisted: Result<(), cosmon_core::error::CosmonError> = 'bootstrap: {
        let _g = match store.lock_fleet() {
            Ok(g) => g,
            Err(e) => break 'bootstrap Err(e),
        };
        let mut mol = match store.load_molecule(&mol_id) {
            Ok(m) => m,
            Err(e) => break 'bootstrap Err(e),
        };
        mol.bootstrap_seals.push(bootstrap_seal.clone());
        mol.updated_at = Utc::now();
        store.save_molecule(&mol_id, &mol)
    };
    if let Err(e) = persisted {
        eprintln!("warning: could not persist bootstrap seal: {e}");
    } else {
        let _ = event_log::emit_one(
            &events_path,
            EventV2::BootstrapSealed {
                molecule_id: mol_id.clone(),
                step: bootstrap_seal.step,
                hash: bootstrap_seal.hash.clone(),
                sealed_at: bootstrap_seal.sealed_at,
                bytes: bootstrap_seal.briefing_bytes,
                canonical_version: bootstrap_seal.canonical_version,
            },
            step_seq,
        );
    }

    // Receipt (godel's intent+receipt pattern, ADR-036): artifacts have
    // landed, so clear the `pending_step` intent atomically. A crash
    // between the main state advance and this clear leaves a stale
    // intent record — idempotent replay of `cs evolve` observes the
    // mismatch (intent.target_step vs committed current_step) and can
    // treat it as a no-op marker to be cleared.
    {
        let _g = store.lock_fleet()?;
        let mut mol = store.load_molecule(&mol_id)?;
        if mol.pending_step.is_some() {
            mol.pending_step = None;
            mol.updated_at = Utc::now();
            store.save_molecule(&mol_id, &mol)?;
        }
    }

    // Output result.
    if ctx.json {
        let json_out = serde_json::json!({
            "molecule": updated.id.as_str(),
            "completed_step": outcome.completed_step.id,
            "new_status": updated.status.to_string(),
            "new_step": match &outcome.new_state {
                NewState::Active { step_id, .. } => Some(step_id.clone()),
                _ => None,
            },
            "warnings": outcome.warnings,
        });
        println!("{}", serde_json::to_string(&json_out).unwrap());
    } else {
        println!(
            "evolved {} past step \"{}\"",
            mol_id, outcome.completed_step.id
        );
        match &outcome.new_state {
            NewState::Active {
                step_id,
                step_title,
                current_step,
            } => {
                println!(
                    "  now on step {} of {}: {} ({})",
                    current_step + 1,
                    formula.steps.len(),
                    step_title,
                    step_id
                );
            }
            NewState::Completed => {
                println!("  molecule COMPLETED — all steps done");
            }
            _ => {}
        }
    }

    Ok(())
}

/// Return the subset of `expected` artifacts that are absent from `mol_dir`.
///
/// Each entry is a path relative to `mol_dir`. A trailing `/` (e.g.
/// `"responses/"`) marks a **directory that must exist and be non-empty** —
/// an empty `responses/` is as much a near-loss as a missing one (no proof
/// of work landed). Every other entry must resolve to an existing path
/// (file or directory).
///
/// This is the read-only, idempotent core of the artifact-presence HARD gate
/// (Lever B). It never mutates state; the caller turns
/// a non-empty result into a failed advance. See
/// [`cosmon_core::formula::Step::expected_artifacts`] for the doctrine
/// (why this blocks where the briefing-seal family swallows).
fn missing_expected_artifacts(mol_dir: &Path, expected: &[String]) -> Vec<String> {
    unsatisfied_expected_artifacts(mol_dir, expected, None)
}

/// The acceptance-artifact presence check, shared by the `cs evolve` gate
/// (per-step, `min_mtime = None`) and the in-process / detached-local finalize
/// guard (whole-formula, `min_mtime = Some(step_start)`).
///
/// An entry is *unsatisfied* — i.e. fails to prove work — when any of the four
/// failure modes the acceptance-artifact contract names holds (Jesse #4):
///
/// * **absent** — the declared path does not exist under `mol_dir`;
/// * **empty** — a declared file exists but is zero bytes, or a declared
///   directory (`"…/"`) exists but holds no entry. A zero-byte file is as much
///   a near-loss as none: a weak model that `touch`es its deliverable without
///   writing it must not pass;
/// * **outside the molecule dir** — the entry is absolute or climbs out with
///   `..`. Such a path can never be satisfied by a real deliverable *inside*
///   the recovery window, so it is treated as unsatisfied rather than resolved
///   against something the `cs done` teardown will not preserve;
/// * **stale** — when `min_mtime` is given, the artifact (or, for a directory,
///   its freshest child) was last modified *before* the step started. A file
///   left over from a previous tackle is not proof that *this* turn did work.
///
/// The check is read-only and idempotent. Legacy steps that declare no
/// `acceptance_artifacts` yield an empty `expected` slice and thus an empty
/// result — backward-compatible.
pub(crate) fn unsatisfied_expected_artifacts(
    mol_dir: &Path,
    expected: &[String],
    min_mtime: Option<SystemTime>,
) -> Vec<String> {
    expected
        .iter()
        .filter(|entry| !artifact_satisfied(mol_dir, entry, min_mtime))
        .cloned()
        .collect()
}

/// Whether a single declared `entry` is satisfied under `mol_dir`. See
/// [`unsatisfied_expected_artifacts`] for the four failure modes this rejects.
fn artifact_satisfied(mol_dir: &Path, entry: &str, min_mtime: Option<SystemTime>) -> bool {
    let rel = entry.trim_end_matches('/');
    // Containment: the declared path must resolve strictly *inside* `mol_dir`.
    // An absolute path or one climbing out with `..` can never be a deliverable
    // inside the recovery window, so it is unsatisfiable by construction.
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return false;
    }
    if Path::new(rel)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let path = mol_dir.join(rel);
    if entry.ends_with('/') {
        // Directory contract: exists, holds at least one entry, and — when a
        // step-start floor is given — at least one entry is fresher than it.
        let Ok(read) = fs::read_dir(&path) else {
            return false;
        };
        let mut saw_child = false;
        for child in read.flatten() {
            saw_child = true;
            if mtime_at_or_after(&child.path(), min_mtime) {
                return true;
            }
        }
        // Non-empty but every child is stale (only reachable with a floor);
        // an empty directory falls through here too.
        saw_child && min_mtime.is_none()
    } else {
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => {
                min_mtime.is_none_or(|floor| meta.modified().is_ok_and(|m| m >= floor))
            }
            _ => false,
        }
    }
}

/// Whether `path`'s mtime is at or after `floor` (always true when no floor).
fn mtime_at_or_after(path: &Path, floor: Option<SystemTime>) -> bool {
    match floor {
        None => true,
        Some(floor) => fs::metadata(path).is_ok_and(|m| m.modified().is_ok_and(|t| t >= floor)),
    }
}

/// Recover acceptance artifacts the worker wrote to the worktree root
/// instead of the canonical molecule directory.
///
/// This is **Lever C** for the recurring deep-think failure where a worker
/// writes `synthesis.md` / `frame.md` / `outcomes.md` / `responses/` to
/// `$WORKTREE_ROOT/` rather than the `molecule_dir` the formula declares —
/// observed repeatedly, each occurrence costing the operator ~5 min of
/// manual `mv` + commit. Lever B turned the silent loss-at-`cs done`-teardown
/// into a *loud* `cs evolve` failure; this lever turns that loud failure
/// into a *silent automatic recovery* — the misplaced file is moved for
/// the worker before the gate is even evaluated.
///
/// For each `entry` still missing from `mol_dir` that exists at
/// `worktree_root`, the file (or the directory's contents) is moved into
/// `mol_dir`. Returns the entries successfully repatriated, in
/// declaration order, so the caller can surface a warning.
///
/// Safety — this only ever *helps*, never weakens [the gate]
/// ([`missing_expected_artifacts`]):
/// - No-op when `worktree_root` canonically equals `mol_dir` (no
///   state-host redirection in play — nothing to move, and a self-move
///   would be a footgun).
/// - Only the step's declared `acceptance_artifacts` are ever touched;
///   arbitrary worktree files are never relocated.
/// - A move failure is swallowed per-entry: the caller re-runs the
///   read-only gate afterwards, so anything that could not be recovered
///   still fails the advance loudly.
fn repatriate_misplaced_artifacts(
    mol_dir: &Path,
    worktree_root: &Path,
    missing: &[String],
) -> Vec<String> {
    if missing.is_empty() {
        return Vec::new();
    }
    // Never move onto ourselves: when the state dir is not redirected to a
    // separate worktree there is nothing misplaced to recover.
    if canonical_or(mol_dir) == canonical_or(worktree_root) {
        return Vec::new();
    }
    let mut repatriated = Vec::new();
    for entry in missing {
        let rel = entry.trim_end_matches('/');
        let src = worktree_root.join(rel);
        let dst = mol_dir.join(rel);
        if entry.ends_with('/') {
            // Directory contract: source must exist and be non-empty to be
            // worth recovering (an empty dir is as much a near-loss as none).
            let src_has_content = fs::read_dir(&src).is_ok_and(|mut d| d.next().is_some());
            if src_has_content && move_dir_contents(&src, &dst).is_ok() {
                repatriated.push(entry.clone());
            }
        } else if src.is_file() {
            if let Some(parent) = dst.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if move_path(&src, &dst).is_ok() {
                repatriated.push(entry.clone());
            }
        }
    }
    repatriated
}

/// Move a single file, falling back to copy+remove when `rename` fails
/// (e.g. across filesystems — a worker's worktree and the state-host main
/// repo may sit on different mounts, where `rename(2)` returns `EXDEV`).
fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    fs::copy(src, dst)?;
    fs::remove_file(src)
}

/// Move every child of `src` into `dst`, merging into an existing `dst`
/// rather than clobbering it (an empty `responses/` may already exist in
/// `mol_dir`), then best-effort remove the drained `src`. Recurses for
/// sub-directories and falls back to copy+remove across filesystems.
fn move_dir_contents(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for child in fs::read_dir(src)? {
        let child = child?;
        let from = child.path();
        let to = dst.join(child.file_name());
        if fs::rename(&from, &to).is_err() {
            if from.is_dir() {
                move_dir_contents(&from, &to)?;
            } else {
                fs::copy(&from, &to)?;
                fs::remove_file(&from)?;
            }
        }
    }
    // Best-effort: leaves `src` in place if a child could not be moved.
    let _ = fs::remove_dir(src);
    Ok(())
}

/// Re-establish the committee-posture pointer in a freshly regenerated
/// `briefing.md`, when — and only when — the molecule is a committee seat.
///
/// A committee seat carries its adversarial contract in the durable,
/// regeneration-stable
/// [`committee-posture.md`](cosmon_core::committee::COMMITTEE_POSTURE_FILE) file
/// that `cs evolve` never rewrites. This function appends the stable
/// [`committee_posture_reference`](cosmon_core::committee::committee_posture_reference)
/// pointer to the just-regenerated `briefing.md` so the seat's persona witness
/// keeps seeing a briefing that *references* its contract — closing the
/// `BriefingNotInjected` hole where wholesale briefing regeneration dropped an
/// inline `## Committee posture` section (committee-20260723-c0a1).
///
/// No-ops for ordinary molecules (no durable file → nothing to point at) and is
/// idempotent (skips when the pointer is already present), so re-running a step
/// never stacks duplicate stanzas.
///
/// # Errors
///
/// Returns an error only if the durable file exists (this *is* a committee
/// seat) but appending the pointer to `briefing.md` fails — a real I/O fault on
/// the seat's contract delivery, which must not pass silently.
fn reinstate_committee_posture_reference(
    mol_dir: &Path,
    briefing_path: &Path,
) -> anyhow::Result<()> {
    let posture_path = mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE);
    if !posture_path.exists() {
        // Not a committee seat — nothing to re-establish.
        return Ok(());
    }
    let reference = cosmon_core::committee::committee_posture_reference();
    let current = fs::read_to_string(briefing_path).unwrap_or_default();
    if current.contains(reference.trim()) {
        // Already delivered this step — stay idempotent.
        return Ok(());
    }
    let mut body = current;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(reference);
    fs::write(briefing_path, body).map_err(|e| {
        anyhow::anyhow!("failed to re-establish committee-posture pointer in briefing.md: {e}")
    })
}

/// Compute a [`BriefingSeal`] over `briefing.md`. Returns `None` if the
/// file cannot be read — the caller treats this as "no seal", never as
/// an error. Seal emission is a probe, not a lock.
///
/// The seal carries an immutable snapshot of the briefing **as it was at
/// this step**. cosmon regenerates `briefing.md` on every advance (and
/// `cs complete` rewrites it once more at the end), so verifying a
/// historical seal against the current file would flag cosmon's own
/// honest per-step rewrite as tampering. Snapshotting the content into
/// the molecule lets `cs verify` check each step's briefing against its
/// own epoch. Binary (non-UTF-8) briefings carry no text snapshot and
/// fall back to the live-file comparison at verify time.
fn try_seal_briefing(briefing_path: &Path, step: u32) -> Option<BriefingSeal> {
    match fs::read(briefing_path) {
        Ok(bytes) => {
            let seal = BriefingSeal::of_text_or_bytes(step, &bytes);
            Some(match std::str::from_utf8(&bytes) {
                Ok(text) => seal.with_snapshot(text),
                Err(_) => seal,
            })
        }
        Err(e) => {
            eprintln!("warning: could not seal briefing.md: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn posture_reference_is_noop_without_the_durable_file() {
        // An ordinary (non-committee) molecule: no `committee-posture.md`, so a
        // regenerated briefing is left exactly as the formula step produced it.
        let tmp = tempfile::tempdir().unwrap();
        let mol_dir = tmp.path();
        let briefing_path = mol_dir.join("briefing.md");
        let original = "# Molecule Briefing\n\n## Current Step 1 of 2\n";
        fs::write(&briefing_path, original).unwrap();

        reinstate_committee_posture_reference(mol_dir, &briefing_path).unwrap();

        assert_eq!(fs::read_to_string(&briefing_path).unwrap(), original);
    }

    #[test]
    fn posture_reference_survives_regeneration_and_is_idempotent() {
        // A committee seat: the durable contract lives in `committee-posture.md`.
        let tmp = tempfile::tempdir().unwrap();
        let mol_dir = tmp.path();
        fs::write(
            mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE),
            cosmon_core::committee::render_committee_posture(
                cosmon_core::committee::ADVERSARIAL_BRIEFING_VERSION,
                "blake3:cafe",
                "Refute the fix.",
            ),
        )
        .unwrap();

        // Simulate `cs evolve` regenerating briefing.md wholesale (no contract).
        let briefing_path = mol_dir.join("briefing.md");
        fs::write(
            &briefing_path,
            "# Molecule Briefing\n\n## Current Step 2 of 3\n",
        )
        .unwrap();

        // First advance: the pointer is (re-)established.
        reinstate_committee_posture_reference(mol_dir, &briefing_path).unwrap();
        let after_first = fs::read_to_string(&briefing_path).unwrap();
        assert!(after_first.contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE));
        assert!(after_first.contains("## Committee posture"));

        // Second advance on the same briefing: idempotent — no duplicate stanza.
        reinstate_committee_posture_reference(mol_dir, &briefing_path).unwrap();
        let after_second = fs::read_to_string(&briefing_path).unwrap();
        assert_eq!(after_first, after_second);
        assert_eq!(after_second.matches("## Committee posture").count(), 1);

        // The durable contract file itself is never touched by this path.
        let posture =
            fs::read_to_string(mol_dir.join(cosmon_core::committee::COMMITTEE_POSTURE_FILE))
                .unwrap();
        assert!(posture.contains("Refute the fix."));
    }

    fn init_repo(dir: &Path) {
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "user.email", "t@t"][..],
            &["config", "user.name", "T"][..],
            &["commit", "--allow-empty", "-m", "root", "--quiet"][..],
        ] {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        }
    }

    #[test]
    fn artifact_guard_no_declarations_is_noop() {
        // A legacy step with no `acceptance_artifacts` never reports anything
        // missing, regardless of directory contents — backward-compatible.
        let td = tempfile::tempdir().unwrap();
        let missing = missing_expected_artifacts(td.path(), &[]);
        assert!(missing.is_empty());
    }

    #[test]
    fn artifact_guard_missing_file_is_reported() {
        // synthesis.md absent from the molecule dir → reported as missing
        // (this is the data-loss class the gate exists to catch).
        let td = tempfile::tempdir().unwrap();
        let missing = missing_expected_artifacts(td.path(), &["synthesis.md".to_owned()]);
        assert_eq!(missing, vec!["synthesis.md".to_owned()]);
    }

    #[test]
    fn artifact_guard_present_file_passes() {
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("synthesis.md"), "done").unwrap();
        let missing = missing_expected_artifacts(td.path(), &["synthesis.md".to_owned()]);
        assert!(missing.is_empty());
    }

    #[test]
    fn artifact_guard_directory_must_be_non_empty() {
        let td = tempfile::tempdir().unwrap();

        // (a) responses/ absent entirely → missing.
        let missing = missing_expected_artifacts(td.path(), &["responses/".to_owned()]);
        assert_eq!(missing, vec!["responses/".to_owned()]);

        // (b) responses/ exists but empty → still missing (no proof of work).
        std::fs::create_dir(td.path().join("responses")).unwrap();
        let missing = missing_expected_artifacts(td.path(), &["responses/".to_owned()]);
        assert_eq!(missing, vec!["responses/".to_owned()]);

        // (c) responses/ exists and contains a file → passes.
        std::fs::write(td.path().join("responses").join("feynman.md"), "x").unwrap();
        let missing = missing_expected_artifacts(td.path(), &["responses/".to_owned()]);
        assert!(missing.is_empty());
    }

    #[test]
    fn repatriate_recovers_misplaced_file() {
        // Worker wrote synthesis.md to the worktree root; mol_dir is empty.
        // Lever C moves it into mol_dir and reports it recovered.
        let mol = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(worktree.path().join("synthesis.md"), "the synthesis").unwrap();

        let missing = vec!["synthesis.md".to_owned()];
        let recovered = repatriate_misplaced_artifacts(mol.path(), worktree.path(), &missing);

        assert_eq!(recovered, vec!["synthesis.md".to_owned()]);
        // File now lives in mol_dir with its content intact...
        assert_eq!(
            std::fs::read_to_string(mol.path().join("synthesis.md")).unwrap(),
            "the synthesis"
        );
        // ...and is gone from the worktree (moved, not copied).
        assert!(!worktree.path().join("synthesis.md").exists());
        // The read-only gate now passes.
        assert!(missing_expected_artifacts(mol.path(), &missing).is_empty());
    }

    #[test]
    fn repatriate_recovers_misplaced_directory_contents() {
        // Worker wrote responses/<persona>.md to the worktree root.
        let mol = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let src_responses = worktree.path().join("responses");
        std::fs::create_dir(&src_responses).unwrap();
        std::fs::write(src_responses.join("feynman.md"), "f").unwrap();
        std::fs::write(src_responses.join("jobs.md"), "j").unwrap();

        let missing = vec!["responses/".to_owned()];
        let recovered = repatriate_misplaced_artifacts(mol.path(), worktree.path(), &missing);

        assert_eq!(recovered, vec!["responses/".to_owned()]);
        assert_eq!(
            std::fs::read_to_string(mol.path().join("responses").join("feynman.md")).unwrap(),
            "f"
        );
        assert!(mol.path().join("responses").join("jobs.md").exists());
        assert!(missing_expected_artifacts(mol.path(), &missing).is_empty());
    }

    #[test]
    fn repatriate_merges_into_existing_empty_directory() {
        // mol_dir already has an empty responses/ (still a near-loss); the
        // worker's populated responses/ must merge into it, not be refused.
        let mol = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::create_dir(mol.path().join("responses")).unwrap();
        let src_responses = worktree.path().join("responses");
        std::fs::create_dir(&src_responses).unwrap();
        std::fs::write(src_responses.join("wheeler.md"), "w").unwrap();

        let missing = vec!["responses/".to_owned()];
        let recovered = repatriate_misplaced_artifacts(mol.path(), worktree.path(), &missing);

        assert_eq!(recovered, vec!["responses/".to_owned()]);
        assert!(mol.path().join("responses").join("wheeler.md").exists());
    }

    #[test]
    fn repatriate_is_noop_when_source_absent() {
        // Nothing at the worktree root → nothing recovered, gate still fails.
        let mol = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let missing = vec!["synthesis.md".to_owned()];
        let recovered = repatriate_misplaced_artifacts(mol.path(), worktree.path(), &missing);
        assert!(recovered.is_empty());
        assert_eq!(missing_expected_artifacts(mol.path(), &missing), missing);
    }

    #[test]
    fn repatriate_is_noop_when_dirs_are_identical() {
        // No state-host redirection: worktree_root == mol_dir. A correctly
        // placed file is not "misplaced" and must never be self-moved.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("synthesis.md"), "x").unwrap();
        // (synthesis.md is present, so it wouldn't be in `missing`; but even
        // if asked, an identical-dir call is a hard no-op.)
        let recovered =
            repatriate_misplaced_artifacts(dir.path(), dir.path(), &["synthesis.md".to_owned()]);
        assert!(recovered.is_empty());
        assert!(dir.path().join("synthesis.md").exists());
    }

    #[test]
    fn repatriate_recovers_only_misplaced_subset() {
        // frame.md already correctly in mol_dir; synthesis.md misplaced at
        // the worktree root. Only the misplaced one is recovered.
        let mol = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(mol.path().join("frame.md"), "framed").unwrap();
        std::fs::write(worktree.path().join("synthesis.md"), "synth").unwrap();

        // `missing` is what the gate reports: only synthesis.md.
        let missing = vec!["synthesis.md".to_owned()];
        let recovered = repatriate_misplaced_artifacts(mol.path(), worktree.path(), &missing);
        assert_eq!(recovered, vec!["synthesis.md".to_owned()]);
        assert!(mol.path().join("synthesis.md").exists());
        assert!(mol.path().join("frame.md").exists());
    }

    #[test]
    fn artifact_guard_empty_file_is_reported() {
        // Jesse #4 "empty" clause: a zero-byte file is a near-loss — a weak
        // model that `touch`es its deliverable without writing it must not pass.
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("result.md"), "").unwrap();
        let missing = missing_expected_artifacts(td.path(), &["result.md".to_owned()]);
        assert_eq!(missing, vec!["result.md".to_owned()]);
    }

    #[test]
    fn artifact_guard_outside_mol_dir_is_reported() {
        // Jesse #4 "outside the molecule dir" clause: an escaping or absolute
        // path can never be satisfied by a deliverable inside the recovery
        // window, so it is always unsatisfied — even if such a file exists.
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("result.md"), "real").unwrap();
        // `..` climbs out of mol_dir.
        assert_eq!(
            unsatisfied_expected_artifacts(td.path(), &["../result.md".to_owned()], None),
            vec!["../result.md".to_owned()]
        );
        // An absolute path is likewise unsatisfiable by contract.
        let abs = td.path().join("result.md").to_string_lossy().into_owned();
        assert_eq!(
            unsatisfied_expected_artifacts(td.path(), std::slice::from_ref(&abs), None),
            vec![abs]
        );
    }

    #[test]
    fn artifact_guard_stale_file_is_reported_under_mtime_floor() {
        // Jesse #4 "older than step start" clause: a file left over from a
        // previous tackle (mtime before the step start) is not proof that THIS
        // turn did work.
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("result.md"), "stale from a prior run").unwrap();
        // Floor = now; the file we just wrote is at-or-after `now` only within
        // clock granularity, so use a floor comfortably in the future to model
        // "written before the step started".
        let future = SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(
            unsatisfied_expected_artifacts(td.path(), &["result.md".to_owned()], Some(future)),
            vec!["result.md".to_owned()]
        );
        // With a floor comfortably in the past, the same file counts as fresh.
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        assert!(
            unsatisfied_expected_artifacts(td.path(), &["result.md".to_owned()], Some(past))
                .is_empty()
        );
    }

    #[test]
    fn artifact_guard_directory_freshness_under_mtime_floor() {
        // A declared directory passes the floor only when at least one child is
        // fresher than the step start.
        let td = tempfile::tempdir().unwrap();
        let responses = td.path().join("responses");
        std::fs::create_dir(&responses).unwrap();
        std::fs::write(responses.join("feynman.md"), "x").unwrap();
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        assert!(
            unsatisfied_expected_artifacts(td.path(), &["responses/".to_owned()], Some(past))
                .is_empty()
        );
        let future = SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(
            unsatisfied_expected_artifacts(td.path(), &["responses/".to_owned()], Some(future)),
            vec!["responses/".to_owned()]
        );
    }

    #[test]
    fn artifact_guard_reports_only_the_absent_subset() {
        // Mixed declaration: one present, one missing → only the missing one
        // is reported, in declaration order.
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join("frame.md"), "framed").unwrap();
        let missing = missing_expected_artifacts(
            td.path(),
            &["frame.md".to_owned(), "synthesis.md".to_owned()],
        );
        assert_eq!(missing, vec!["synthesis.md".to_owned()]);
    }

    #[test]
    fn dirty_tree_produces_commit() {
        let td = tempfile::tempdir().unwrap();
        init_repo(td.path());
        std::fs::write(td.path().join("a.txt"), "hello").unwrap();

        let outcome = auto_commit_step(td.path(), "mol-x", 1, 3, "implement").unwrap();
        assert!(matches!(outcome, AutoCommitOutcome::Committed(_)));

        let log = Command::new("git")
            .args(["log", "-1", "--format=%s"])
            .current_dir(td.path())
            .output()
            .unwrap();
        let subject = String::from_utf8_lossy(&log.stdout);
        assert!(
            subject.starts_with("evolve(mol-x): step 1/3 — implement"),
            "subject was: {subject}"
        );
    }

    #[test]
    fn clean_tree_skips_commit() {
        let td = tempfile::tempdir().unwrap();
        init_repo(td.path());

        let before = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(td.path())
            .output()
            .unwrap();
        let before_sha = String::from_utf8_lossy(&before.stdout).trim().to_owned();

        let outcome = auto_commit_step(td.path(), "mol-x", 2, 3, "verify").unwrap();
        assert_eq!(outcome, AutoCommitOutcome::SkippedClean);

        let after = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(td.path())
            .output()
            .unwrap();
        let after_sha = String::from_utf8_lossy(&after.stdout).trim().to_owned();
        assert_eq!(before_sha, after_sha, "no new commit expected");
    }

    #[test]
    fn two_dirty_evolves_produce_two_commits() {
        let td = tempfile::tempdir().unwrap();
        init_repo(td.path());

        std::fs::write(td.path().join("a.txt"), "one").unwrap();
        let c1 = auto_commit_step(td.path(), "mol-y", 1, 2, "first").unwrap();
        std::fs::write(td.path().join("b.txt"), "two").unwrap();
        let c2 = auto_commit_step(td.path(), "mol-y", 2, 2, "second").unwrap();

        match (c1, c2) {
            (AutoCommitOutcome::Committed(a), AutoCommitOutcome::Committed(b)) => {
                assert_ne!(a, b);
            }
            other => panic!("expected two commits, got {other:?}"),
        }

        let count = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(td.path())
            .output()
            .unwrap();
        let n: usize = String::from_utf8_lossy(&count.stdout)
            .trim()
            .parse()
            .unwrap();
        // root + 2 step commits
        assert_eq!(n, 3);
    }

    #[test]
    fn non_git_directory_is_skipped() {
        let td = tempfile::tempdir().unwrap();
        let outcome = auto_commit_step(td.path(), "mol-z", 1, 1, "solo").unwrap();
        assert_eq!(outcome, AutoCommitOutcome::NotAGitRepo);
    }

    /// Regression for the worktree-mismatch bug: when cwd is inside a git
    /// worktree, `worker_worktree_root` returns that worktree (so git ops
    /// stay scoped) — not a fallback path that may belong to another worktree.
    #[test]
    fn worker_worktree_root_resolves_from_cwd() {
        let td = tempfile::tempdir().unwrap();
        let main_repo = td.path().join("main");
        std::fs::create_dir(&main_repo).unwrap();
        init_repo(&main_repo);

        let worker_tree = td.path().join("worker");
        let out = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "wt-branch",
                worker_tree.to_str().unwrap(),
            ])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        assert!(out.status.success());

        // Per-test cwd guard: chdir into the worker worktree, capture, restore.
        // We do not parallelize against other tests that mutate cwd; the
        // serial guard is the test runner's per-binary lock for cwd-touching
        // tests in this file (only this test mutates cwd).
        let prior = std::env::current_dir().unwrap();
        std::env::set_current_dir(&worker_tree).unwrap();
        let resolved = worker_worktree_root(&main_repo);
        std::env::set_current_dir(&prior).unwrap();

        // Canonicalize both sides — macOS resolves `/var/` ↔ `/private/var/`.
        let resolved_canon = resolved.canonicalize().unwrap_or(resolved);
        let expected_canon = worker_tree.canonicalize().unwrap_or(worker_tree);
        assert_eq!(
            resolved_canon, expected_canon,
            "worker_worktree_root should return the cwd's worktree, not the fallback (main)"
        );
    }

    /// Regression for the worktree-mismatch bug: a worker running auto-commit
    /// in its own worktree must NOT capture uncommitted edits sitting in the
    /// main worktree's working directory. The worktrees share a `.git/`
    /// objects database but each has an isolated working tree; passing the
    /// **worker's** worktree path to `auto_commit_step` (rather than a
    /// path derived from `ops_dir` after worktree-state-host redirect)
    /// is what makes the isolation hold.
    #[test]
    fn auto_commit_from_worker_worktree_does_not_capture_main_edits() {
        let td = tempfile::tempdir().unwrap();
        let main_repo = td.path().join("main-repo");
        std::fs::create_dir(&main_repo).unwrap();
        init_repo(&main_repo);
        std::fs::write(main_repo.join("seed.txt"), "seed").unwrap();
        let out = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = Command::new("git")
            .args(["commit", "--quiet", "-m", "seed"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git commit seed failed: {out:?}");

        let worker_tree = td.path().join("worker-tree");
        let out = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "worker-branch",
                worker_tree.to_str().unwrap(),
            ])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Pilot edits a file in the MAIN worktree (uncommitted).
        std::fs::write(main_repo.join("hot-patch.txt"), "PILOT EDIT").unwrap();
        // Worker edits a file in its OWN worktree.
        std::fs::write(worker_tree.join("worker-output.txt"), "WORKER EDIT").unwrap();

        // Auto-commit from the worker's worktree.
        let outcome = auto_commit_step(&worker_tree, "mol-iso", 1, 1, "implement").unwrap();
        let sha = match outcome {
            AutoCommitOutcome::Committed(sha) => sha,
            other => panic!("expected Committed, got {other:?}"),
        };

        // The commit must list `worker-output.txt` only — never `hot-patch.txt`.
        let show = Command::new("git")
            .args(["show", "--name-only", "--format=", &sha])
            .current_dir(&worker_tree)
            .output()
            .unwrap();
        let files = String::from_utf8_lossy(&show.stdout);
        assert!(
            files.contains("worker-output.txt"),
            "expected worker-output.txt in commit, got: {files}"
        );
        assert!(
            !files.contains("hot-patch.txt"),
            "auto-commit from worker worktree captured a main-worktree file (cross-contamination): {files}"
        );

        // The pilot's hot-patch must remain uncommitted in the main worktree.
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        let porcelain = String::from_utf8_lossy(&status.stdout);
        assert!(
            porcelain.contains("hot-patch.txt"),
            "expected hot-patch.txt to remain uncommitted in main worktree, got: {porcelain}"
        );
    }

    // -----------------------------------------------------------------
    // Worktree guard (idea-20260531-1e1b) — spatial twin of b654.
    // These exercise the pure predicates that decide whether the
    // auto-commit may run, given a recorded worktree and the actual
    // cwd git toplevel.
    // -----------------------------------------------------------------

    /// No recorded worktree (legacy / `--no-worktree` with nothing on the
    /// fleet / tests) → behave as today: commit is allowed.
    #[test]
    fn evolve_guard_allows_when_no_recorded_worktree() {
        assert!(evolve_worktree_mismatch(None, Path::new("/anywhere")).is_none());
    }

    /// cwd git toplevel equals the recorded worktree → allowed (the normal
    /// worker-in-its-own-worktree case).
    #[test]
    fn evolve_guard_allows_on_exact_match() {
        let td = tempfile::tempdir().unwrap();
        let wt = td.path().join("worktree");
        std::fs::create_dir(&wt).unwrap();
        assert!(evolve_worktree_mismatch(Some(&wt), &wt).is_none());
    }

    /// cwd git toplevel is a *different* repo than the recorded worktree
    /// (the genericize ghost-committer: a worker cwd'd in a foreign clone)
    /// → mismatch, commit must be skipped, both paths surfaced.
    #[test]
    fn evolve_guard_skips_on_mismatch() {
        let td = tempfile::tempdir().unwrap();
        let recorded = td.path().join("galaxy-worktree");
        let actual = td.path().join("scratch-clone");
        std::fs::create_dir(&recorded).unwrap();
        std::fs::create_dir(&actual).unwrap();
        let mismatch = evolve_worktree_mismatch(Some(&recorded), &actual);
        assert!(mismatch.is_some(), "distinct repos must be flagged");
        let (rec, act) = mismatch.unwrap();
        assert_ne!(rec, act);
    }

    /// `--no-worktree` records the galaxy root itself (the worker runs on the
    /// main checkout), so cwd toplevel == recorded == galaxy root → allowed.
    #[test]
    fn evolve_guard_no_worktree_matches_galaxy_root() {
        let td = tempfile::tempdir().unwrap();
        let galaxy = td.path().join("galaxy");
        std::fs::create_dir(&galaxy).unwrap();
        assert!(evolve_worktree_mismatch(Some(&galaxy), &galaxy).is_none());
    }

    /// A worktree reached through a symlink must canonicalize to the same
    /// target as the recorded path → allowed (no false-positive skip).
    #[cfg(unix)]
    #[test]
    fn evolve_guard_matches_through_symlink() {
        let td = tempfile::tempdir().unwrap();
        let real = td.path().join("real-worktree");
        std::fs::create_dir(&real).unwrap();
        let link = td.path().join("link-worktree");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(
            evolve_worktree_mismatch(Some(&real), &link).is_none(),
            "symlinked worktree path must canonicalize to the recorded target"
        );
    }

    /// `cs done` commits from the galaxy root; the recorded worktree lives
    /// *inside* it (`.worktrees/<mol>`) → containment holds → allowed.
    #[test]
    fn done_guard_allows_when_worktree_inside_repo() {
        let td = tempfile::tempdir().unwrap();
        let galaxy = td.path().join("galaxy");
        let wt = galaxy.join(".worktrees").join("task-x");
        std::fs::create_dir_all(&wt).unwrap();
        assert!(done_worktree_mismatch(Some(&wt), &galaxy).is_none());
    }

    /// `cs done` run from a foreign release clone: the molecule's recorded
    /// worktree is not inside it → refuse to commit artifacts there.
    #[test]
    fn done_guard_skips_when_worktree_outside_repo() {
        let td = tempfile::tempdir().unwrap();
        let galaxy = td.path().join("galaxy");
        let foreign = td.path().join("scratch-clone");
        let wt = galaxy.join(".worktrees").join("task-x");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        assert!(done_worktree_mismatch(Some(&wt), &foreign).is_some());
    }

    /// No recorded worktree → `cs done` commits as today.
    #[test]
    fn done_guard_allows_when_no_recorded_worktree() {
        assert!(done_worktree_mismatch(None, Path::new("/anywhere")).is_none());
    }

    // -----------------------------------------------------------------
    // Shared-main-checkout guard (delib-20260704-f676) — the auto-commit
    // must refuse `git add -A` on a galaxy's main checkout, where the
    // working tree carries unrelated host content, and only allow it in an
    // isolated linked worktree that `cs tackle` created for the molecule.
    // -----------------------------------------------------------------

    /// A plain `git init` repo is a main working tree: `--git-dir` and
    /// `--git-common-dir` coincide → flagged as a shared checkout so the
    /// blanket auto-commit is refused (the delib-f676 leak site).
    #[test]
    fn shared_checkout_detected_on_main_working_tree() {
        let td = tempfile::tempdir().unwrap();
        init_repo(td.path());
        assert!(
            is_shared_main_checkout(td.path()),
            "a main checkout must be flagged shared so `git add -A` is refused"
        );
    }

    /// A linked worktree created by `git worktree add` (the shape `cs tackle`
    /// produces) has a distinct `--git-dir` → NOT a shared checkout, so the
    /// per-step auto-commit proceeds as intended.
    #[test]
    fn linked_worktree_is_not_shared_checkout() {
        let td = tempfile::tempdir().unwrap();
        let main_repo = td.path().join("main");
        std::fs::create_dir(&main_repo).unwrap();
        init_repo(&main_repo);

        let worker_tree = td.path().join("worker");
        let out = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "wt-branch",
                worker_tree.to_str().unwrap(),
            ])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        assert!(
            !is_shared_main_checkout(&worker_tree),
            "a dedicated linked worktree must not be flagged as a shared checkout"
        );
        // …and the main checkout it was spun from still is.
        assert!(
            is_shared_main_checkout(&main_repo),
            "the main checkout backing the worktree is still shared"
        );
    }

    /// Not a git repo → total predicate degrades to `false` (commit allowed;
    /// the low-level `auto_commit_step` then reports `NotAGitRepo` anyway).
    #[test]
    fn shared_checkout_predicate_is_total_on_non_git_dir() {
        let td = tempfile::tempdir().unwrap();
        assert!(
            !is_shared_main_checkout(td.path()),
            "a non-git directory must not be flagged shared"
        );
    }
}
