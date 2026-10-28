// SPDX-License-Identifier: AGPL-3.0-only

//! `cs evolve` — advance a molecule to its next lifecycle state.

use chrono::Utc;
use cosmon_core::event::Envelope;
use cosmon_core::event_v2::EventV2;
use cosmon_core::evolve::{self, EvolveRequest, NewState};
use cosmon_core::formula::Formula;
use cosmon_core::id::MoleculeId;
use cosmon_filestore::FileStore;
use cosmon_state::ops::evolve::{
    evolve as evolve_op, EvolveEffects, EvolveError, EvolveOpRequest, StepRefusal,
};
use cosmon_state::{event_log, BriefingSeal, StateStore};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// The acceptance-artifact guard lives in the shared operation; `cs tackle`'s
// finalize guard and this module's tests keep their old spelling.
pub(super) use cosmon_state::ops::evolve::deliver_committee_posture_reference;
#[allow(unused_imports)]
pub(crate) use cosmon_state::ops::evolve::{
    missing_expected_artifacts, try_seal_briefing, unsatisfied_expected_artifacts,
};

use super::Context;

// The worktree anchor and the two mismatch guards moved to `cosmon-harvest`
// with the harvest transaction — `cs done`'s containment guard is one of the
// three, and the RPP route performs that harvest without going through this
// crate. Re-exported (not re-implemented) so `super::evolve::canonical_or`
// stays the spelling every call site in this crate already uses.
#[allow(unused_imports)] // `done_worktree_mismatch` is the harvest's half of
// the pair; re-exported here so `super::evolve::…` keeps naming all three.
pub(crate) use cosmon_harvest::worktree::{
    canonical_or, done_worktree_mismatch, recorded_worktree_for,
};

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

/// Maximum number of non-generated paths an automatic snapshot will stage.
///
/// A larger change needs a deliberate, path-scoped commit. This ceiling keeps
/// a missing ignore rule from turning one lifecycle tick into a repository
/// dump while leaving ordinary source changes comfortably below the limit.
const AUTO_COMMIT_MAX_PATHS: usize = 1_000;

/// Maximum aggregate size of non-generated files an automatic snapshot will
/// stage without a deliberate commit by the worker.
const AUTO_COMMIT_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// The evidence text as an event field: `None` when the worker recorded none,
/// so the event omits the key rather than carrying an empty string.
/// Find generated-artifact roots identified by the standard `CACHEDIR.TAG`
/// marker.
///
/// Cargo writes this marker at the root of every target directory, including
/// target directories with project-specific names. Once a marker is found we
/// do not descend further: the whole directory is cache material and must be
/// outside the automatic snapshot.
fn cachedir_roots(project_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fn visit(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        if dir.join("CACHEDIR.TAG").is_file() {
            let relative = dir.strip_prefix(root).map_err(|error| {
                anyhow::anyhow!(
                    "generated-artifact path {} escaped snapshot root {}: {error}",
                    dir.display(),
                    root.display()
                )
            })?;
            if !relative.as_os_str().is_empty() {
                found.push(relative.to_path_buf());
            }
            return Ok(());
        }

        for entry in fs::read_dir(dir)
            .map_err(|error| anyhow::anyhow!("scan {} failed: {error}", dir.display()))?
        {
            let entry = entry.map_err(|error| {
                anyhow::anyhow!("scan an entry under {} failed: {error}", dir.display())
            })?;
            let file_type = entry.file_type().map_err(|error| {
                anyhow::anyhow!("inspect {} failed: {error}", entry.path().display())
            })?;
            if file_type.is_dir() && entry.file_name() != ".git" {
                visit(root, &entry.path(), found)?;
            }
        }
        Ok(())
    }

    let mut found = Vec::new();
    visit(project_root, project_root, &mut found)?;
    found.sort();
    Ok(found)
}

fn path_is_within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Refuse an abnormally broad automatic snapshot.
///
/// Marker-identified cache trees are excluded before applying the limits.
/// The remaining candidate paths must fit both ceilings; otherwise the worker
/// has to inspect the tree and make a deliberate commit, which is the required
/// confirmation rather than an unsafe non-interactive yes/no default.
fn guard_snapshot_scale(project_root: &Path, cachedirs: &[PathBuf]) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "--modified",
            "--deleted",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(project_root)
        .output()
        .map_err(|error| anyhow::anyhow!("git ls-files failed: {error}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut paths = 0_usize;
    let mut bytes = 0_u64;
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let relative = PathBuf::from(std::str::from_utf8(raw).map_err(|_| {
            anyhow::anyhow!("automatic snapshot refused: candidate path is not valid UTF-8")
        })?);
        if path_is_within(&relative, cachedirs) {
            continue;
        }
        paths = paths.saturating_add(1);
        if let Ok(metadata) = fs::metadata(project_root.join(&relative)) {
            bytes = bytes.saturating_add(metadata.len());
        }
    }

    if paths > AUTO_COMMIT_MAX_PATHS || bytes > AUTO_COMMIT_MAX_BYTES {
        anyhow::bail!(
            "automatic snapshot refused: {paths} paths / {bytes} bytes exceeds the safety ceiling \
             ({AUTO_COMMIT_MAX_PATHS} paths / {AUTO_COMMIT_MAX_BYTES} bytes); inspect the tree and \
             commit deliberate paths to confirm this change"
        );
    }
    Ok(())
}

fn cached_staged_paths(project_root: &Path, cachedirs: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["diff", "--cached", "--name-only", "-z"])
        .current_dir(project_root)
        .output()
        .map_err(|error| anyhow::anyhow!("git diff --cached failed: {error}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff --cached failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut staged = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let path = PathBuf::from(std::str::from_utf8(raw).map_err(|_| {
            anyhow::anyhow!("automatic snapshot refused: staged path is not valid UTF-8")
        })?);
        if path_is_within(&path, cachedirs) {
            staged.push(path);
        }
    }
    Ok(staged)
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

    let cachedirs = cachedir_roots(project_root)?;
    let staged_cache_paths = cached_staged_paths(project_root, &cachedirs)?;
    if !staged_cache_paths.is_empty() {
        anyhow::bail!(
            "automatic snapshot refused: generated artifacts are already staged under {}. \
             Unstage them and commit deliberate paths",
            staged_cache_paths[0].display()
        );
    }
    guard_snapshot_scale(project_root, &cachedirs)?;

    let mut add = Command::new("git");
    add.args(["add", "-A", "--", "."]);
    for cachedir in &cachedirs {
        let Some(path) = cachedir.to_str() else {
            anyhow::bail!(
                "automatic snapshot refused: generated-artifact directory {} is not valid UTF-8",
                cachedir.display()
            );
        };
        add.arg(format!(":(top,exclude,literal){path}"));
    }
    let add = add
        .current_dir(project_root)
        .output()
        .map_err(|e| anyhow::anyhow!("git add failed: {e}"))?;
    if !add.status.success() {
        anyhow::bail!(
            "git add -A failed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
    }

    let staged = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(project_root)
        .status()
        .map_err(|e| anyhow::anyhow!("git diff --cached failed: {e}"))?;
    match staged.code() {
        Some(0) => return Ok(AutoCommitOutcome::SkippedClean),
        Some(1) => {}
        _ => anyhow::bail!("git diff --cached failed while checking the automatic snapshot"),
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

/// Decide whether `cs evolve`'s per-step auto-commit may run at all when the
/// molecule has **no recorded worktree**.
///
/// Until 2026-07-25 a `None` recorded worktree was read as "legacy / test
/// shape — behave as before" and the blanket `git add -A` was allowed to run
/// in whatever git worktree the caller happened to be cwd'd in. That default
/// is unsafe, and it fired for real: during a `cargo test --workspace` run,
/// `tests/cli.rs`'s verification-gate fixture nucleated a throwaway molecule,
/// inherited the *calling worker's* worktree as its cwd, and committed that
/// worker's uncommitted WIP under the fixture molecule's name
/// (`evolve(vg-…): step 1/2 — Gated step`). No guard could catch it: the
/// fixture molecule has no bound worker, so there was no recorded path to
/// compare against, and a linked worktree is not a shared main checkout.
///
/// The invariant the two sibling guards encode is *"only commit into the tree
/// this molecule was tackled in"*. With no recorded tree there is no such
/// tree, so the honest answer is to refuse rather than to fall back on the
/// ambient cwd. Refusing is cheap: a molecule with no bound worker was never
/// tackled, so nothing downstream (merge-on-done, restore anchors) depends on
/// the anchor existing, and the operator keeps every alternative — commit by
/// hand, or `cs tackle` the molecule so a worktree *is* recorded.
///
/// Returns `true` when the auto-commit must be skipped.
pub(crate) fn unrecorded_worktree_refusal(recorded_worktree: Option<&Path>) -> bool {
    recorded_worktree.is_none()
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
    // Unrecorded-worktree guard (task-20260725-2c49). No recorded worktree
    // means the molecule was never tackled, so there is no tree we are
    // *entitled* to blanket-commit into — and the ambient cwd is emphatically
    // not that tree. See `unrecorded_worktree_refusal` for the incident.
    if unrecorded_worktree_refusal(recorded_worktree) {
        eprintln!(
            "⚠️  cs evolve: SKIPPING {label} auto-commit — {mol_id} has no recorded \
             worktree (no bound worker), so there is no tree this commit may \
             claim.\n     cwd git toplevel:  {}\n     \
             (refusing to `git add -A` the ambient working tree; `cs tackle` the \
             molecule to get a per-step anchor, or commit deliberate paths yourself)",
            project_root.display(),
        );
        return;
    }
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
///
/// The transition is [`cosmon_state::ops::evolve::evolve`], the same operation
/// `POST /v1/molecules/:id/evolve` calls. This module supplies what only a
/// local worker's machine can do, as [`CliEffects`]: the shell verification
/// a step may declare, artifact repatriation from the worktree, the per-step
/// auto-commit, the inline gate step, and the bootstrap seal.
#[allow(clippy::unnecessary_wraps, clippy::too_many_lines)]
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let mol_id = MoleculeId::new(&args.molecule)?;
    let ops_dir = cosmon_filestore::resolve_state_dir(args.ops_dir.as_deref());
    let store = FileStore::new(&ops_dir);

    // ADR-168 §D6 — authority guard, before any write. On a mission under
    // co-pilotage, advancing it is the pilot's gesture and a co-pilot without
    // the lease is refused by the mechanism, not by its brief (M7 friction F9,
    // `task-20260731-bd92` §8). Silent on a mission with no lease.
    super::guard::refuse_unleased_pilot_gesture(
        &ctx.state_dir(),
        &mol_id,
        "cs evolve",
        &super::guard::process_env,
    )?;

    // Worktree-isolated project root for git ops, shell verification, and
    // gate execution. `ops_dir` may have been redirected to the main repo's
    // `.cosmon/state/` (worktree state-host pattern); deriving the project
    // root from it would point at the **main worktree** and let `git add -A`
    // (and `cargo` invocations) leak across worker boundaries. We resolve
    // from cwd via `git rev-parse --show-toplevel` instead. Falls back to the
    // ops_dir-based path when cwd is not in a git repo (legacy / test
    // environments).
    let main_root_buf = ops_dir
        .parent()
        .and_then(|p| p.parent())
        .map_or_else(|| ops_dir.clone(), Path::to_path_buf);
    let project_root_buf = worker_worktree_root(&main_root_buf);

    // Load formula (read-only, no lock needed).
    let formula_text = fs::read_to_string(&args.formula)
        .map_err(|e| anyhow::anyhow!("failed to read formula {}: {e}", args.formula.display()))?;
    let formula = Formula::parse(&formula_text)?;

    let effects = CliEffects {
        ctx,
        store: &store,
        ops_dir: &ops_dir,
        mol_id: &mol_id,
        formula: &formula,
        main_root: &main_root_buf,
        project_root: &project_root_buf,
    };
    let view = match evolve_op(
        &store,
        &ops_dir,
        &mol_id,
        &EvolveOpRequest {
            evidence: &args.evidence,
            formula: &formula,
        },
        &effects,
    ) {
        Ok(view) => view,
        Err(e) => return report_refusal(ctx, &mol_id, e),
    };
    let (updated, outcome) = (view.updated, view.outcome);

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

/// Print a refused advance the way `cs evolve` always has, and decide
/// whether it is an error exit.
///
/// A verification failure — retryable or final — is reported and exits 0:
/// the molecule is either untouched or parked, and the worker's next move is
/// its own. Every other refusal is an error.
fn report_refusal(ctx: &Context, mol_id: &MoleculeId, err: EvolveError) -> anyhow::Result<()> {
    match err {
        EvolveError::EnergyExhausted { cap } => {
            let reason = format!("energy-exhausted (cap={cap}, remaining=0)");
            if ctx.json {
                let json_out = serde_json::json!({
                    "molecule": mol_id.as_str(),
                    "stuck": true,
                    "reason": reason,
                    "energy_exhausted": true,
                    "energy_cap": cap,
                });
                println!("{}", serde_json::to_string(&json_out).unwrap());
            } else {
                eprintln!("error: {reason}");
                println!("❄️  {mol_id} stuck: {reason}");
            }
            anyhow::bail!("{reason}");
        }
        EvolveError::VerificationRetry {
            step,
            output,
            retries_remaining,
        } => {
            eprintln!(
                "warning: verification failed for step \"{step}\": {output}, {retries_remaining} retries remaining"
            );
            if ctx.json {
                let json_out = serde_json::json!({
                    "molecule": mol_id.as_str(),
                    "verification_failed": true,
                    "step": step,
                    "output": output,
                    "retries_remaining": retries_remaining,
                });
                println!("{}", serde_json::to_string(&json_out).unwrap());
            }
            Ok(())
        }
        EvolveError::VerificationExhausted { reason } => {
            if ctx.json {
                let json_out = serde_json::json!({
                    "molecule": mol_id.as_str(),
                    "verification_failed": true,
                    "stuck": true,
                    "reason": reason,
                });
                println!("{}", serde_json::to_string(&json_out).unwrap());
            } else {
                eprintln!("error: {reason}");
                println!("❄️ {mol_id} stuck: {reason}");
            }
            Ok(())
        }
        EvolveError::ArtifactsMissing {
            step,
            mol_dir,
            missing,
        } => {
            let reason = format!(
                "step \"{step}\" declares acceptance_artifacts that are missing from the molecule directory.\n  \
canonical molecule_dir: {mol_dir}\n  missing: {}\n\n\
Move the misplaced artifact(s) into the canonical directory above, then re-run `cs evolve`. \
The molecule has NOT advanced — its state is unchanged, so this is recoverable until `cs done` tears down the worktree.",
                missing.join(", "),
            );
            if ctx.json {
                let json_out = serde_json::json!({
                    "molecule": mol_id.as_str(),
                    "artifact_guard_failed": true,
                    "advanced": false,
                    "step": step,
                    "molecule_dir": mol_dir,
                    "missing_artifacts": missing,
                });
                println!("{}", serde_json::to_string(&json_out).unwrap());
            } else {
                eprintln!("error: {reason}");
            }
            anyhow::bail!("{reason}");
        }
        EvolveError::NotRunnable(_, status) => {
            Err(cosmon_core::evolve::EvolveError::NotRunnable { status }.into())
        }
        other => Err(other.into()),
    }
}

/// What `cs evolve` does on the worker's own machine around the shared
/// advance. See [`cosmon_state::ops::evolve::evolve`] for why these are not part of it.
struct CliEffects<'a> {
    ctx: &'a Context,
    store: &'a FileStore,
    ops_dir: &'a Path,
    mol_id: &'a MoleculeId,
    formula: &'a Formula,
    main_root: &'a Path,
    project_root: &'a Path,
}

impl EvolveEffects for CliEffects<'_> {
    fn verify_step(
        &self,
        mol: &cosmon_state::MoleculeData,
        step: &cosmon_core::formula::Step,
    ) -> Result<(), StepRefusal> {
        let Some(verification) = &step.verification else {
            return Ok(());
        };
        if verification.criteria.is_empty() {
            return Ok(());
        }
        // Trust gate (B5, RCE-by-clone): `verification.criteria` is a
        // repo-supplied shell string. Refuse to run it unless the operator has
        // vouched for this repository (`cs trust`). This runs BEFORE any state
        // mutation, so a refusal leaves the molecule untouched.
        cosmon_cli::trust::ensure_trusted(self.project_root)
            .map_err(|e| StepRefusal::Other(e.to_string()))?;

        // Run in the worker's own worktree (see `worker_worktree_root`), not
        // the main-repo path derived from `ops_dir` — otherwise verification
        // would shell out against the main worktree's tree state instead of
        // the worker's WIP code.
        let output = Command::new("sh")
            .arg("-c")
            .arg(&verification.criteria)
            .current_dir(self.project_root)
            .output()
            .map_err(|e| StepRefusal::Other(format!("failed to run verification command: {e}")))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = if stderr.is_empty() {
            stdout.to_string()
        } else {
            format!("{stdout}{stderr}")
        };
        let combined = combined.trim().to_owned();
        let _ = mol;
        if verification.max_retries > 0 {
            // Do NOT persist the step advancement — the agent can fix and retry.
            return Err(StepRefusal::Retry {
                output: combined,
                retries_remaining: verification.max_retries,
            });
        }
        Err(StepRefusal::Exhausted {
            reason: format!(
                "verification failed after {} retries: {}",
                verification.max_retries, combined
            ),
        })
    }

    fn repatriate(&self, mol_dir: &Path, missing: &[String]) -> Vec<String> {
        // Lever C (task-20260504-33f6): before the gate fails, try to
        // auto-repatriate any declared artifact the worker wrote to the
        // worktree root instead of the canonical `mol_dir`.
        let repatriated = repatriate_misplaced_artifacts(mol_dir, self.project_root, missing);
        if !repatriated.is_empty() {
            eprintln!(
                "warning: auto-repatriated misplaced acceptance artifact(s) into the molecule directory: {}\n  \
the worker wrote them to the worktree root ({}) instead of the canonical molecule_dir ({}); \
they would have been destroyed at `cs done` teardown.",
                repatriated.join(", "),
                self.project_root.display(),
                mol_dir.display(),
            );
        }
        repatriated
    }

    fn append_legacy_event(&self, events_path: &Path, envelope: &Envelope) {
        let _ = cosmon_filestore::event::append(events_path, envelope);
    }

    fn on_completed(&self, state_dir: &Path, id: &MoleculeId) {
        crate::energy_probe::capture_realized_at_completion(state_dir, id);
    }

    fn warn(&self, message: &str) {
        eprintln!("warning: {message}");
    }

    fn after_advance(
        &self,
        updated: &mut cosmon_state::MoleculeData,
        outcome: &cosmon_core::evolve::EvolveOutcome,
    ) -> Result<(), String> {
        self.auto_commit_and_gate(updated, outcome)
            .map_err(|e| e.to_string())
    }

    fn after_briefing(
        &self,
        updated: &cosmon_state::MoleculeData,
        briefing_path: &Path,
        seal_step: u32,
        causal_parent: Option<cosmon_core::event_v2::Seq>,
    ) -> Result<(), String> {
        let _ = (updated, briefing_path);
        self.seal_bootstrap(seal_step, causal_parent);
        Ok(())
    }
}

impl CliEffects<'_> {
    /// Per-step auto-commit and the inline gate step, in the order the
    /// command has always run them: after the advance is persisted, before
    /// any event is emitted.
    #[allow(clippy::too_many_lines)]
    fn auto_commit_and_gate(
        &self,
        updated: &mut cosmon_state::MoleculeData,
        outcome: &cosmon_core::evolve::EvolveOutcome,
    ) -> anyhow::Result<()> {
        let ctx = self.ctx;
        let store = self.store;
        let ops_dir = self.ops_dir;
        let mol_id = self.mol_id;
        let formula = self.formula;
        let project_root_buf = self.project_root;

        // Worktree guard (idea-20260531-1e1b). The auto-commit below must land
        // in the worktree `cs tackle` assigned to this molecule, never in an
        // unrelated repo the worker happens to be cwd'd in (the genericize
        // ghost-committer). Resolved from the bound worker's recorded repo;
        // `None` for legacy / test molecules, which then behave as today.
        let recorded_worktree = recorded_worktree_for(store, updated, self.main_root);

        // Per-step auto-commit — give each step its own restore anchor.
        // See `auto_commit_step` for the Carnot rationale.
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
                project_root_buf,
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
                    cosmon_cli::trust::ensure_trusted(project_root_buf)?;

                    // Run the gate command outside the lock (shell execution),
                    // in the worker's own worktree — gates such as `cargo test`
                    // must observe the worker's WIP code, not the main repo.
                    let gate_result = Command::new("sh")
                        .arg("-c")
                        .arg(gate_cmd)
                        .current_dir(project_root_buf)
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
                                    formula,
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
                                if updated.status
                                    == cosmon_core::molecule::MoleculeStatus::Completed
                                {
                                    crate::energy_probe::capture_realized_at_completion(
                                        ops_dir, mol_id,
                                    );
                                }
                                store.save_molecule(&updated.id.clone(), updated)?;

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
                                        evidence: Some(gate_request.evidence.clone()),
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
                                            summary: Some(gate_request.evidence.clone()),
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
                                project_root_buf,
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
                                store.save_molecule(&updated.id.clone(), updated)?;
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
                                store.save_molecule(&updated.id.clone(), updated)?;
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

        Ok(())
    }

    /// Bootstrap-context seal (W2 of delib-20260519-e6db, adversary
    /// F2.1+F2.2). The agent-harness bootstrap walk surfaces every
    /// `AGENTS.md` / `CLAUDE.md` from the project root up to the enclosing
    /// `.git/` as fenced `<bootstrap_context>` blocks. We hash that exact byte
    /// stream so a later `cs verify` can re-run the walk and detect
    /// cross-worktree poisoning. Defensive: any failure is logged and
    /// swallowed, never blocks step advance.
    fn seal_bootstrap(&self, seal_step: u32, step_seq: Option<cosmon_core::event_v2::Seq>) {
        let store = self.store;
        let mol_id = self.mol_id;
        let events_path = self.ops_dir.join("events.jsonl");
        let bootstrap_bytes =
            cosmon_agent_harness::bootstrap::collect_bootstrap_context(self.project_root);
        // Snapshot the walk output into the molecule. The bootstrap walk covers
        // the operator's ambient `AGENTS.md` / `CLAUDE.md`, which live OUTSIDE
        // the molecule and drift legitimately; sealing an immutable snapshot of
        // the walk-as-it-was-at-this-step keeps tamper-evidence without
        // alarming on honest ambient evolution.
        let bootstrap_seal =
            BriefingSeal::of_text(seal_step, &bootstrap_bytes).with_snapshot(&bootstrap_bytes);
        // Defensive capture (ADR-131 Decision 2).
        let persisted: Result<(), cosmon_core::error::CosmonError> = 'bootstrap: {
            let _g = match store.lock_fleet() {
                Ok(g) => g,
                Err(e) => break 'bootstrap Err(e),
            };
            let mut mol = match store.load_molecule(mol_id) {
                Ok(m) => m,
                Err(e) => break 'bootstrap Err(e),
            };
            mol.bootstrap_seals.push(bootstrap_seal.clone());
            mol.updated_at = Utc::now();
            store.save_molecule(mol_id, &mol)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::SystemTime;

    #[test]
    fn posture_reference_is_noop_without_the_durable_file() {
        // An ordinary (non-committee) molecule: no `committee-posture.md`, so a
        // regenerated briefing is left exactly as the formula step produced it.
        let tmp = tempfile::tempdir().unwrap();
        let mol_dir = tmp.path();
        let briefing_path = mol_dir.join("briefing.md");
        let original = "# Molecule Briefing\n\n## Current Step 1 of 2\n";
        fs::write(&briefing_path, original).unwrap();

        deliver_committee_posture_reference(mol_dir, &briefing_path).unwrap();

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
        deliver_committee_posture_reference(mol_dir, &briefing_path).unwrap();
        let after_first = fs::read_to_string(&briefing_path).unwrap();
        assert!(after_first.contains(cosmon_core::committee::COMMITTEE_POSTURE_FILE));
        assert!(after_first.contains("## Committee posture"));

        // Second advance on the same briefing: idempotent — no duplicate stanza.
        deliver_committee_posture_reference(mol_dir, &briefing_path).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn artifact_guard_refuses_a_symlink_outside_canonical_custody() {
        use std::os::unix::fs::symlink;

        let molecule = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("result.md"), "outside").unwrap();
        symlink(
            outside.path().join("result.md"),
            molecule.path().join("result.md"),
        )
        .unwrap();

        assert_eq!(
            unsatisfied_expected_artifacts(molecule.path(), &["result.md".to_owned()], None),
            vec!["result.md".to_owned()]
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

    /// Regression for e9f1: an automatic `git add -A` captured 3,256 Cargo
    /// artifacts because that build used an as-yet-unignored target name.
    /// The marker identifies the class, independent of the directory name.
    #[test]
    fn auto_commit_excludes_arbitrarily_named_cachedir_tree() {
        let td = tempfile::tempdir().unwrap();
        init_repo(td.path());
        let target = td.path().join("target-xyz");
        std::fs::create_dir_all(target.join("debug/deps")).unwrap();
        std::fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        std::fs::write(target.join("debug/deps/libfake.rlib"), "generated").unwrap();
        std::fs::write(td.path().join("source.rs"), "fn proof() {}\n").unwrap();

        let outcome = auto_commit_step(td.path(), "mol-cache", 1, 1, "proof").unwrap();
        let sha = match outcome {
            AutoCommitOutcome::Committed(sha) => sha,
            other => panic!("expected source commit, got {other:?}"),
        };
        let show = Command::new("git")
            .args(["show", "--name-only", "--format=", &sha])
            .current_dir(td.path())
            .output()
            .unwrap();
        let committed = String::from_utf8_lossy(&show.stdout);
        assert!(committed.contains("source.rs"), "commit was: {committed}");
        assert!(
            !committed.contains("target-xyz"),
            "CACHEDIR.TAG tree leaked into automatic snapshot: {committed}"
        );

        let status = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(td.path())
            .output()
            .unwrap();
        let remaining = String::from_utf8_lossy(&status.stdout);
        assert!(
            remaining.contains("target-xyz/CACHEDIR.TAG")
                && remaining.contains("target-xyz/debug/deps/libfake.rlib"),
            "generated tree should remain untracked, got: {remaining}"
        );
    }

    #[test]
    fn cache_only_tree_skips_zero_exergy_commit() {
        let td = tempfile::tempdir().unwrap();
        init_repo(td.path());
        let target = td.path().join("bespoke-build-output");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("CACHEDIR.TAG"), "cache marker\n").unwrap();
        std::fs::write(target.join("artifact.bin"), "generated").unwrap();

        let outcome = auto_commit_step(td.path(), "mol-cache", 1, 1, "proof").unwrap();
        assert_eq!(outcome, AutoCommitOutcome::SkippedClean);
    }

    #[test]
    fn abnormal_snapshot_scale_requires_deliberate_commit() {
        let td = tempfile::tempdir().unwrap();
        init_repo(td.path());
        for index in 0..=AUTO_COMMIT_MAX_PATHS {
            std::fs::write(td.path().join(format!("generated-{index:04}.txt")), "x").unwrap();
        }

        let error = auto_commit_step(td.path(), "mol-wide", 1, 1, "proof").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("automatic snapshot refused"), "{message}");
        assert!(message.contains("commit deliberate paths"), "{message}");
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

    /// No recorded worktree → the *mismatch* predicate has nothing to compare
    /// and stays silent. The refusal is the separate
    /// [`unrecorded_worktree_refusal`] guard's job (below); this test pins the
    /// division of labour between the two.
    #[test]
    fn evolve_guard_allows_when_no_recorded_worktree() {
        assert!(evolve_worktree_mismatch(None, Path::new("/anywhere")).is_none());
    }

    /// No recorded worktree → the auto-commit is refused outright: there is no
    /// tree this molecule is entitled to `git add -A`. This is the guard that
    /// would have stopped the `vg-20260725-e4c2` fixture from committing a
    /// worker's WIP during `cargo test --workspace`.
    #[test]
    fn unrecorded_worktree_refuses_auto_commit() {
        assert!(unrecorded_worktree_refusal(None));
    }

    /// A recorded worktree exists → this guard stands aside and lets the
    /// mismatch / shared-checkout guards decide.
    #[test]
    fn recorded_worktree_passes_the_unrecorded_guard() {
        assert!(!unrecorded_worktree_refusal(Some(Path::new(
            "/galaxy/.worktrees/m"
        ))));
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
