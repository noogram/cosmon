// SPDX-License-Identifier: AGPL-3.0-only

//! Unified state directory resolution.
//!
//! Both the CLI (`cs`) and the MCP server must agree on where state lives.
//! This module provides a single [`resolve_state_dir`] function that both
//! consumers call, eliminating the split-brain problem where CLI reads
//! `~/cosmon/state` while MCP writes `.cosmon/`.
//!
//! Resolution precedence (first match wins):
//!
//! 1. **Explicit override** — `--config` flag or direct parameter
//! 2. **`COSMON_STATE_DIR`** environment variable
//! 3. **Walk-up discovery** — search from CWD upward for a `.cosmon/` directory
//! 4. **Global fallback** — `$HOME/cosmon/state`

use std::path::{Path, PathBuf};

/// The marker directory name that identifies a cosmon project root.
pub const COSMON_DIR_NAME: &str = ".cosmon";

/// The canonical set of `COSMON_*` environment variables that **this
/// module's resolvers consult** to redirect a `cs` process's notion of
/// where state, formulas, config, and the cluster file live.
///
/// This is the single source of truth for "which env vars steer
/// `cosmon-filestore` resolution". Each entry corresponds to an
/// `std::env::var(...)` read inside this module:
///
/// - `COSMON_STATE_DIR` — [`resolve_state_dir`] / [`resolve_state_dir_from`]
/// - `COSMON_FORMULAS_DIR` — [`resolve_formulas_dir`] / [`resolve_formulas_dir_from`]
/// - `COSMON_CONFIG` — [`resolve_config_path`] / [`resolve_config_path_from`]
/// - `COSMON_CLUSTER_CONFIG` — [`resolve_cluster_config_path`]
///
/// **Why this is `pub`.** Any boundary that spawns a child `cs` and must
/// *not* let an ambient resolution var leak across (e.g. the RPP
/// subprocess envelope's strip half) should import this slice as a
/// **view** rather than re-listing the names by hand. A hand-maintained
/// mirror silently drifts the moment a new resolver is added here; a
/// `use`-imported view re-strips the new var for free at the next build —
/// the compiler becomes the synchronizer. If you add a `COSMON_*` read to
/// this module, add its name here and every importer is correct again
/// without edits.
pub const RESOLUTION_VARS: &[&str] = &[
    "COSMON_STATE_DIR",
    "COSMON_FORMULAS_DIR",
    "COSMON_CONFIG",
    "COSMON_CLUSTER_CONFIG",
];

/// How [`resolve_state_dir_with_origin`] arrived at its result.
///
/// Distinguishes a **project-scoped** resolution (walk-up found a galaxy's
/// `.cosmon/config.toml`) from the silent **home-global** fallback. Callers
/// that *write* state — `cs nucleate` in particular — use this to warn the
/// operator when a molecule would be born into the invisible
/// `~/.cosmon/state` fleet rather than into a galaxy on disk.
///
/// The home-global fallback is intentional (it hosts scheduler / patrol /
/// daemon-supervisor state, ADR-069), but creating a *molecule* there from
/// the wrong cwd is a silent-success orphan trap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateDirOrigin {
    /// Caller passed an explicit path (`--config` / `--store-dir`).
    Explicit,
    /// Resolved from the `COSMON_STATE_DIR` environment variable.
    Env,
    /// Walk-up discovery found a `.cosmon/config.toml`-bearing ancestor.
    Project,
    /// No project found in cwd or ancestors — fell back to
    /// `$HOME/.cosmon/state`, a host-global state dir invisible to every
    /// galaxy.
    GlobalFallback,
}

/// Resolve the Cosmon state directory.
///
/// Precedence:
/// 1. `explicit` — if `Some`, returned directly (CLI `--config` flag)
/// 2. `COSMON_STATE_DIR` env var
/// 3. Walk up from CWD looking for `.cosmon/` (like `git` finds `.git/`)
/// 4. Fallback to `$HOME/cosmon/state`
///
/// When a `.cosmon/` directory is found via walk-up (case 3), the function
/// returns `.cosmon/state/` — the subdirectory where [`crate::FileStore`]
/// expects `fleet.json` and `ops/molecules/`. The outer `.cosmon/` directory
/// also contains `formulas/` and `molecules/` (git-tracked declarations).
///
/// This is a convenience wrapper over [`resolve_state_dir_with_origin`] that
/// discards the [`StateDirOrigin`]. Callers that need to react to the
/// home-global fallback (e.g. to warn an operator before writing) should call
/// the origin-returning variant instead.
#[must_use]
pub fn resolve_state_dir(explicit: Option<&Path>) -> PathBuf {
    resolve_state_dir_with_origin(explicit).0
}

/// Resolve the Cosmon state directory, also reporting **how** it was resolved.
///
/// Same precedence as [`resolve_state_dir`], but returns the resolved path
/// paired with a [`StateDirOrigin`] so the caller can tell a project-scoped
/// hit apart from the silent home-global fallback. The classifying behaviour
/// is identical to [`resolve_state_dir`] — only the extra origin tag is new.
#[must_use]
pub fn resolve_state_dir_with_origin(explicit: Option<&Path>) -> (PathBuf, StateDirOrigin) {
    // 1. Explicit override wins.
    if let Some(path) = explicit {
        return (path.to_path_buf(), StateDirOrigin::Explicit);
    }

    // 2. Environment variable.
    if let Ok(dir) = std::env::var("COSMON_STATE_DIR") {
        return (PathBuf::from(dir), StateDirOrigin::Env);
    }

    // 3. Walk-up discovery — returns .cosmon/state/ for FileStore compatibility.
    if let Some(found) = walk_up_find_cosmon_dir() {
        return (found.join("state"), StateDirOrigin::Project);
    }

    // 4. Global fallback — no galaxy in cwd or ancestors.
    (global_fallback(), StateDirOrigin::GlobalFallback)
}

/// Walk up from the current working directory looking for a `.cosmon/` directory.
///
/// Returns `Some(path_to_.cosmon)` if found, `None` otherwise. Thin wrapper
/// over [`walk_up_find_cosmon_dir_from`] that uses the process CWD as the
/// starting point.
fn walk_up_find_cosmon_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    walk_up_find_cosmon_dir_from(&cwd)
}

/// Walk up from `start` looking for a cosmon **project root** — a
/// directory containing `.cosmon/config.toml` (as a regular file).
///
/// Returns `Some(path_to_.cosmon)` if found, `None` otherwise.
///
/// A `.cosmon/` directory **without** `config.toml` is a user-level
/// state host (scheduler, patrol supervisor, recovery logs) and does
/// not participate in project discovery — the walk continues past it.
/// See [ADR-069](../../../docs/adr/069-cosmon-project-vs-user-root.md).
///
/// When running inside a git worktree, the worktree may contain a `.cosmon/`
/// directory (checked out from the branch) that lacks the `state/` subdirectory
/// where the real fleet state lives. This function detects git worktrees by
/// checking whether `.git` is a file (not a directory) — the standard git
/// worktree indicator — and follows it back to the main repo's `.cosmon/`.
///
/// Factored out so callers that know their working directory (e.g. an MCP
/// server serving requests from multiple client CWDs) can resolve without
/// relying on the process CWD.
#[must_use]
pub fn walk_up_find_cosmon_dir_from(start: &Path) -> Option<PathBuf> {
    let mut dir = start;

    loop {
        let candidate = dir.join(COSMON_DIR_NAME);
        if candidate.is_dir() && candidate.join("config.toml").is_file() {
            // Check if this is a git worktree: .git is a file, not a directory.
            // In a worktree, .git contains "gitdir: /path/to/main/.git/worktrees/<name>".
            // The worktree's .cosmon/ is a git-tracked checkout copy and typically
            // lacks state/. Redirect to the main repo's .cosmon/ instead.
            if let Some(main_cosmon) = resolve_worktree_main_cosmon(dir) {
                return Some(main_cosmon);
            }
            // Canonicalize to match the worktree redirect path (which is
            // canonicalized via gitdir resolution). Without this, macOS
            // `/var/` vs `/private/var/` mismatches cause silent `==`
            // failures downstream.
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
        dir = dir.parent()?;
    }
}

/// If `project_dir` is a git worktree, find the main repo's `.cosmon/` directory.
///
/// Git worktrees have a `.git` **file** (not directory) containing:
/// ```text
/// gitdir: /path/to/main-repo/.git/worktrees/<worktree-name>
/// ```
///
/// We follow this pointer back to the main repo root and check for `.cosmon/`.
/// Returns `None` if this is not a worktree or the main repo has no `.cosmon/`.
fn resolve_worktree_main_cosmon(project_dir: &Path) -> Option<PathBuf> {
    let git_path = project_dir.join(".git");

    // A real git repo has .git as a directory; a worktree has it as a file.
    if git_path.is_dir() || !git_path.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(&git_path).ok()?;
    let gitdir = content.strip_prefix("gitdir: ")?.trim();
    let gitdir_path = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        project_dir.join(gitdir)
    };

    // gitdir points to e.g. /repo/.git/worktrees/<name>.
    // The main repo root is the parent of .git/, so .git/../ = repo root.
    // Canonicalize to resolve any relative segments (../../) in the path.
    let gitdir_canonical = gitdir_path.canonicalize().ok()?;
    let git_main_dir = gitdir_canonical
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == ".git"))?;
    let main_repo_root = git_main_dir.parent()?;
    let main_cosmon = main_repo_root.join(COSMON_DIR_NAME);

    if main_cosmon.is_dir() {
        Some(main_cosmon)
    } else {
        None
    }
}

/// Resolve the Cosmon formulas directory.
///
/// Precedence:
/// 1. `explicit` — if `Some`, returned directly (CLI `--formulas-dir` flag)
/// 2. `COSMON_FORMULAS_DIR` env var
/// 3. Walk up from CWD looking for `.cosmon/` (like `git` finds `.git/`)
/// 4. Fallback to `$HOME/.cosmon/formulas`
///
/// When a `.cosmon/` directory is found via walk-up (case 3), the function
/// returns `.cosmon/formulas/` — the subdirectory where formula TOML files live.
#[must_use]
pub fn resolve_formulas_dir(explicit: Option<&Path>) -> PathBuf {
    // 1. Explicit override wins.
    if let Some(path) = explicit {
        return path.to_path_buf();
    }

    // 2. Environment variable.
    if let Ok(dir) = std::env::var("COSMON_FORMULAS_DIR") {
        return PathBuf::from(dir);
    }

    // 3. Walk-up discovery — returns .cosmon/formulas/.
    if let Some(found) = walk_up_find_cosmon_dir() {
        return found.join("formulas");
    }

    // 4. Global fallback.
    global_formulas_fallback()
}

/// Resolve the Cosmon state directory starting walk-up from `start`.
///
/// Same precedence as [`resolve_state_dir`], but the walk-up step (case 3)
/// uses `start` as its origin instead of the process CWD. This is what the
/// MCP server needs when a client provides an explicit `cwd`: the caller's
/// working directory should determine which project's state is mutated,
/// not the long-lived server's own CWD.
///
/// The `COSMON_STATE_DIR` environment variable still wins over walk-up so
/// that explicit ops overrides remain authoritative even when a client
/// supplies a `cwd` — callers that have an explicit path should use
/// `resolve_state_dir(Some(path))` directly instead of this helper.
#[must_use]
pub fn resolve_state_dir_from(start: &Path) -> PathBuf {
    // 1. Environment variable still wins over walk-up.
    if let Ok(dir) = std::env::var("COSMON_STATE_DIR") {
        return PathBuf::from(dir);
    }

    // 2. Walk-up discovery from the caller-supplied start dir.
    if let Some(found) = walk_up_find_cosmon_dir_from(start) {
        return found.join("state");
    }

    // 3. Global fallback.
    global_fallback()
}

/// Resolve the Cosmon formulas directory starting walk-up from `start`.
///
/// Same precedence as [`resolve_formulas_dir`], but the walk-up step (case 3)
/// uses `start` as its origin instead of the process CWD. This is what the
/// MCP server needs when a client provides an explicit `cwd`: the caller's
/// working directory should determine which project's formulas are loaded,
/// not the long-lived server's own CWD.
#[must_use]
pub fn resolve_formulas_dir_from(start: &Path) -> PathBuf {
    // 1. Environment variable still wins over walk-up (explicit override
    //    equivalent is out of scope — callers that have an explicit path
    //    should use `resolve_formulas_dir(Some(path))` directly).
    if let Ok(dir) = std::env::var("COSMON_FORMULAS_DIR") {
        return PathBuf::from(dir);
    }

    // 2. Walk-up discovery from the caller-supplied start dir.
    if let Some(found) = walk_up_find_cosmon_dir_from(start) {
        return found.join("formulas");
    }

    // 3. Global fallback.
    global_formulas_fallback()
}

/// Resolve the path to the cluster config file (`cluster.toml`).
///
/// This is a **machine-level** config (one file per host in the cluster),
/// distinct from the per-project `.cosmon/config.toml`. See
/// [ADR-066](../../../docs/adr/066-surfaces-cluster-config.md).
///
/// Precedence:
/// 1. `explicit` — if `Some`, returned directly (CLI `--cluster-config`)
/// 2. `COSMON_CLUSTER_CONFIG` env var
/// 3. `$HOME/.config/cosmon/cluster.toml` (XDG-idiomatic)
#[must_use]
pub fn resolve_cluster_config_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Ok(path) = std::env::var("COSMON_CLUSTER_CONFIG") {
        return PathBuf::from(path);
    }
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".config")
        .join("cosmon")
        .join("cluster.toml")
}

/// Resolve the Cosmon config file path (`.cosmon/config.toml`).
///
/// Precedence:
/// 1. `explicit` — if `Some`, returned directly
/// 2. `COSMON_CONFIG` env var
/// 3. Walk up from CWD looking for `.cosmon/` (like `git` finds `.git/`)
/// 4. Fallback to `$HOME/.cosmon/config.toml`
#[must_use]
pub fn resolve_config_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Ok(path) = std::env::var("COSMON_CONFIG") {
        return PathBuf::from(path);
    }
    if let Some(found) = walk_up_find_cosmon_dir() {
        return found.join("config.toml");
    }
    global_config_fallback()
}

/// Resolve the Cosmon config file path starting walk-up from `start`.
#[must_use]
pub fn resolve_config_path_from(start: &Path) -> PathBuf {
    if let Ok(path) = std::env::var("COSMON_CONFIG") {
        return PathBuf::from(path);
    }
    if let Some(found) = walk_up_find_cosmon_dir_from(start) {
        return found.join("config.toml");
    }
    global_config_fallback()
}

/// Global fallback: `$HOME/.cosmon/config.toml`.
fn global_config_fallback() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".cosmon/config.toml")
}

/// Global fallback: `$HOME/.cosmon/formulas`.
fn global_formulas_fallback() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".cosmon/formulas")
}

/// Global fallback: `$HOME/.cosmon/state`.
///
/// Uses the dotdir convention (`~/.cosmon`) consistent with other Unix tools
/// (`~/.git`, `~/.cargo`, `~/.config`).
fn global_fallback() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".cosmon/state")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_explicit_override_wins() {
        let path = PathBuf::from("/tmp/my-cosmon-state");
        let result = resolve_state_dir(Some(&path));
        assert_eq!(result, path);
    }

    #[test]
    fn test_explicit_origin_is_tagged() {
        let path = PathBuf::from("/tmp/my-cosmon-state");
        let (resolved, origin) = resolve_state_dir_with_origin(Some(&path));
        assert_eq!(resolved, path);
        assert_eq!(origin, StateDirOrigin::Explicit);
    }

    #[test]
    fn test_walk_up_finds_cosmon_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let cosmon_dir = project.join(".cosmon");
        let subdir = project.join("src/deep/nested");

        fs::create_dir_all(&cosmon_dir).unwrap();
        fs::create_dir_all(&subdir).unwrap();

        // Simulate walk-up from a nested directory.
        let mut dir = subdir.as_path();
        let mut found = None;
        loop {
            let candidate = dir.join(COSMON_DIR_NAME);
            if candidate.is_dir() {
                found = Some(candidate);
                break;
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }

        assert_eq!(found, Some(cosmon_dir));
    }

    /// The walk-up primitive underneath `resolve_state_dir_from` must pick
    /// up a `.cosmon/` directory that lives at or above the supplied
    /// `start`. This is how the MCP server resolves the right project's
    /// state when a client supplies its own cwd instead of relying on the
    /// long-lived server's process CWD.
    ///
    /// We test the private walk-up helper directly rather than the public
    /// `resolve_state_dir_from`, because the public function consults
    /// `COSMON_STATE_DIR` first and this crate sets
    /// `#![forbid(unsafe_code)]`, which means tests cannot safely manipulate
    /// the environment in parallel with other tests.
    #[test]
    fn test_walk_up_find_cosmon_dir_from_returns_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        let cosmon_dir = project.join(".cosmon");
        let nested = project.join("src/a/b/c");
        fs::create_dir_all(&cosmon_dir).unwrap();
        fs::create_dir_all(&nested).unwrap();
        // ADR-069: a real project root is a `.cosmon/` carrying
        // `config.toml`. Seed the marker so walk-up recognises this
        // ancestor as a project.
        fs::write(cosmon_dir.join("config.toml"), "# seeded by test\n").unwrap();

        let found = walk_up_find_cosmon_dir_from(&nested)
            .expect("should find .cosmon/ by walking up from nested");
        // The result is canonicalized (e.g. /private/var on macOS).
        let expected = cosmon_dir.canonicalize().unwrap();
        assert_eq!(found, expected);

        // And `resolve_state_dir_from` composes walk-up with the `state`
        // subdirectory join — guard the composition unless the caller's
        // environment pre-sets COSMON_STATE_DIR.
        if std::env::var_os("COSMON_STATE_DIR").is_none() {
            assert_eq!(resolve_state_dir_from(&nested), expected.join("state"));
        }
    }

    /// Filestore parity test for ADR-069: a `.cosmon/` directory
    /// **without** `config.toml` is a user-level state host, not a
    /// cosmon project root. The walk-up must skip it and continue,
    /// returning `None` when no config-bearing ancestor exists above.
    ///
    /// This is the filestore-side counterpart to
    /// `init_allows_child_of_configless_ancestor_cosmon` in
    /// `cosmon-cli`; both call sites share the predicate and the
    /// ADR forbids drift between them.
    #[test]
    fn walk_up_skips_configless_cosmon() {
        let tmp = tempfile::tempdir().unwrap();
        // A config-less `.cosmon/` directly above the walk-up origin —
        // stands in for `~/.cosmon/` on a machine running patrols or
        // the daemon supervisor.
        let host = tmp.path().join("user-host");
        let host_cosmon = host.join(".cosmon");
        fs::create_dir_all(&host_cosmon).unwrap();
        assert!(
            !host_cosmon.join("config.toml").exists(),
            "precondition: user-level host must lack config.toml"
        );

        // A subdirectory deep inside the host — where a nested galaxy
        // would sit, or where a `cs` command would be invoked from.
        let nested = host.join("galaxies/annex/src/a/b");
        fs::create_dir_all(&nested).unwrap();

        // Walk-up must refuse to return the config-less ancestor and
        // continue to the filesystem root, where no project exists.
        let found = walk_up_find_cosmon_dir_from(&nested);
        assert!(
            found.is_none(),
            "walk-up must skip a config-less `.cosmon/`, got: {found:?}"
        );

        // Seed the marker → walk-up now finds the ancestor.
        fs::write(host_cosmon.join("config.toml"), "# seeded\n").unwrap();
        let found = walk_up_find_cosmon_dir_from(&nested)
            .expect("once config.toml exists, walk-up must find the ancestor");
        assert_eq!(found, host_cosmon.canonicalize().unwrap());
    }

    #[test]
    fn test_global_fallback_contains_cosmon() {
        let fb = global_fallback();
        assert!(
            fb.to_string_lossy().contains(".cosmon/state"),
            "fallback should contain .cosmon/state, got: {fb:?}"
        );
    }

    #[test]
    fn test_formulas_explicit_override_wins() {
        let path = PathBuf::from("/tmp/my-formulas");
        let result = resolve_formulas_dir(Some(&path));
        assert_eq!(result, path);
    }

    #[test]
    fn test_formulas_global_fallback() {
        let fb = global_formulas_fallback();
        assert!(
            fb.to_string_lossy().contains(".cosmon/formulas"),
            "fallback should contain .cosmon/formulas, got: {fb:?}"
        );
    }

    #[test]
    fn test_worktree_redirects_to_main_repo_cosmon() {
        let tmp = tempfile::tempdir().unwrap();

        // Create main repo with .git dir and .cosmon/
        let main_repo = tmp.path().join("main-repo");
        let main_git = main_repo.join(".git");
        let main_cosmon = main_repo.join(".cosmon");
        fs::create_dir_all(&main_git).unwrap();
        fs::create_dir_all(&main_cosmon).unwrap();

        // Create worktree with .git file pointing to main
        let worktree = tmp.path().join("worktree");
        let wt_cosmon = worktree.join(".cosmon");
        fs::create_dir_all(&wt_cosmon).unwrap();

        let gitdir_target = main_git.join("worktrees/my-branch");
        fs::create_dir_all(&gitdir_target).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}", gitdir_target.display()),
        )
        .unwrap();

        // resolve_worktree_main_cosmon should redirect to main repo.
        // Canonicalize expected path to handle macOS /var -> /private/var symlink.
        let result = resolve_worktree_main_cosmon(&worktree);
        assert_eq!(result, Some(main_cosmon.canonicalize().unwrap()));
    }

    #[test]
    fn test_non_worktree_returns_none() {
        let tmp = tempfile::tempdir().unwrap();

        // Normal repo with .git directory (not a worktree).
        let repo = tmp.path().join("repo");
        let git_dir = repo.join(".git");
        fs::create_dir_all(&git_dir).unwrap();

        let result = resolve_worktree_main_cosmon(&repo);
        assert_eq!(result, None);
    }

    #[test]
    fn test_worktree_without_main_cosmon_returns_none() {
        let tmp = tempfile::tempdir().unwrap();

        // Main repo WITHOUT .cosmon/
        let main_repo = tmp.path().join("main-repo");
        let main_git = main_repo.join(".git");
        fs::create_dir_all(&main_git).unwrap();

        // Worktree pointing to main
        let worktree = tmp.path().join("worktree");
        fs::create_dir_all(&worktree).unwrap();

        let gitdir_target = main_git.join("worktrees/my-branch");
        fs::create_dir_all(&gitdir_target).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}", gitdir_target.display()),
        )
        .unwrap();

        // No .cosmon in main repo, so should return None.
        let result = resolve_worktree_main_cosmon(&worktree);
        assert_eq!(result, None);
    }

    #[test]
    fn test_worktree_relative_gitdir() {
        let tmp = tempfile::tempdir().unwrap();

        // Create main repo at tmp/main-repo
        let main_repo = tmp.path().join("main-repo");
        let main_git = main_repo.join(".git");
        let main_cosmon = main_repo.join(".cosmon");
        fs::create_dir_all(&main_git).unwrap();
        fs::create_dir_all(&main_cosmon).unwrap();

        // Create worktree at tmp/main-repo/.worktrees/branch
        let worktree = main_repo.join(".worktrees/branch");
        let wt_cosmon = worktree.join(".cosmon");
        fs::create_dir_all(&wt_cosmon).unwrap();

        let gitdir_target = main_git.join("worktrees/branch");
        fs::create_dir_all(&gitdir_target).unwrap();

        // Use relative gitdir path (relative to worktree dir).
        fs::write(worktree.join(".git"), "gitdir: ../../.git/worktrees/branch").unwrap();

        let result = resolve_worktree_main_cosmon(&worktree);
        assert_eq!(result, Some(main_cosmon.canonicalize().unwrap()));
    }
}
