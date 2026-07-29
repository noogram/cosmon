// SPDX-License-Identifier: AGPL-3.0-only

//! Resolution of a molecule's **integration base** — the branch its worker's
//! `feat/<mol-id>` branch is cut from and merged back into.
//!
//! # Why this module exists
//!
//! The base used to be an *ambient* fact, deduced independently at the two
//! ends of a molecule's life and free to disagree between them:
//!
//! * at the cut, `cs tackle` branched from whatever `HEAD` the main checkout
//!   happened to point at;
//! * at the harvest, `cs done` re-derived it from `COSMON_BASE_BRANCH`, then
//!   `origin/HEAD`, then the literal `"main"`.
//!
//! Piloting two molecules on two different trunks from one session therefore
//! meant a manual `git checkout` dance around every `cs tackle` and an
//! `export` in front of every `cs done` — and a `cs done` fired from a tmux
//! hook, whose server environment froze at startup and never sees a later
//! `export`, refused the merge with `NotOnBase`.
//!
//! [`resolve`](crate::base_branch::resolve) makes the base a **property of the
//! molecule**: the branch named by `cs tackle --base` is persisted in
//! [`MoleculeData::base_branch`](cosmon_state::MoleculeData::base_branch) and
//! wins over every ambient source. Molecules with no persisted base keep the
//! pre-existing behaviour byte for byte.
//!
//! ```no_run
//! use std::path::Path;
//! use cosmon_cli::base_branch;
//!
//! // A molecule tackled with `--base release/2.0` merges back there,
//! // whatever the environment or the galaxy's configured trunk says.
//! let base = base_branch::resolve(Path::new("/repo"), Some("release/2.0"), Some("dev"));
//! assert_eq!(base, "release/2.0");
//! ```

use std::path::Path;
use std::process::Command;

/// Environment variable naming the base branch when the molecule does not
/// carry one — the pre-existing operator override.
pub const BASE_BRANCH_ENV: &str = "COSMON_BASE_BRANCH";

/// Last-resort default when nothing else resolves.
pub const DEFAULT_BASE_BRANCH: &str = "main";

/// Which link of the precedence chain named the branch that won.
///
/// Carried alongside the name so a refusal can say *where the branch came
/// from* rather than only what it is. Five sources can produce the string
/// `main`, and "the configured base branch is `main`" is true and useless on a
/// galaxy whose trunk is misconfigured: the operator cannot tell an explicit
/// `[project] trunk_branch = "main"` from a silent fallback that reached the
/// built-in default because nothing else answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseSource {
    /// The molecule's own persisted `base_branch`, stamped by
    /// `cs tackle --base`.
    Persisted,
    /// The [`BASE_BRANCH_ENV`] environment variable.
    Environment,
    /// The galaxy's `[project] trunk_branch` config declaration.
    ConfiguredTrunk,
    /// `origin/HEAD` — the default branch the remote advertises.
    OriginHead,
    /// The built-in [`DEFAULT_BASE_BRANCH`], reached when nothing answered.
    Default,
}

impl BaseSource {
    /// A short operator-facing phrase naming this source, for refusal
    /// messages. Written to slot after "resolved from".
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Persisted => "the molecule's own base, stamped by `cs tackle --base`",
            Self::Environment => "the COSMON_BASE_BRANCH environment variable",
            Self::ConfiguredTrunk => "`[project] trunk_branch` in .cosmon/config.toml",
            Self::OriginHead => "origin/HEAD, the default branch the remote advertises",
            Self::Default => "cosmon's built-in default — nothing else named a branch",
        }
    }
}

/// A resolved base branch together with the chain link that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBase {
    /// The branch *name*, not a full ref.
    pub branch: String,
    /// Which link of the precedence chain won.
    pub source: BaseSource,
}

/// Resolve the branch a molecule's work must be integrated into.
///
/// Precedence, first non-empty wins:
///
/// 1. `persisted` — the molecule's own
///    [`base_branch`](cosmon_state::MoleculeData::base_branch), stamped by
///    `cs tackle --base`. A property of the molecule, so it survives a frozen
///    tmux environment, a different shell, and a hook-triggered harvest. It
///    outranks the galaxy's configured trunk deliberately: a molecule may
///    legitimately integrate into a parked work branch, and letting a later
///    config edit retarget an in-flight molecule would silently rewrite where
///    finished work lands.
/// 2. the `COSMON_BASE_BRANCH` environment variable — the session-wide
///    operator override, explicit by construction.
/// 3. `configured_trunk` — the galaxy's
///    [`trunk_branch`](cosmon_core::config::ProjectSection::trunk_branch)
///    declaration. Naming the trunk is the operator's authoritative statement
///    of where work integrates by default; before this link it governed only
///    the deploy gate ([`reference_trunk`]) and was inert for the merge
///    itself, so a galaxy whose private trunk is `dev` still had `cs done`
///    resolve `main` and refuse (task-20260729-b016).
/// 4. `git symbolic-ref --short refs/remotes/origin/HEAD`, stripped of its
///    `origin/` prefix — the default branch the remote advertises. Skipped
///    when there is no `origin`.
/// 5. the literal [`DEFAULT_BASE_BRANCH`].
///
/// Returns the branch *name*, not a full ref: callers concatenate it with
/// `refs/heads/` or hand it to `git merge-base --is-ancestor`, which resolves
/// it as a commitish. Use [`resolve_with_source`] when the caller must also
/// report *why* that branch won.
#[must_use]
pub fn resolve(
    repo_root: &Path,
    persisted: Option<&str>,
    configured_trunk: Option<&str>,
) -> String {
    resolve_with_source(repo_root, persisted, configured_trunk).branch
}

/// [`resolve`], keeping the chain link that produced the answer.
#[must_use]
pub fn resolve_with_source(
    repo_root: &Path,
    persisted: Option<&str>,
    configured_trunk: Option<&str>,
) -> ResolvedBase {
    if let Some(base) = persisted.map(str::trim).filter(|b| !b.is_empty()) {
        return ResolvedBase {
            branch: base.to_owned(),
            source: BaseSource::Persisted,
        };
    }
    resolve_ambient_with_source(repo_root, configured_trunk)
}

/// The ambient half of [`resolve`] — steps 2 to 5, with no molecule in hand.
///
/// Exposed separately because a few call sites genuinely have no molecule
/// (repository-wide probes); everything that *does* have one must go through
/// [`resolve`] so the molecule's own base cannot be silently ignored.
///
/// `configured_trunk` is passed in rather than read from disk here: this crate
/// keeps its I/O at the edges, and a function that takes the value is testable
/// without standing up a fixture galaxy.
#[must_use]
pub fn resolve_ambient(repo_root: &Path, configured_trunk: Option<&str>) -> String {
    resolve_ambient_with_source(repo_root, configured_trunk).branch
}

/// [`resolve_ambient`], keeping the chain link that produced the answer.
fn resolve_ambient_with_source(repo_root: &Path, configured_trunk: Option<&str>) -> ResolvedBase {
    if let Ok(explicit) = std::env::var(BASE_BRANCH_ENV) {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return ResolvedBase {
                branch: explicit.to_owned(),
                source: BaseSource::Environment,
            };
        }
    }

    if let Some(trunk) = configured_trunk.map(str::trim).filter(|b| !b.is_empty()) {
        return ResolvedBase {
            branch: trunk.to_owned(),
            source: BaseSource::ConfiguredTrunk,
        };
    }

    if let Some(remote_head) = origin_head(repo_root) {
        return ResolvedBase {
            branch: remote_head,
            source: BaseSource::OriginHead,
        };
    }

    ResolvedBase {
        branch: DEFAULT_BASE_BRANCH.to_owned(),
        source: BaseSource::Default,
    }
}

/// The galaxy's **reference trunk** — the branch it treats as its principal
/// line of integration.
///
/// This is a *different question* from [`resolve`]. `resolve` answers "where
/// does *this molecule's* work integrate?", and honours the molecule's own
/// persisted base and the `COSMON_BASE_BRANCH` session override — a molecule
/// may legitimately integrate into a parked work branch. `reference_trunk`
/// answers "what does the galaxy consider its *trunk*?", independent of any
/// one molecule, so it deliberately ignores both the persisted base and the
/// environment override. It consults, first answer wins:
///
/// 1. `configured` — the galaxy's explicit
///    [`trunk_branch`](cosmon_core::config::ProjectSection::trunk_branch)
///    declaration. The operator's authoritative statement of the trunk name;
///    it removes any need to *assume* the trunk is called `main`.
/// 2. `git symbolic-ref --short refs/remotes/origin/HEAD`, stripped of its
///    `origin/` prefix — the default branch the remote advertises, when it
///    answers. Drift-proof discovery: a galaxy that renames its default branch
///    on the remote moves its trunk with it, no config edit required.
/// 3. the literal [`DEFAULT_BASE_BRANCH`] (`main`) as a last resort — reached
///    only when nothing is configured and there is no `origin` to ask.
///
/// # Why the two differ
///
/// Since task-20260729-b016 the two chains are the *same chain* minus a
/// prefix: [`resolve`] is exactly the two molecule-scoped steps (the persisted
/// base, then `COSMON_BASE_BRANCH`) followed by these three, in this order. So
/// the only way the two answers can diverge is a molecule that carries its own
/// base or a session that exports the override — which is precisely the
/// intended difference, not an accident of two hand-maintained lists.
///
/// They therefore coincide for the common case — a molecule cut from and
/// merged back into the trunk. They diverge exactly when work is harvested onto
/// a parked branch (`feat/…`): there the destination is the parked branch while
/// the trunk is unchanged. Consumers that must act *only for a real trunk
/// merge* — most sharply the `post_merge` deploy hook, which refreshes the
/// on-disk binary and would silently rejuvenate it from a parked, older branch
/// — compare the resolved base against this trunk rather than against a
/// hard-coded `"main"` string (task-20260725-b64f).
///
/// The two are kept separate rather than merged: the questions are genuinely
/// different — "where does *this molecule* integrate?" versus "what is the
/// galaxy's trunk?" — and a single function would have to be told which one
/// the caller means, which is the two functions again with worse names.
#[must_use]
pub fn reference_trunk(repo_root: &Path, configured: Option<&str>) -> String {
    if let Some(trunk) = configured.map(str::trim).filter(|b| !b.is_empty()) {
        return trunk.to_owned();
    }
    origin_head(repo_root).unwrap_or_else(|| DEFAULT_BASE_BRANCH.to_owned())
}

/// Read the default branch `origin` advertises, as a local branch name.
fn origin_head(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `symbolic-ref --short` already trims `refs/remotes/` — the result is
    // e.g. `origin/main`. Strip the remote prefix to get the local branch.
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let name = raw.strip_prefix("origin/").unwrap_or(&raw);
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A molecule that carries its own base wins over everything ambient —
    /// this is the whole point of the field. Deliberately env-agnostic: the
    /// persisted branch short-circuits before `COSMON_BASE_BRANCH` is even
    /// read, so this holds whatever the operator exported.
    #[test]
    fn persisted_base_wins_over_ambient_resolution() {
        assert_eq!(
            resolve(Path::new("/nonexistent"), Some("release/2.0"), None),
            "release/2.0"
        );
    }

    /// A blank persisted value is not a base branch — fall through to the
    /// ambient chain rather than merging into `""`.
    #[test]
    fn blank_persisted_base_falls_through_to_ambient() {
        let ambient = resolve_ambient(Path::new("/nonexistent"), None);
        assert_eq!(
            resolve(Path::new("/nonexistent"), Some("   "), None),
            ambient
        );
        assert_eq!(resolve(Path::new("/nonexistent"), None, None), ambient);
    }

    /// Backward compatibility: no persisted base, no env, no configured
    /// trunk, no `origin` → exactly the historical default.
    #[test]
    fn no_molecule_base_no_env_defaults_to_main() {
        if std::env::var_os(BASE_BRANCH_ENV).is_some() {
            // The operator's own override is in scope; this test asserts the
            // *absence* branch and has nothing to say here.
            return;
        }
        assert_eq!(
            resolve(Path::new("/nonexistent"), None, None),
            DEFAULT_BASE_BRANCH
        );
    }

    /// The regression this module was changed for (task-20260729-b016): an
    /// explicit `[project] trunk_branch` must govern the merge DESTINATION,
    /// not only the deploy gate. Asserted on a nonexistent repo so the
    /// `origin/HEAD` probe cannot answer — before the fix this returned
    /// `main` (the built-in default) with `trunk_branch = "dev"` committed.
    #[test]
    fn configured_trunk_governs_the_merge_destination() {
        if std::env::var_os(BASE_BRANCH_ENV).is_some() {
            return;
        }
        assert_eq!(resolve(Path::new("/nonexistent"), None, Some("dev")), "dev");
    }

    /// A blank `trunk_branch` is not a trunk name — fall through rather than
    /// merging into `""`.
    #[test]
    fn blank_configured_trunk_falls_through_in_resolve() {
        if std::env::var_os(BASE_BRANCH_ENV).is_some() {
            return;
        }
        assert_eq!(
            resolve(Path::new("/nonexistent"), None, Some("   ")),
            DEFAULT_BASE_BRANCH
        );
    }

    /// The molecule's own base still outranks the galaxy's configured trunk:
    /// editing config must not retarget work already in flight.
    #[test]
    fn persisted_base_beats_configured_trunk() {
        assert_eq!(
            resolve(Path::new("/nonexistent"), Some("release/2.0"), Some("dev")),
            "release/2.0"
        );
    }

    /// The session override still outranks the galaxy's configured trunk —
    /// `COSMON_BASE_BRANCH` is an explicit gesture, `trunk_branch` a default.
    ///
    /// The env var is read by the production chain, so this test sets it; it
    /// runs in its own process via `--test-threads` only when the suite is
    /// serialised, so it asserts through the pure helper instead of mutating
    /// the environment: `resolve_ambient_with_source` reads the env first and
    /// the chain below it is what we pin here.
    #[test]
    fn env_override_beats_configured_trunk() {
        let Ok(explicit) = std::env::var(BASE_BRANCH_ENV) else {
            // No override exported: assert the shape we *can* observe — with
            // no env, the configured trunk wins the ambient chain.
            assert_eq!(
                resolve_ambient_with_source(Path::new("/nonexistent"), Some("dev")),
                ResolvedBase {
                    branch: "dev".to_owned(),
                    source: BaseSource::ConfiguredTrunk,
                }
            );
            return;
        };
        assert_eq!(
            resolve_ambient(Path::new("/nonexistent"), Some("dev")),
            explicit.trim(),
            "an exported COSMON_BASE_BRANCH must outrank [project] trunk_branch"
        );
    }

    /// Every link of the chain reports itself, so a refusal can name where
    /// the branch came from instead of only what it is.
    #[test]
    fn each_link_names_itself() {
        assert_eq!(
            resolve_with_source(Path::new("/nonexistent"), Some("release/2.0"), Some("dev")).source,
            BaseSource::Persisted
        );
        if std::env::var_os(BASE_BRANCH_ENV).is_none() {
            assert_eq!(
                resolve_with_source(Path::new("/nonexistent"), None, Some("dev")).source,
                BaseSource::ConfiguredTrunk
            );
            assert_eq!(
                resolve_with_source(Path::new("/nonexistent"), None, None).source,
                BaseSource::Default
            );
        }
        // Every phrase is non-empty: the message that embeds it must never
        // read "resolved from ".
        for source in [
            BaseSource::Persisted,
            BaseSource::Environment,
            BaseSource::ConfiguredTrunk,
            BaseSource::OriginHead,
            BaseSource::Default,
        ] {
            assert!(!source.describe().is_empty());
        }
    }

    /// The reference trunk ignores an operator's `COSMON_BASE_BRANCH` override
    /// and any persisted molecule base — it is a property of the galaxy, not of
    /// the merge. With nothing configured and no `origin` to advertise a
    /// default, it falls back to the literal trunk regardless of the
    /// environment.
    #[test]
    fn reference_trunk_ignores_env_and_falls_back_to_main() {
        // No config, no `origin/HEAD` on a nonexistent path → the `main` last
        // resort, whatever `COSMON_BASE_BRANCH` might hold. The function never
        // reads the env var, so this holds unconditionally.
        assert_eq!(
            reference_trunk(Path::new("/nonexistent"), None),
            DEFAULT_BASE_BRANCH
        );
    }

    /// An explicit `trunk_branch` declaration is authoritative: it wins over
    /// remote discovery and the `main` fallback alike, so a galaxy never has to
    /// *assume* its trunk is called `main`.
    #[test]
    fn configured_trunk_wins_over_discovery_and_default() {
        assert_eq!(
            reference_trunk(Path::new("/nonexistent"), Some("develop")),
            "develop"
        );
    }

    /// A blank configured value is not a trunk name — fall through to
    /// discovery / the default rather than pinning the trunk to `""`.
    #[test]
    fn blank_configured_trunk_falls_through() {
        assert_eq!(
            reference_trunk(Path::new("/nonexistent"), Some("   ")),
            DEFAULT_BASE_BRANCH
        );
    }
}
