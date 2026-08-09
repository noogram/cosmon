// SPDX-License-Identifier: AGPL-3.0-only

//! Resolution of a galaxy's **target repository** — the single place that
//! answers "which git repository does this galaxy's work land in?".
//!
//! # Why this module exists
//!
//! A galaxy and its repository used to be bound by nothing but coincidence.
//! Two *independent* resolutions ran from the same current directory and
//! happened to agree:
//!
//! * the **state** — [`cosmon_filestore::resolve_state_dir`] walks up from the
//!   cwd looking for `.cosmon/`;
//! * the **repository** — `git rev-parse --show-toplevel`, from the cwd, so
//!   the nearest `.git` wins.
//!
//! Nothing declared that these two must answer the same tree, and in the
//! nested topology — an orchestration galaxy holding a third party's
//! deliverable repository in a subdirectory — they deliberately do *not*.
//! That arrangement already worked, by accident, undeclared.
//!
//! The failure mode is the one an undeclared coupling always has: it is
//! silent. A `cs tackle` fired from the wrong directory branches the wrong
//! repository. There is no error and no warning — the work simply lands
//! somewhere else and is discovered later, which is the same shape as a guard
//! rail that never speaks.
//!
//! [`resolve()`](crate::target_repo::resolve) makes the binding a
//! **declaration**. The optional
//! [`target_repo`](cosmon_core::config::ProjectSection::target_repo) key in
//! `.cosmon/config.toml` names the repository; when it is present the repo is
//! resolved from *there* and a non-repository is refused out loud. When it is
//! absent — which is every galaxy that has not opted in — the answer is the
//! cwd-derived one, byte for byte.
//!
//! ```no_run
//! use std::path::Path;
//! use cosmon_cli::target_repo;
//!
//! // `[project] target_repo = "deliverable"` in a galaxy rooted at /gal
//! // points at /gal/deliverable, whatever directory `cs` was fired from.
//! let probe = target_repo::declared_path(Path::new("/gal"), "deliverable").unwrap();
//! assert_eq!(probe, Path::new("/gal/deliverable"));
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

/// Which resolution answered "where is the repository?".
///
/// Carried alongside the path so a diagnostic can say *how* the repository was
/// chosen. "The repository is `/x`" is true and useless when the operator's
/// question is whether their declaration took effect at all: an accidental
/// cwd hit and an honoured `target_repo` produce the same string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoSource {
    /// The galaxy's `[project] target_repo` declaration.
    Declared,
    /// `git rev-parse --show-toplevel` from the current directory — the
    /// pre-existing, undeclared behaviour.
    Cwd,
}

impl RepoSource {
    /// A short operator-facing phrase naming this source, for diagnostics.
    /// Written to slot after "resolved from".
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Declared => "`[project] target_repo` in .cosmon/config.toml",
            Self::Cwd => "the working directory — no `[project] target_repo` is declared",
        }
    }
}

/// A resolved repository root together with the resolution that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRepo {
    /// The repository's top level, as `git rev-parse --show-toplevel` reports
    /// it.
    pub root: PathBuf,
    /// Which resolution won.
    pub source: RepoSource,
}

/// Resolve the git repository this galaxy's work belongs to.
///
/// Precedence:
///
/// 1. `[project] target_repo`, when the galaxy declares one — the repository
///    is probed at that path and a non-repository is a hard refusal, never a
///    silent fall-through to the cwd. Falling back would reintroduce exactly
///    the silence the declaration exists to remove.
/// 2. `git rev-parse --show-toplevel` from the current directory — the
///    pre-existing behaviour, unchanged for every galaxy that declares
///    nothing.
///
/// The config file is located the same way every other `cs` command locates
/// it ([`cosmon_filestore::resolve_config_path`]), so state and repository are
/// resolved from one walk-up rather than two unrelated ones. A config that is
/// missing or unparseable is treated as "no declaration": this function must
/// not be the thing that fails a galaxy whose config is broken for unrelated
/// reasons — the commands that need the config already say so themselves.
///
/// # Errors
///
/// Returns an error when the declared `target_repo` does not exist, is not a
/// git repository, or when no declaration exists and the current directory is
/// not inside a git repository.
pub fn resolve() -> anyhow::Result<PathBuf> {
    resolve_with_source().map(|r| r.root)
}

/// [`resolve()`](crate::target_repo::resolve), keeping the resolution that
/// produced the answer.
///
/// # Errors
///
/// Same as [`resolve()`](crate::target_repo::resolve).
pub fn resolve_with_source() -> anyhow::Result<ResolvedRepo> {
    let config_path = cosmon_filestore::resolve_config_path(None);
    resolve_from_config(&config_path)
}

/// [`resolve_with_source`] against an explicitly located `config.toml`.
///
/// Exposed for the callers that already hold a resolved config path (and for
/// tests, which must not depend on the process-wide current directory to
/// choose a galaxy).
///
/// # Errors
///
/// Same as [`resolve()`](crate::target_repo::resolve).
pub fn resolve_from_config(config_path: &Path) -> anyhow::Result<ResolvedRepo> {
    let declared = cosmon_filestore::load_project_config(config_path)
        .ok()
        .and_then(|cfg| cfg.project.target_repo)
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty());

    if let Some(declared) = declared {
        let galaxy_root = galaxy_root_of(config_path);
        let probe = declared_path(&galaxy_root, &declared)?;
        let root = git_toplevel(&probe).ok_or_else(|| {
            anyhow::anyhow!(
                "`[project] target_repo = \"{declared}\"` in {} does not name a git \
                 repository.\n\
                 Resolved to: {}\n\
                 Nothing was branched. Point the key at a git working tree, or remove it \
                 to fall back to the repository containing the current directory.",
                config_path.display(),
                probe.display(),
            )
        })?;
        return Ok(ResolvedRepo {
            root,
            source: RepoSource::Declared,
        });
    }

    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to read the current directory: {e}"))?;
    let root = git_toplevel(&cwd)
        .ok_or_else(|| anyhow::anyhow!("not in a git repository: {}", cwd.display()))?;
    let root = unnest_cosmon_worktree(&root).unwrap_or(root);
    Ok(ResolvedRepo {
        root,
        source: RepoSource::Cwd,
    })
}

/// The path a `target_repo` declaration points at, before any git probe.
///
/// Pure: no filesystem access, so the grammar is testable without standing up
/// a repository. Absolute declarations are taken as written; relative ones —
/// including the `"."` that the merged galaxy-is-the-repo case writes —
/// resolve against the **galaxy root**, never against the current directory.
/// Resolving against the cwd would make the declaration mean a different tree
/// depending on where `cs` was fired from, which is the exact property this
/// key exists to remove.
///
/// # Errors
///
/// Refuses a leading `~`. A shell expands the tilde and a config file does
/// not, so `~/gt` would otherwise be probed literally and reported as "not a
/// git repository" — a true message about the wrong path.
pub fn declared_path(galaxy_root: &Path, declared: &str) -> anyhow::Result<PathBuf> {
    if declared.starts_with('~') {
        anyhow::bail!(
            "`[project] target_repo = \"{declared}\"` starts with `~`, which is expanded by a \
             shell and not by a config file. Write the absolute path instead."
        );
    }
    let path = Path::new(declared);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        galaxy_root.join(path)
    })
}

/// The galaxy root a `config.toml` belongs to — the directory that *contains*
/// `.cosmon/`.
///
/// Handles both layouts cosmon accepts: the production `<root>/.cosmon/config.toml`
/// and the flat one used by fixtures, where `config.toml` sits directly beside
/// the state files. Falls back to the config file's own directory when the
/// path has no parent to speak of.
#[must_use]
pub fn galaxy_root_of(config_path: &Path) -> PathBuf {
    let dir = config_path.parent().unwrap_or(Path::new("."));
    if dir.file_name().is_some_and(|n| n == ".cosmon") {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

/// `git rev-parse --show-toplevel` run *inside* `dir`.
///
/// `None` when `dir` does not exist, git is unavailable, or the directory is
/// not inside a working tree. `-C` rather than a `current_dir` change so the
/// process-wide cwd is never mutated — several `cs` commands run concurrently
/// inside one process in tests.
fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let out = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!top.is_empty()).then(|| PathBuf::from(top))
}

/// The main working tree of the repository containing `dir`, when `dir` is a
/// **cosmon-managed linked worktree** — otherwise `None`.
///
/// # Why this exists
///
/// `git rev-parse --show-toplevel` answers "which working tree am I in?", and
/// inside `…/<galaxy>/.worktrees/task-A` that answer is the *worktree*, not
/// the galaxy. Every caller that then builds `<root>/.worktrees/<mol>` — which
/// is `cs tackle`'s only way of siting a worker — therefore nests the child
/// under its parent: `…/.worktrees/task-A/.worktrees/task-B`. That nesting is
/// not merely untidy. When `cs done task-A` removes the parent's worktree it
/// takes the child's directory with it, git keeps the now-dangling
/// registration, and the child's own `cs done` fails its branch delete with
/// `cannot delete branch 'feat/task-B' used by worktree` — leaving a ghost
/// branch and a ghost registration that only a manual `git worktree remove
/// --force` + `git worktree prune` clears. Five such nestings were observed on
/// 2026-08-08.
///
/// The redirection is deliberately narrow: it fires **only** when the linked
/// worktree sits directly under `<main>/.worktrees/`, i.e. when it is one
/// cosmon put there. A worktree an operator keeps elsewhere is their own
/// topology and is returned unchanged — this function must not quietly move
/// someone's work to a tree they did not name.
fn unnest_cosmon_worktree(top: &Path) -> Option<PathBuf> {
    let main = main_worktree_of(top)?;
    if main == top {
        return None;
    }
    (top.parent()?.file_name()? == ".worktrees" && top.parent()?.parent()? == main).then_some(main)
}

/// The main working tree of the repository `dir` belongs to.
///
/// `--git-common-dir` is the discriminator: it names the *shared* `.git`
/// directory, which is the main worktree's, whereas `--git-dir` names the
/// per-worktree one (`…/.git/worktrees/<name>`). Stripping the trailing
/// `.git` therefore yields the main working tree. Returns `None` for a bare
/// repository, or when git cannot answer.
fn main_worktree_of(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args([
            "-C",
            &dir.to_string_lossy(),
            "rev-parse",
            "--git-common-dir",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if raw.is_empty() {
        return None;
    }
    // Git reports this path relative to the directory it ran in (`.git` from a
    // main worktree) or absolute (from a linked one); both are accepted.
    let common = {
        let p = PathBuf::from(&raw);
        if p.is_absolute() {
            p
        } else {
            dir.join(p)
        }
    };
    // A bare repository has no working tree to return.
    git_toplevel(common.parent()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A relative declaration is anchored on the galaxy root, not the cwd.
    #[test]
    fn relative_declaration_anchors_on_galaxy_root() {
        let p = declared_path(Path::new("/gal"), "deliverable").unwrap();
        assert_eq!(p, Path::new("/gal/deliverable"));
    }

    /// `"."` is the merged case the v2 normalisation wants everyone to write:
    /// the galaxy *is* the repository, said out loud.
    #[test]
    fn dot_declaration_is_the_galaxy_root() {
        let p = declared_path(Path::new("/gal"), ".").unwrap();
        assert_eq!(p, Path::new("/gal/."));
    }

    /// An absolute declaration is taken as written.
    #[test]
    fn absolute_declaration_passes_through() {
        let p = declared_path(Path::new("/gal"), "/elsewhere/repo").unwrap();
        assert_eq!(p, Path::new("/elsewhere/repo"));
    }

    /// A tilde is refused with a message naming the reason, rather than
    /// probed literally and reported as a missing repository.
    #[test]
    fn tilde_declaration_is_refused_by_name() {
        let err = declared_path(Path::new("/gal"), "~/gt").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains('~'), "message must name the tilde: {msg}");
        assert!(
            msg.contains("absolute path"),
            "message must say what to do instead: {msg}"
        );
    }

    /// Production layout: `<root>/.cosmon/config.toml` belongs to `<root>`.
    #[test]
    fn galaxy_root_strips_the_dot_cosmon_directory() {
        let root = galaxy_root_of(Path::new("/gal/.cosmon/config.toml"));
        assert_eq!(root, Path::new("/gal"));
    }

    /// Flat fixture layout: the config's own directory is the root.
    #[test]
    fn galaxy_root_of_a_flat_layout_is_the_config_directory() {
        let root = galaxy_root_of(Path::new("/tmp/fixture/config.toml"));
        assert_eq!(root, Path::new("/tmp/fixture"));
    }

    /// No declaration ⇒ the pre-existing cwd resolution, unchanged. Run from
    /// this crate's source tree, which is inside the cosmon repository.
    #[test]
    fn absent_declaration_resolves_from_the_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        std::fs::write(&cfg, "[project]\nproject_id = \"test-0000\"\n").unwrap();

        let resolved = resolve_from_config(&cfg).unwrap();
        assert_eq!(resolved.source, RepoSource::Cwd);
        // The test process runs inside the cosmon checkout, so a toplevel
        // exists; what matters is that the *declaration* did not choose it.
        assert!(resolved.root.is_dir());
    }

    /// A linked worktree cosmon created under `<main>/.worktrees/` resolves to
    /// the main working tree — the property that stops `cs tackle` from
    /// nesting a child worktree inside its parent's (task-20260808-3033).
    #[test]
    fn a_cosmon_worktree_resolves_to_the_main_working_tree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.invalid"]);
        git(&["config", "user.name", "T"]);
        git(&["commit", "-q", "--allow-empty", "-m", "seed"]);
        let wt = root.join(".worktrees").join("task-a");
        git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            &wt.to_string_lossy(),
            "main",
        ]);

        assert_eq!(
            unnest_cosmon_worktree(&wt).as_deref(),
            Some(root.as_path()),
            "a worktree under .worktrees/ must resolve to the galaxy"
        );
        assert_eq!(
            unnest_cosmon_worktree(&root),
            None,
            "the main working tree is already the answer"
        );
    }

    /// A worktree an operator keeps outside `.worktrees/` is their topology,
    /// not cosmon's, and is returned untouched. Redirecting it would move
    /// someone's work to a tree they never named.
    #[test]
    fn a_worktree_outside_dot_worktrees_is_left_alone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.invalid"]);
        git(&["config", "user.name", "T"]);
        git(&["commit", "-q", "--allow-empty", "-m", "seed"]);
        let wt = base.join("elsewhere");
        git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            &wt.to_string_lossy(),
            "main",
        ]);

        assert_eq!(unnest_cosmon_worktree(&wt), None);
    }

    /// A declaration pointing at a non-repository refuses out loud, naming the
    /// key and the path it resolved to. It never falls back to the cwd — that
    /// fallback would be the silent misbranch the key exists to prevent.
    #[test]
    fn declared_non_repository_is_refused_not_silently_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cosmon")).unwrap();
        std::fs::create_dir_all(tmp.path().join("not-a-repo")).unwrap();
        let cfg = tmp.path().join(".cosmon/config.toml");
        std::fs::write(
            &cfg,
            "[project]\nproject_id = \"test-0000\"\ntarget_repo = \"not-a-repo\"\n",
        )
        .unwrap();

        let err = resolve_from_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("target_repo"), "must name the key: {err}");
        assert!(err.contains("not-a-repo"), "must name the path: {err}");
    }

    /// A declaration pointing at a real repository wins over the cwd, and says
    /// so through its source.
    #[test]
    fn declared_repository_wins_over_the_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("deliverable");
        std::fs::create_dir_all(&target).unwrap();
        let ok = Command::new("git")
            .args(["-C", &target.to_string_lossy(), "init", "--quiet"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return; // No git available; the other tests cover the grammar.
        }
        std::fs::create_dir_all(tmp.path().join(".cosmon")).unwrap();
        let cfg = tmp.path().join(".cosmon/config.toml");
        std::fs::write(
            &cfg,
            "[project]\nproject_id = \"test-0000\"\ntarget_repo = \"deliverable\"\n",
        )
        .unwrap();

        let resolved = resolve_from_config(&cfg).unwrap();
        assert_eq!(resolved.source, RepoSource::Declared);
        assert_eq!(
            resolved.root.canonicalize().unwrap(),
            target.canonicalize().unwrap()
        );
    }

    /// An empty declaration is not a declaration — it means the operator left
    /// the key blank, not that the repository is the empty path.
    #[test]
    fn blank_declaration_is_treated_as_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = tmp.path().join("config.toml");
        std::fs::write(
            &cfg,
            "[project]\nproject_id = \"test-0000\"\ntarget_repo = \"   \"\n",
        )
        .unwrap();

        let resolved = resolve_from_config(&cfg).unwrap();
        assert_eq!(resolved.source, RepoSource::Cwd);
    }

    /// Each `RepoSource` names itself in words an operator can act on.
    #[test]
    fn every_source_describes_itself() {
        assert!(RepoSource::Declared.describe().contains("target_repo"));
        assert!(RepoSource::Cwd.describe().contains("working directory"));
    }
}
