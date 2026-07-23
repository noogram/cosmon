// SPDX-License-Identifier: AGPL-3.0-only

//! `surface_visual` mindguard — refuses `cs complete <MOL>` when the
//! molecule touched the *visual* surface without a verify-surface
//! molecule landing GREEN in the preceding `T_max` window.
//!
//! Janis §3a discipline: the gate lives in the CLI critical path, not
//! in a skippable worker hook. Agents cannot forge the gate by
//! fabricating a hook signal — the gate reads the molecule's git diff
//! against the project's main branch and the sibling state store
//! directly.
//!
//! # Signal
//!
//! "Surface touched" is computed at gate time from
//! `git diff --name-only --diff-filter=d <base>...HEAD -- <patterns>`
//! where `<base>` is the molecule's **fork point** and the patterns come
//! from [`config::SurfaceConfig::paths`]. Non-empty diff → surface=touched.
//!
//! ## Deletion-only diffs are not a touch (false-alarm fix, 2026-06-23)
//!
//! The diff is filtered with `--diff-filter=d` (lower-case `d` *excludes*
//! deleted paths), so a molecule that merely **removes** surface-globbed
//! files does not trip the gate. A pure deletion leaves nothing for the
//! verify-surface render step to paint — demanding a visual witness for a
//! surface that no longer exists is structurally unsatisfiable, the same
//! infinite-regress shape the verify-surface self-exemption avoids.
//! Added, modified, renamed, copied, and type-changed surface paths still
//! count as a touch — only the *delete* status is dropped. Observed on
//! task-20260622-eeb9 (scope trim): deleting
//! `crates/almanac/tests/fixtures/scihub/*.html` wrongly demanded a witness.
//!
//! ## Fork-point semantics (ADR-114)
//!
//! The base is *not* unconditionally `origin/main`. Under DAG-aligned
//! branching ([`crate::cmd::tackle`]) a worker's `feat/<MOL>` branch is
//! forked from its **blocker's** branch, not from `main`, so the
//! worktree *carries* (`charrie`) every file the blocker authored. On a
//! knowledge galaxy every branch carries `wiki/**`; diffing against
//! `origin/main` would therefore attribute the blocker's surface edits
//! to `<MOL>` — a category error that fires the gate on a molecule that
//! never touched the surface.
//!
//! The fix: diff against the branch `<MOL>` forked from. Because git's
//! triple-dot `<base>...HEAD` diffs from `merge-base(base, HEAD)`, using
//! the blocker branch as `<base>` yields exactly the fork point — the
//! diff is precisely what `<MOL>` authored on top of its start point,
//! never what the branch inherited. The blocker branches are resolved
//! the same way [`crate::cmd::tackle`] resolves its start point (first
//! `BlockedBy` whose `feat/<dep>` branch exists), so the gate's notion
//! of "what this molecule authored" matches the branch the worktree was
//! actually created from.
//!
//! ## Molecule-ref endpoint (automata misfire, 2026-06-07)
//!
//! The *endpoint* of the diff must be molecule-attributable too. The
//! gate's git commands run with `-C <project_root>` — the galaxy root —
//! where `HEAD` is whatever the root checkout happens to be on
//! (typically `main`), **not** the molecule's branch. Diffing
//! `<blocker>...HEAD` at the root therefore measures *everything merged
//! to main since the blocker forked* — other molecules' work — and so
//! could fire the gate on an advisory molecule whose own branch had an
//! empty diff, blaming it for changes it never made.
//!
//! The fix is symmetric to the fork-point one: when the branch
//! `feat/<MOL>` exists, it is the diff endpoint; `HEAD` is only the
//! fallback when the molecule has no branch (then the invocation
//! context's checkout is the best attribution available — the legacy
//! behaviour). Likewise the last-resort working-tree diff runs in the
//! **molecule's worktree** when one is checked out, never at the galaxy
//! root, whose dirty files (pre-existing, unrelated) are not the
//! molecule's doing.
//!
//! ## Fail-closed direction is preserved
//!
//! Fork-point bases are tried **first**; the freshest of `main` /
//! `origin/main` then the other of the two then a plain `HEAD` diff
//! follow as the conservative fallback. If the blocker branch cannot be
//! resolved (root molecule, blocker merged and its branch deleted, store
//! unreadable) the gate falls back to the freshest local baseline, which
//! over-captures inherited surface — a false *positive*, which fails
//! **closed** (the safe direction). Because the worker protocol forbids
//! pushing, `origin/main` goes permanently stale; preferring the freshest
//! of the two keeps the fallback's over-capture minimal instead of
//! attributing every unpushed merge to the molecule. The
//! narrower fork-point diff can never miss a surface `<MOL>` authored:
//! every commit `<MOL>` makes is reachable from `HEAD` and strictly
//! after `merge-base(blocker, HEAD)`, so it is always inside
//! `<blocker>...HEAD`. There is no false-negative path (a real surface
//! escaping the gate) introduced by the narrowing.
//!
//! # GREEN verify-surface
//!
//! A sibling molecule satisfies the gate when *all* hold:
//! - `formula_id` is `verify-surface`;
//! - variables contain `target=<MOL>`;
//! - status is [`MoleculeStatus::Completed`];
//! - `updated_at` is within `T_max` of *now*.
//!
//! # Fail-closed
//!
//! Any uncertainty — git command fails, state store unreachable, no
//! project root resolvable — surfaces as
//! [`MindguardError::Unavailable`]. The operator may bypass with
//! `--override-mindguard-down --justification "…"`; the override is
//! recorded write-once in [`super::ledger`] *before* the wrapped
//! operation runs.

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use cosmon_core::id::MoleculeId;
use cosmon_core::molecule::MoleculeStatus;
use cosmon_filestore::FileStore;
use cosmon_state::{MoleculeFilter, StateStore};

use super::config::{self, SurfaceConfig};
use super::MindguardError;

/// Gate `cs complete <mol_id>`.
///
/// Returns `Ok(())` when:
/// - the molecule did not touch the visual surface, OR
/// - a sibling `verify-surface` molecule landed GREEN inside `T_max`.
///
/// Returns `Err(Refused)` when surface is touched and no GREEN
/// verify-surface exists. Returns `Err(Unavailable)` when the gate
/// machinery itself cannot run.
///
/// # Errors
///
/// See [`MindguardError`].
pub fn gate(store: &FileStore, mol_id: &MoleculeId) -> Result<(), MindguardError> {
    let config = config::load().map_err(MindguardError::Unavailable)?;
    gate_with_config(store, mol_id, &config, Utc::now())
}

/// Gate with an explicit config + clock for tests.
pub(crate) fn gate_with_config(
    store: &FileStore,
    mol_id: &MoleculeId,
    config: &SurfaceConfig,
    now: chrono::DateTime<Utc>,
) -> Result<(), MindguardError> {
    let project_root = store.project_root().ok_or_else(|| {
        MindguardError::Unavailable(
            "no project root resolvable from state store (test fixture?)".to_owned(),
        )
    })?;

    // Self-exemption: a `verify-surface` molecule *is* the terminal
    // observation that mints the GREEN this gate looks for. Gating its
    // own completion on a sibling `verify-surface` is infinite regress —
    // no verify-surface can ever land GREEN while the surface reads as
    // touched, so the remedy it prints is structurally unsatisfiable.
    // The witness pass recorded by this molecule is the safety, not a
    // sibling. (cosmon-ward D9 from automata, verify-20260610-df54.)
    if let Ok(mol) = store.load_molecule(mol_id) {
        if mol.formula_id.as_str() == "verify-surface" {
            return Ok(());
        }
    }

    // Resolve the branch(es) this molecule forked from so the surface
    // diff is computed against its fork point, not against main. See the
    // module-level "Fork-point semantics" doc (ADR-114).
    let molecule_bases = molecule_fork_bases(store, mol_id, &project_root);

    // Resolve the diff *endpoint* to the molecule's own branch when it
    // exists. The gate runs git at the galaxy root, whose HEAD is the
    // root checkout (main) — not the molecule. See the module-level
    // "Molecule-ref endpoint" doc (automata misfire, 2026-06-07).
    let mol_branch = format!("feat/{}", mol_id.as_str());
    let (head_ref, plain_diff_dir) = if branch_exists(&project_root, &mol_branch) {
        let worktree =
            molecule_worktree(&project_root, &mol_branch).unwrap_or_else(|| project_root.clone());
        (mol_branch, worktree)
    } else {
        ("HEAD".to_owned(), project_root.clone())
    };

    let touched = surface_touched(
        &project_root,
        &config.paths,
        &molecule_bases,
        &head_ref,
        &plain_diff_dir,
    )?;
    if !touched {
        return Ok(());
    }

    let mol_str = mol_id.as_str();
    let molecules = store
        .list_molecules(&MoleculeFilter::default())
        .map_err(|e| MindguardError::Unavailable(format!("list_molecules failed: {e}")))?;

    let cutoff = now - config.t_max;
    let mut latest_completed: Option<chrono::DateTime<Utc>> = None;

    for mol in &molecules {
        if mol.formula_id.as_str() != "verify-surface" {
            continue;
        }
        let target = mol.variables.get("target");
        if target.map(String::as_str) != Some(mol_str) {
            continue;
        }
        if mol.status != MoleculeStatus::Completed {
            continue;
        }
        // Authoritative timestamp for "landed GREEN at": `updated_at`,
        // which is rewritten at status transition by `complete_one`.
        if mol.updated_at < cutoff {
            continue;
        }
        latest_completed = Some(match latest_completed {
            Some(prev) if prev > mol.updated_at => prev,
            _ => mol.updated_at,
        });
    }

    if latest_completed.is_some() {
        Ok(())
    } else {
        Err(MindguardError::Refused(format!(
            "molecule {mol_str} touched the visual surface but no \
             verify-surface molecule landed GREEN in the last {} minutes. \
             Remedy: cs nucleate verify-surface --var target={mol_str}",
            config.t_max.num_minutes()
        )))
    }
}

/// Resolve the branch refs this molecule's worktree was forked from, in
/// the same order [`crate::cmd::tackle`] resolves its start point.
///
/// These are the molecule's `BlockedBy` predecessors whose `feat/<dep>`
/// branch currently exists. Used as the highest-priority diff base so
/// the surface diff reflects what *this* molecule authored on top of its
/// fork point, not what its branch inherited (ADR-114). Returns an empty
/// vec for a root molecule, a molecule whose blockers have all merged
/// and had their branches deleted, or when the molecule cannot be loaded
/// — every such case falls back to the conservative `origin/main` base.
fn molecule_fork_bases(store: &FileStore, mol_id: &MoleculeId, project_root: &Path) -> Vec<String> {
    let Ok(mol) = store.load_molecule(mol_id) else {
        return Vec::new();
    };
    mol.blocked_by()
        .into_iter()
        .map(|dep| format!("feat/{dep}"))
        .filter(|branch| branch_exists(project_root, branch))
        .collect()
}

/// True iff `refs/heads/<branch>` exists. Branch heads are shared across
/// all linked worktrees of a repo, so this resolves the blocker branch
/// even when called from inside the molecule's own worktree.
fn branch_exists(project_root: &Path, branch: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Return the conservative fallback diff bases (`main`, `origin/main`)
/// ordered **freshest first**, including only refs that actually exist.
///
/// The cosmon worker protocol forbids pushing to remote, so `origin/main`
/// goes permanently stale: local `main` accumulates every `cs done` merge
/// while the remote-tracking ref stays frozen at the last fetch. Diffing
/// against a stale `origin/main` over-captures every inherited commit as
/// if *this* molecule had authored it — the fleet-wide misfire reported
/// cosmon-ward from automata (verify-20260610-df54: 10 unpushed commits,
/// 12 `wiki/**` files falsely attributed). Preferring the freshest of the
/// two keeps the fallback as tight as the available refs allow.
///
/// "Freshest" = the descendant. In the no-push steady state `origin/main`
/// is an ancestor of `main`, so `main` is fresher and tried first. On a
/// divergent topology (neither is an ancestor of the other) the order is
/// indifferent for correctness — both remain in the list as fallbacks —
/// so `main` is kept first as the local source of truth.
fn freshest_fallback_bases(project_root: &Path) -> Vec<String> {
    let main_exists = ref_exists(project_root, "main");
    let origin_exists = ref_exists(project_root, "origin/main");
    match (main_exists, origin_exists) {
        (true, true) => {
            if is_ancestor(project_root, "main", "origin/main")
                && !is_ancestor(project_root, "origin/main", "main")
            {
                // origin/main strictly ahead of main → origin/main fresher.
                vec!["origin/main".to_owned(), "main".to_owned()]
            } else {
                // main ahead (the no-push steady state) or divergent →
                // local main is the source of truth, tried first.
                vec!["main".to_owned(), "origin/main".to_owned()]
            }
        }
        (true, false) => vec!["main".to_owned()],
        (false, true) => vec!["origin/main".to_owned()],
        (false, false) => Vec::new(),
    }
}

/// True iff `<gitref>` resolves to a commit in `project_root`'s repo.
/// Works for both local branches (`main`) and remote-tracking refs
/// (`origin/main`) via the `^{commit}` peel.
fn ref_exists(project_root: &Path, gitref: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{gitref}^{{commit}}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// True iff `ancestor` is an ancestor of (or equal to) `descendant`.
/// Thin wrapper over `git merge-base --is-ancestor`, whose exit status
/// is 0 for ancestor, 1 otherwise.
fn is_ancestor(project_root: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Compute whether the molecule's attributable diff touches a surface
/// pattern.
///
/// Source-of-truth ranking (most → least preferred):
/// 1. `git diff <base>...<head>` for each fork-point base in
///    `molecule_bases` (the blocker branches this molecule forked from)
/// 2. same, with `<base>` = the **freshest** of `{main, origin/main}`
///    (see [`freshest_fallback_bases`]) then the other of the two
/// 3. plain `git diff HEAD` *in `plain_diff_dir`* (covers worktrees not
///    yet merged anywhere)
///
/// `head` is the molecule's `feat/<MOL>` branch when it exists, `HEAD`
/// otherwise — diffing the root checkout's HEAD attributes other
/// molecules' merged work to `<MOL>` (the automata misfire; see the
/// module-level "Molecule-ref endpoint" doc). `plain_diff_dir` is the
/// molecule's worktree when one is checked out, so the last-resort
/// working-tree diff never reads the galaxy root's unrelated dirty
/// files.
///
/// The fork-point bases come first so the diff attributes only what the
/// molecule *authored* (triple-dot diffs from `merge-base(base, head)`).
/// The `main` / `origin/main` fallbacks over-capture inherited surface,
/// which fails **closed** — the safe direction — but the freshest-first
/// ordering keeps the over-capture minimal: a stale `origin/main` (no
/// push by protocol) would otherwise attribute every inherited merge to
/// the molecule. See ADR-114 and the module-level "Fork-point semantics"
/// doc.
///
/// Every diff is computed with `--diff-filter=d`, so a molecule that
/// only *deletes* surface files never reads as a touch — there is no
/// rendered surface left for a witness to paint (see [`git_diff_names`]).
///
/// If [`is_git_repo`] returns false the gate treats the surface as
/// untouched (the gate is *for* git-tracked surfaces — a project
/// without git has no surface the gate is meant to protect).
///
/// Failure of *all* diff attempts on an *existing* git repo is
/// [`MindguardError::Unavailable`] — we never silently say "no surface
/// touched" when we cannot tell.
fn surface_touched(
    project_root: &Path,
    patterns: &[String],
    molecule_bases: &[String],
    head: &str,
    plain_diff_dir: &Path,
) -> Result<bool, MindguardError> {
    if !is_git_repo(project_root) {
        // No git repo at the project root: the surface gate has no
        // surface to protect. This is the legitimate flat-tempdir
        // path used by every cosmon-cli integration test that
        // constructs a FileStore at `<tmp>/state` without `git init`.
        return Ok(false);
    }

    // Fork-point bases first (molecule-attributable diff), then the
    // conservative `main` / `origin/main` fallbacks ordered freshest
    // first. The worker protocol forbids pushing, so `origin/main` goes
    // permanently stale and over-captures inherited history; the freshest
    // of the two is the tightest honest baseline (cosmon-ward D9 from
    // automata, 2026-06-10).
    let fallbacks = freshest_fallback_bases(project_root);
    let candidates: Vec<&str> = molecule_bases
        .iter()
        .map(String::as_str)
        .chain(fallbacks.iter().map(String::as_str))
        .collect();
    let mut last_err: Option<String> = None;

    for base in candidates {
        match git_diff_names(project_root, base, head) {
            Ok(names) => return Ok(names.iter().any(|n| matches_any(n, patterns))),
            Err(e) => last_err = Some(e),
        }
    }

    // Last resort: diff the working tree against HEAD itself — in the
    // molecule's worktree when one exists, so the galaxy root's
    // unrelated dirty files are never attributed to the molecule.
    // Catches uncommitted-but-staged changes on a fresh checkout;
    // misses already-committed changes but those should be against
    // main first.
    match git_diff_names_plain(plain_diff_dir) {
        Ok(names) => Ok(names.iter().any(|n| matches_any(n, patterns))),
        Err(e) => Err(MindguardError::Unavailable(format!(
            "git diff against fork-point bases, main/origin/main, and HEAD all failed in {}; \
             last: {e} (prior errors: {})",
            project_root.display(),
            last_err.unwrap_or_default()
        ))),
    }
}

/// Resolve the checked-out worktree of `branch`, if any.
///
/// Parses `git worktree list --porcelain` from the project root —
/// worktree records are shared across all linked worktrees, so this
/// finds the molecule's `.worktrees/<MOL>` checkout regardless of
/// where the gate itself runs. `None` when the branch is not checked
/// out anywhere (e.g. the worker already tore down, or the branch was
/// created without `cs tackle`).
fn molecule_worktree(project_root: &Path, branch: &str) -> Option<std::path::PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let wanted = format!("branch refs/heads/{branch}");
    let mut current: Option<std::path::PathBuf> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(std::path::PathBuf::from(path));
        } else if line == wanted {
            return current;
        }
    }
    None
}

/// Return true if `project_root` is inside a git work tree.
fn is_git_repo(project_root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"true"))
}

/// Run `git diff --name-only --diff-filter=d <base>...<head>` and parse
/// the names.
///
/// The filter keeps Added / Modified / Renamed / Copied / Type-changed paths
/// and drops pure deletions: a molecule that only removes surface files leaves
/// nothing to render, so it must not read as a surface touch (false-alarm fix,
/// 2026-06-23). We spell the kept classes explicitly as `--diff-filter=ACMRT`
/// (an *inclusion* set) rather than the equivalent pure-exclusion `d`
/// (issue #5, item 2): the exclusion-only spelling is the fragile form that
/// leaks a `git diff` usage screen on some older/downstream git builds, whereas
/// an explicit inclusion set is unambiguous on every git that ships diff-filter.
fn git_diff_names(project_root: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .arg("diff")
        .arg("--name-only")
        .arg("--diff-filter=ACMRT")
        .arg(format!("{base}...{head}"))
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff {base}...{head} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .collect())
}

/// Fallback: `git diff --name-only --diff-filter=d HEAD` (no triple-dot,
/// no merge-base).
///
/// Used only when the merge-base candidates (`origin/main`, `main`)
/// have all failed — typically because the worktree is on a fresh
/// branch with no shared ancestor recorded yet. Catches the staged
/// changes that *will* land on main. `dir` is the molecule's worktree
/// when one is checked out, the project root otherwise — a working-tree
/// diff is only molecule-attributable in the molecule's own checkout.
///
/// `--diff-filter=ACMRT` keeps the same classes as [`git_diff_names`] and drops
/// pure deletions for the same reason: removing surface files is not a surface
/// touch. The explicit inclusion set is used in place of the pure-exclusion `d`
/// for the portability reason documented on [`git_diff_names`] (issue #5).
fn git_diff_names_plain(dir: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("diff")
        .arg("--name-only")
        .arg("--diff-filter=ACMRT")
        .arg("HEAD")
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .collect())
}

/// Test if any pattern matches `path`.
fn matches_any(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| matches_glob(path, p))
}

/// Minimal glob matcher for the patterns the mindguard config accepts.
///
/// Supports two idioms (the only ones used by the default and any
/// reasonable override):
///
/// - `<prefix>/**` — match any path whose first segment chain is
///   `<prefix>/`. Equivalent to `path.starts_with("<prefix>/")`.
/// - `**/*.<ext>` — match any path ending with `.<ext>`.
///   Equivalent to `path.ends_with(".<ext>")`.
/// - exact literal — fallback.
///
/// We deliberately do not pull `globset` for three patterns; the
/// firebreak surface is small enough that a 20-line matcher with
/// exhaustive unit tests beats a 200kB dependency.
fn matches_glob(path: &str, pattern: &str) -> bool {
    // `<prefix>/**` form.
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    // `**/*.<ext>` form.
    if let Some(suffix) = pattern.strip_prefix("**/*.") {
        return path.ends_with(&format!(".{suffix}"));
    }
    // `*.<ext>` form (no `**/`, equivalent to ends_with).
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return path.ends_with(&format!(".{suffix}"));
    }
    path == pattern
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{FleetId, FormulaId};
    use cosmon_state::MoleculeData;
    use std::collections::HashMap;
    use tempfile::TempDir;

    // --- matches_glob --------------------------------------------------

    #[test]
    fn glob_double_star_suffix_matches_subtree() {
        assert!(matches_glob("wiki/foo.md", "wiki/**"));
        assert!(matches_glob("wiki/sub/dir/page.md", "wiki/**"));
        // exact prefix without slash should still match.
        assert!(matches_glob("wiki", "wiki/**"));
        // sibling that merely shares the prefix must NOT match.
        assert!(!matches_glob("wiki-archive/x.md", "wiki/**"));
        // unrelated paths must not match.
        assert!(!matches_glob("docs/foo.md", "wiki/**"));
    }

    #[test]
    fn glob_double_star_prefix_matches_extension() {
        assert!(matches_glob("foo.html", "**/*.html"));
        assert!(matches_glob("a/b/c.html", "**/*.html"));
        assert!(!matches_glob("a/b/c.htm", "**/*.html"));
    }

    #[test]
    fn glob_lumen_web_pattern() {
        assert!(matches_glob(
            "poc/optix-modernization/lumen/web/index.html",
            "poc/optix-modernization/lumen/web/**"
        ));
        assert!(!matches_glob(
            "poc/other-modernization/index.html",
            "poc/optix-modernization/lumen/web/**"
        ));
    }

    // --- gate logic ----------------------------------------------------

    /// Build a `FileStore` rooted at `<tmp>/.cosmon/state` so
    /// `project_root()` resolves to `<tmp>` — required by `gate`
    /// because we use git from the project root.
    fn make_store_in_repo() -> (TempDir, FileStore) {
        let tmp = TempDir::new().unwrap();
        // Initialise an empty git repo at the project root so git diff
        // calls succeed (returning an empty diff).
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["init", "--quiet", "--initial-branch=main"])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "user.email", "test@example.com"])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["config", "user.name", "test"])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "--allow-empty", "-m", "init", "--quiet"])
            .output();

        let state_dir = tmp.path().join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        (tmp, store)
    }

    /// Run a git command in `repo`, asserting success.
    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Resolve a git ref to its full SHA in `repo`, asserting success.
    fn git_rev_parse(repo: &Path, gitref: &str) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", gitref])
            .output()
            .unwrap();
        assert!(out.status.success(), "git rev-parse {gitref} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn sample_mol(id: &str, formula: &str, status: MoleculeStatus) -> MoleculeData {
        MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: FleetId::new("default").unwrap(),
            formula_id: FormulaId::new(formula).unwrap(),
            status,
            variables: HashMap::new(),
            assigned_worker: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
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
    fn gate_passes_when_surface_untouched() {
        let (_tmp, store) = make_store_in_repo();
        let mol = sample_mol("task-20260527-aaaa", "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();
        let cfg = SurfaceConfig::default();
        // Empty git diff (no commits beyond init) → not touched.
        gate_with_config(&store, &mol.id, &cfg, Utc::now()).expect("untouched should pass");
    }

    #[test]
    fn gate_is_moot_when_project_root_has_no_git_repo() {
        // FileStore rooted at a flat temp dir → project_root() resolves
        // to some real ancestor path that is not inside any git work
        // tree. The gate is *for* git-tracked surfaces; without git
        // there is no surface to protect, so the gate passes through.
        //
        // This is the legitimate path used by integration tests that
        // construct a FileStore at `<tmp>/state` without `git init` —
        // and by the historical `cs complete` invocations in those
        // tests, which predate the mindguard.
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = sample_mol("task-20260527-bbbb", "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();
        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &mol.id, &cfg, Utc::now())
            .expect("no-git ancestry is moot, not unavailable");
    }

    #[test]
    fn gate_refuses_when_surface_touched_and_no_verify() {
        let (tmp, store) = make_store_in_repo();
        // Commit an HTML file to introduce a surface-touching diff.
        std::fs::write(tmp.path().join("page.html"), "<html/>").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["checkout", "-q", "-b", "feat/touch"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "page.html"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "-m", "touch surface", "--quiet"])
            .output()
            .unwrap();

        let mol = sample_mol("task-20260527-cccc", "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        let err = gate_with_config(&store, &mol.id, &cfg, Utc::now()).unwrap_err();
        match err {
            MindguardError::Refused(msg) => {
                assert!(msg.contains("verify-surface"), "{msg}");
                assert!(msg.contains("task-20260527-cccc"), "{msg}");
            }
            other @ MindguardError::Unavailable(_) => {
                panic!("expected Refused, got {other:?}")
            }
        }
    }

    #[test]
    fn gate_passes_when_verify_surface_completed_within_window() {
        let (tmp, store) = make_store_in_repo();
        std::fs::write(tmp.path().join("page.html"), "<html/>").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["checkout", "-q", "-b", "feat/touch"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "page.html"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "-m", "touch surface", "--quiet"])
            .output()
            .unwrap();

        let target = sample_mol("task-20260527-dddd", "task-work", MoleculeStatus::Running);
        store.save_molecule(&target.id, &target).unwrap();

        let now = Utc::now();
        let mut verify = sample_mol(
            "verify-20260527-eeee",
            "verify-surface",
            MoleculeStatus::Completed,
        );
        verify
            .variables
            .insert("target".to_owned(), "task-20260527-dddd".to_owned());
        verify.updated_at = now - chrono::Duration::minutes(5);
        store.save_molecule(&verify.id, &verify).unwrap();

        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &target.id, &cfg, now).expect("GREEN verify should unlock");
    }

    #[test]
    fn gate_refuses_when_verify_surface_too_old() {
        let (tmp, store) = make_store_in_repo();
        std::fs::write(tmp.path().join("style.css"), "body{}").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["checkout", "-q", "-b", "feat/touch"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "style.css"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "-m", "touch surface", "--quiet"])
            .output()
            .unwrap();

        let target = sample_mol("task-20260527-ffff", "task-work", MoleculeStatus::Running);
        store.save_molecule(&target.id, &target).unwrap();

        let now = Utc::now();
        let mut verify = sample_mol(
            "verify-20260527-1111",
            "verify-surface",
            MoleculeStatus::Completed,
        );
        verify
            .variables
            .insert("target".to_owned(), "task-20260527-ffff".to_owned());
        // Past the 60-minute window.
        verify.updated_at = now - chrono::Duration::minutes(120);
        store.save_molecule(&verify.id, &verify).unwrap();

        let cfg = SurfaceConfig::default();
        let err = gate_with_config(&store, &target.id, &cfg, now).unwrap_err();
        assert!(matches!(err, MindguardError::Refused(_)), "{err}");
    }

    #[test]
    fn gate_refuses_when_verify_surface_targets_other_molecule() {
        let (tmp, store) = make_store_in_repo();
        std::fs::write(tmp.path().join("app.js"), "// hi").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["checkout", "-q", "-b", "feat/touch"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "app.js"])
            .output()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "-m", "touch surface", "--quiet"])
            .output()
            .unwrap();

        let target = sample_mol("task-20260527-2222", "task-work", MoleculeStatus::Running);
        store.save_molecule(&target.id, &target).unwrap();

        let now = Utc::now();
        let mut decoy = sample_mol(
            "verify-20260527-3333",
            "verify-surface",
            MoleculeStatus::Completed,
        );
        decoy
            .variables
            .insert("target".to_owned(), "task-20260527-zzzz".to_owned());
        decoy.updated_at = now - chrono::Duration::minutes(5);
        store.save_molecule(&decoy.id, &decoy).unwrap();

        let cfg = SurfaceConfig::default();
        let err = gate_with_config(&store, &target.id, &cfg, now).unwrap_err();
        assert!(matches!(err, MindguardError::Refused(_)), "{err}");
    }

    /// Acceptance criterion #5 (briefing): a test that forces
    /// [`MindguardError::Unavailable`] through the public `gate()` entry
    /// point. We trigger it by pointing the config loader at a malformed
    /// TOML file via `$COSMON_MINDGUARD_SURFACE_CONFIG` — `gate()` calls
    /// `config::load()` first, which fails fast.
    ///
    /// Serialized against other tests that might touch the env var by
    /// using a unique path; the var is restored on drop.
    #[test]
    fn gate_returns_unavailable_when_config_is_malformed() {
        struct EnvGuard {
            key: &'static str,
            prev: Option<String>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }

        let (_tmp, store) = make_store_in_repo();
        let mol = sample_mol("task-20260527-9999", "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let bad_config = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(bad_config.path(), "[[[ not valid toml").unwrap();

        let _g = EnvGuard {
            key: "COSMON_MINDGUARD_SURFACE_CONFIG",
            prev: std::env::var("COSMON_MINDGUARD_SURFACE_CONFIG").ok(),
        };
        std::env::set_var("COSMON_MINDGUARD_SURFACE_CONFIG", bad_config.path());

        let err = gate(&store, &mol.id).unwrap_err();
        match err {
            MindguardError::Unavailable(msg) => {
                assert!(msg.contains("parse"), "{msg}");
            }
            other @ MindguardError::Refused(_) => {
                panic!("expected Unavailable, got {other:?}")
            }
        }
    }

    // --- fork-point semantics (ADR-114) --------------------------------

    /// Build the DAG-aligned topology that motivates ADR-114:
    ///
    /// ```text
    /// main:                  init
    /// feat/<blocker>:        init - (authors page.html, a surface file)
    /// feat/<mol>:            init - blocker - (authors logic.rs, no surface)
    /// ```
    ///
    /// Leaves HEAD on `feat/<mol>`. Returns `(blocker_id, mol_id)`.
    fn make_blocker_topology(root: &Path) -> (&'static str, &'static str) {
        let blocker = "task-20260531-bbbb";
        let mol = "task-20260531-mmmm";
        git(root, &["checkout", "-q", "-b", &format!("feat/{blocker}")]);
        std::fs::write(root.join("page.html"), "<html/>").unwrap();
        git(root, &["add", "page.html"]);
        git(root, &["commit", "-qm", "blocker authors surface"]);
        git(root, &["checkout", "-q", "-b", &format!("feat/{mol}")]);
        std::fs::write(root.join("logic.rs"), "fn main() {}").unwrap();
        git(root, &["add", "logic.rs"]);
        git(
            root,
            &[
                "commit",
                "-qm",
                "evolve(task-20260531-mmmm): step 1/1 — code",
            ],
        );
        (blocker, mol)
    }

    /// The core regression: `surface_touched` with no fork base (the
    /// pre-ADR-114 behaviour) attributes the blocker's `page.html` to the
    /// molecule; with the blocker branch as the fork base it sees only the
    /// molecule's own `logic.rs`.
    #[test]
    fn surface_touched_ignores_blocker_inherited_files() {
        let (tmp, _store) = make_store_in_repo();
        let root = tmp.path();
        let (blocker, _mol) = make_blocker_topology(root);
        let patterns = SurfaceConfig::default().paths;

        // Pre-fix shape: diff against main captures the inherited surface.
        assert!(
            surface_touched(root, &patterns, &[], "HEAD", root).unwrap(),
            "without a fork base the inherited page.html is (wrongly) counted"
        );

        // Fixed shape: diff against the blocker branch (the fork point)
        // sees only logic.rs — no surface.
        assert!(
            !surface_touched(root, &patterns, &[format!("feat/{blocker}")], "HEAD", root).unwrap(),
            "fork-point base must exclude what the branch merely inherited"
        );
    }

    /// End-to-end through `gate_with_config`: a molecule that only
    /// inherited a surface file from its blocker seals without a
    /// `verify-surface` sibling.
    #[test]
    fn gate_passes_when_only_blocker_authored_surface() {
        let (tmp, store) = make_store_in_repo();
        let (blocker, mol_id) = make_blocker_topology(tmp.path());

        let mut mol = sample_mol(mol_id, "task-work", MoleculeStatus::Running);
        mol.typed_links
            .push(cosmon_core::interaction::MoleculeLink::BlockedBy {
                source: MoleculeId::new(blocker).unwrap(),
            });
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &mol.id, &cfg, Utc::now())
            .expect("molecule that only inherited surface from its blocker should pass");
    }

    /// No false negative: a molecule that authors a surface file *on top
    /// of* its blocker is still gated. The fork-point narrowing must not
    /// let a real surface escape.
    #[test]
    fn gate_refuses_when_molecule_authors_surface_atop_blocker() {
        let (tmp, store) = make_store_in_repo();
        let root = tmp.path();
        let blocker = "task-20260531-bbbb";
        let mol_id = "task-20260531-mmmm";

        // Blocker authors only non-surface code.
        git(root, &["checkout", "-q", "-b", &format!("feat/{blocker}")]);
        std::fs::write(root.join("lib.rs"), "// blocker").unwrap();
        git(root, &["add", "lib.rs"]);
        git(root, &["commit", "-qm", "blocker authors code"]);

        // Molecule forks from the blocker and authors a surface file.
        git(root, &["checkout", "-q", "-b", &format!("feat/{mol_id}")]);
        std::fs::write(root.join("page.html"), "<html/>").unwrap();
        git(root, &["add", "page.html"]);
        git(
            root,
            &[
                "commit",
                "-qm",
                "evolve(task-20260531-mmmm): step 1/1 — surface",
            ],
        );

        let mut mol = sample_mol(mol_id, "task-work", MoleculeStatus::Running);
        mol.typed_links
            .push(cosmon_core::interaction::MoleculeLink::BlockedBy {
                source: MoleculeId::new(blocker).unwrap(),
            });
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        let err = gate_with_config(&store, &mol.id, &cfg, Utc::now()).unwrap_err();
        assert!(
            matches!(err, MindguardError::Refused(_)),
            "molecule authoring surface atop a blocker must still be gated: {err}"
        );
    }

    /// `molecule_fork_bases` returns only blocker branches that actually
    /// exist — a root molecule (no blockers) yields none, so the gate
    /// falls back to the conservative `origin/main` base.
    #[test]
    fn fork_bases_empty_for_root_molecule() {
        let (tmp, store) = make_store_in_repo();
        let mol = sample_mol("task-20260531-root", "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();
        let bases = molecule_fork_bases(&store, &mol.id, tmp.path());
        assert!(
            bases.is_empty(),
            "root molecule has no fork base: {bases:?}"
        );
    }

    // --- molecule-ref endpoint (automata misfire, 2026-06-07) ----------

    /// The automata misfire reproduction: an advisory molecule whose
    /// own branch authored **no** surface, while
    ///
    /// 1. the galaxy-root checkout sits on `main`, which has advanced
    ///    with *other* molecules' surface work (`wiki/**` merges) since
    ///    the blocker forked, and
    /// 2. the root working tree carries an unrelated, pre-existing
    ///    dirty surface file (`styles.css`).
    ///
    /// Pre-fix the gate diffed `<blocker>...HEAD` at the root
    /// (HEAD = main) and attributed point 1 to the molecule. The gate
    /// must NOT fire: the molecule-attributable diff is empty of
    /// surface.
    #[test]
    fn gate_passes_for_clean_molecule_when_root_checkout_dirty_and_main_advanced() {
        let (tmp, store) = make_store_in_repo();
        let root = tmp.path();
        let blocker = "mission-20260607-aaaa";
        let mol_id = "task-20260607-cccc";

        // A surface file tracked on main from the start.
        std::fs::write(root.join("styles.css"), "body{}").unwrap();
        git(root, &["add", "styles.css"]);
        git(root, &["commit", "-qm", "seed surface file"]);

        // Blocker (mission) branch forks from main here.
        git(root, &["branch", &format!("feat/{blocker}")]);

        // Molecule forks from the blocker and authors only Markdown.
        git(root, &["checkout", "-q", &format!("feat/{blocker}")]);
        git(root, &["checkout", "-q", "-b", &format!("feat/{mol_id}")]);
        std::fs::write(root.join("algo-spec.md"), "# notes").unwrap();
        git(root, &["add", "algo-spec.md"]);
        git(root, &["commit", "-qm", "molecule authors a note"]);

        // Meanwhile main advances with other molecules' surface work…
        git(root, &["checkout", "-q", "main"]);
        std::fs::create_dir_all(root.join("wiki")).unwrap();
        std::fs::write(root.join("wiki/index.md"), "# wiki").unwrap();
        git(root, &["add", "wiki"]);
        git(
            root,
            &["commit", "-qm", "other molecule's wiki work on main"],
        );

        // …and the root working tree carries an unrelated dirty
        // surface file.
        std::fs::write(root.join("styles.css"), "body{color:red}").unwrap();

        let mut mol = sample_mol(mol_id, "task-work", MoleculeStatus::Running);
        mol.typed_links
            .push(cosmon_core::interaction::MoleculeLink::BlockedBy {
                source: MoleculeId::new(blocker).unwrap(),
            });
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &mol.id, &cfg, Utc::now()).expect(
            "molecule whose own branch authored no surface must pass, \
             whatever the root checkout looks like",
        );
    }

    /// The last-resort working-tree diff must run in the molecule's
    /// worktree, not at the galaxy root. Forced by a repo whose default
    /// branch is not `main` and that has no `origin` remote: every
    /// `<base>...<head>` candidate fails, so the plain diff decides.
    /// The root is dirty with a surface file; the molecule's worktree
    /// is clean → the gate must pass.
    #[test]
    fn plain_fallback_diffs_molecule_worktree_not_root_checkout() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "--initial-branch=trunk"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "test"]);
        std::fs::write(root.join("styles.css"), "body{}").unwrap();
        git(root, &["add", "styles.css"]);
        git(root, &["commit", "-qm", "seed"]);

        let mol_id = "task-20260610-wwww";
        git(root, &["branch", &format!("feat/{mol_id}")]);
        let wt = root.join(".worktrees").join(mol_id);
        git(
            root,
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                &format!("feat/{mol_id}"),
            ],
        );

        // Root checkout dirty with an unrelated surface edit; the
        // molecule's worktree stays clean.
        std::fs::write(root.join("styles.css"), "body{color:red}").unwrap();

        let state_dir = root.join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = sample_mol(mol_id, "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &mol.id, &cfg, Utc::now())
            .expect("plain fallback must read the molecule's worktree, not the dirty root");
    }

    /// No false negative through the worktree scoping: uncommitted
    /// surface work sitting in the molecule's *own* worktree is still
    /// caught by the plain fallback.
    #[test]
    fn plain_fallback_still_catches_dirty_surface_in_molecule_worktree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "--initial-branch=trunk"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "test"]);
        std::fs::write(root.join("styles.css"), "body{}").unwrap();
        git(root, &["add", "styles.css"]);
        git(root, &["commit", "-qm", "seed"]);

        let mol_id = "task-20260610-xxxx";
        git(root, &["branch", &format!("feat/{mol_id}")]);
        let wt = root.join(".worktrees").join(mol_id);
        git(
            root,
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                &format!("feat/{mol_id}"),
            ],
        );

        // The molecule's worktree carries uncommitted surface work.
        std::fs::write(wt.join("styles.css"), "body{color:blue}").unwrap();

        let state_dir = root.join(".cosmon").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = FileStore::new(state_dir);
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = sample_mol(mol_id, "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        let err = gate_with_config(&store, &mol.id, &cfg, Utc::now()).unwrap_err();
        assert!(
            matches!(err, MindguardError::Refused(_)),
            "uncommitted surface in the molecule's own worktree must still gate: {err}"
        );
    }

    /// The corrected remedy string must be a valid `cs` invocation:
    /// formula positional, no `--formula` flag (the automata blocker's
    /// defect #2 — the prescribed command failed to parse).
    #[test]
    fn refusal_remedy_is_positional_nucleate_syntax() {
        let (tmp, store) = make_store_in_repo();
        std::fs::write(tmp.path().join("page.html"), "<html/>").unwrap();
        git(tmp.path(), &["checkout", "-q", "-b", "feat/touch"]);
        git(tmp.path(), &["add", "page.html"]);
        git(tmp.path(), &["commit", "-qm", "touch surface"]);

        let mol = sample_mol("task-20260610-rrrr", "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        let err = gate_with_config(&store, &mol.id, &cfg, Utc::now()).unwrap_err();
        let MindguardError::Refused(msg) = err else {
            panic!("expected Refused");
        };
        assert!(
            msg.contains("cs nucleate verify-surface --var target=task-20260610-rrrr"),
            "remedy must use the positional formula syntax: {msg}"
        );
        assert!(
            !msg.contains("--formula"),
            "remedy must not prescribe the invalid --formula flag: {msg}"
        );
    }

    /// A `BlockedBy` whose `feat/<dep>` branch does not exist (blocker
    /// merged + branch deleted) is filtered out, so the gate falls back
    /// to `origin/main` rather than diffing against a missing ref.
    #[test]
    fn fork_bases_skips_deleted_blocker_branch() {
        let (tmp, store) = make_store_in_repo();
        let mut mol = sample_mol("task-20260531-dep", "task-work", MoleculeStatus::Running);
        mol.typed_links
            .push(cosmon_core::interaction::MoleculeLink::BlockedBy {
                source: MoleculeId::new("task-20260531-gone").unwrap(),
            });
        store.save_molecule(&mol.id, &mol).unwrap();
        // feat/task-20260531-gone was never created.
        let bases = molecule_fork_bases(&store, &mol.id, tmp.path());
        assert!(
            bases.is_empty(),
            "non-existent blocker branch must be skipped: {bases:?}"
        );
    }

    // --- cosmon-ward D9 fixes (automata, 2026-06-10) -------------------

    /// Pathology 1 — a `verify-surface` molecule must be self-exempt from
    /// the gate. Its witness pass IS the terminal observation; gating it
    /// on a sibling verify-surface is infinite regress (no verify-surface
    /// could ever land GREEN while the surface reads touched). The
    /// completing molecule here authored a real surface file and has no
    /// GREEN sibling — yet it must pass *because* it is verify-surface.
    #[test]
    fn gate_exempts_verify_surface_molecule_itself() {
        let (tmp, store) = make_store_in_repo();
        let root = tmp.path();
        // Touch a surface so the gate would otherwise fire.
        git(root, &["checkout", "-q", "-b", "feat/verify-20260610-df54"]);
        std::fs::write(root.join("witness.html"), "<html/>").unwrap();
        git(root, &["add", "witness.html"]);
        git(root, &["commit", "-qm", "render surface for witness"]);

        let mol = sample_mol(
            "verify-20260610-df54",
            "verify-surface",
            MoleculeStatus::Running,
        );
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &mol.id, &cfg, Utc::now())
            .expect("a verify-surface molecule must be self-exempt from its own gate");
    }

    /// Pathology 2 — when `origin/main` is stale (no push by protocol) and
    /// local `main` has advanced with an inherited surface file, the gate
    /// must diff against the *fresher* local `main`, not the stale
    /// `origin/main`. A root molecule sitting on `main` that authored
    /// nothing must therefore pass, even though `origin/main...HEAD` would
    /// over-capture the inherited file.
    #[test]
    fn gate_prefers_fresh_local_main_over_stale_origin_main() {
        let (tmp, store) = make_store_in_repo();
        let root = tmp.path();
        // Freeze a stale origin/main at the init commit.
        let c0 = git_rev_parse(root, "HEAD");
        git(root, &["update-ref", "refs/remotes/origin/main", &c0]);
        // Local main advances with an inherited surface file (a merge the
        // worker protocol forbids pushing).
        std::fs::write(root.join("inherited.html"), "<html/>").unwrap();
        git(root, &["add", "inherited.html"]);
        git(
            root,
            &["commit", "-qm", "merge: inherited surface from sibling"],
        );

        // Sanity: freshest fallback must put local main first.
        assert_eq!(
            freshest_fallback_bases(root),
            vec!["main".to_owned(), "origin/main".to_owned()],
            "local main is ahead of stale origin/main → main must be freshest"
        );

        // HEAD == main; the molecule is a root that authored nothing.
        let mol = sample_mol("task-20260610-aaaa", "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &mol.id, &cfg, Utc::now())
            .expect("stale origin/main must not over-capture inherited surface");
    }

    /// The freshest ordering flips when `origin/main` is genuinely ahead
    /// (e.g. just after a fetch): the more-recent remote ref is tried
    /// first so the fallback stays as tight as possible.
    #[test]
    fn freshest_fallback_prefers_origin_main_when_it_is_ahead() {
        let (tmp, _store) = make_store_in_repo();
        let root = tmp.path();
        // origin/main advances one commit beyond local main.
        std::fs::write(root.join("ahead.txt"), "x").unwrap();
        git(root, &["add", "ahead.txt"]);
        git(root, &["commit", "-qm", "remote is ahead"]);
        let ahead = git_rev_parse(root, "HEAD");
        git(root, &["update-ref", "refs/remotes/origin/main", &ahead]);
        // Move local main back to the parent so origin/main is strictly ahead.
        git(root, &["reset", "-q", "--hard", "HEAD~1"]);

        assert_eq!(
            freshest_fallback_bases(root),
            vec!["origin/main".to_owned(), "main".to_owned()],
            "origin/main ahead of main → origin/main must be freshest"
        );
    }

    // --- deletion-only diffs (false-alarm fix, 2026-06-23) -------------

    /// Unit level: `surface_touched` must NOT fire when the molecule's
    /// diff is a *pure deletion* of a surface file. The render step has
    /// nothing to paint when the surface is gone, so demanding a witness
    /// is structurally unsatisfiable. Mirrors the task-20260622-eeb9
    /// scope-trim misfire (deleting test-fixture `.html` files).
    #[test]
    fn surface_touched_ignores_deletion_only_diff() {
        let (tmp, _store) = make_store_in_repo();
        let root = tmp.path();
        // Seed a surface file on main, then a branch that deletes it.
        std::fs::write(root.join("page.html"), "<html/>").unwrap();
        git(root, &["add", "page.html"]);
        git(root, &["commit", "-qm", "seed surface file"]);
        git(root, &["checkout", "-q", "-b", "feat/delete-surface"]);
        git(root, &["rm", "-q", "page.html"]);
        git(root, &["commit", "-qm", "remove surface fixture"]);

        let patterns = SurfaceConfig::default().paths;
        assert!(
            !surface_touched(root, &patterns, &[], "HEAD", root).unwrap(),
            "a pure deletion of a surface file must not read as a touch"
        );
    }

    /// End-to-end: a molecule whose branch only *removes* surface files
    /// seals without a `verify-surface` sibling — the false alarm this
    /// fix eliminates.
    #[test]
    fn gate_passes_for_deletion_only_surface_diff() {
        let (tmp, store) = make_store_in_repo();
        let root = tmp.path();
        let mol_id = "task-20260623-dele";

        // A surface fixture tracked on main.
        std::fs::write(root.join("fixture.html"), "<html/>").unwrap();
        git(root, &["add", "fixture.html"]);
        git(root, &["commit", "-qm", "seed surface fixture"]);

        // The molecule's branch deletes it and nothing else.
        git(root, &["checkout", "-q", "-b", &format!("feat/{mol_id}")]);
        git(root, &["rm", "-q", "fixture.html"]);
        git(root, &["commit", "-qm", "scope trim: drop fixture"]);

        let mol = sample_mol(mol_id, "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        gate_with_config(&store, &mol.id, &cfg, Utc::now())
            .expect("deletion-only surface diff must not activate the gate");
    }

    /// No false negative: a diff that deletes one surface file *and*
    /// modifies another still fires. The `--diff-filter=d` exclusion
    /// drops only the delete status, never a real add/modify.
    #[test]
    fn gate_still_fires_when_deletion_accompanies_modification() {
        let (tmp, store) = make_store_in_repo();
        let root = tmp.path();
        let mol_id = "task-20260623-mixx";

        std::fs::write(root.join("old.html"), "<html/>").unwrap();
        std::fs::write(root.join("kept.css"), "body{}").unwrap();
        git(root, &["add", "old.html", "kept.css"]);
        git(root, &["commit", "-qm", "seed two surface files"]);

        git(root, &["checkout", "-q", "-b", &format!("feat/{mol_id}")]);
        git(root, &["rm", "-q", "old.html"]);
        std::fs::write(root.join("kept.css"), "body{color:red}").unwrap();
        git(root, &["add", "kept.css"]);
        git(root, &["commit", "-qm", "drop one, edit the other"]);

        let mol = sample_mol(mol_id, "task-work", MoleculeStatus::Running);
        store.save_molecule(&mol.id, &mol).unwrap();

        let cfg = SurfaceConfig::default();
        let err = gate_with_config(&store, &mol.id, &cfg, Utc::now()).unwrap_err();
        assert!(
            matches!(err, MindguardError::Refused(_)),
            "a real modification alongside a deletion must still gate: {err}"
        );
    }
}
