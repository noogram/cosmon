// SPDX-License-Identifier: AGPL-3.0-only

//! The pure half of `cs spore install` — the package-manager verb that fetches
//! a spore bundle and **places** it into the current project.
//!
//! # Why a verb, and why this one
//!
//! `cs spore run` reads a bundle from a path that must already exist on the
//! operator's disk. Sharing a spore therefore had no first step: the recipient
//! ran `git clone` (or copied a directory by hand), guessed where to put it, and
//! — the part that actually bites — usually never copied the bundle's recipes
//! into `.cosmon/formulas/`. A germinated molecule stores its formula **by id**
//! and `cs tackle` resolves that id against the *mission project's* registry,
//! not against the directory the spore came from, so an uninstalled bundle
//! germinates fine and then dispatches with every per-step `adapter`/`model` pin
//! silently inert (task-20260725-eb3b). `cs spore run` warns about this; the
//! warning names a copy nobody had a verb for.
//!
//! Installing *is* that copy. The verb is named `install` and not `add` or
//! `import` for that reason: its load-bearing effect is the **registration** of
//! the bundle's recipes into the registry the dispatcher reads, which is what
//! "install" means and what "add" (edit a dependency manifest — cosmon has none
//! for spores) and "import" (the inverse of `export`, which emits a hash and an
//! RO-Crate in place, not a fetchable artifact) both misdescribe.
//!
//! # What lives here
//!
//! Everything decidable without touching disk or network:
//!
//! - [`parse_source`] — the `<SOURCE>` grammar (local path, `github:` shorthand,
//!   GitHub `tree`/`blob` URL, any other git remote) into a [`SporeSource`];
//! - [`check_bundle_path`] — the containment grammar for a manifest-declared
//!   relative path, so a hostile bundle cannot make the installer write outside
//!   the destination;
//! - [`plan_formula`] — the three-way registry decision (install / already
//!   identical / conflicts) that decides whether an install is a no-op, a write,
//!   or a refusal;
//! - [`default_dest_slug`] — where a bundle lands when the operator names no
//!   destination.
//!
//! The shell (`cs spore install`) owns the git subprocess, the copying, and the
//! hash check. It can therefore be reasoned about here, in tests that need no
//! network.

use std::path::{Component, Path};

use super::validate_node_id;
use super::SporeError;

/// The conventional directory, relative to the project root, that holds spore
/// bundles installed into a project. Matches the existing `spores/<name>/`
/// layout the repository already uses for its own bundles.
pub const SPORES_DIR: &str = "spores";

/// Where an installed bundle records what it was installed *from*.
///
/// Dot-prefixed because it is install metadata, not bundle content: it is
/// deliberately outside the coverage set a bundle hash binds, so writing it
/// never changes the id of the bundle it describes.
pub const PROVENANCE_FILE: &str = ".spore-install.toml";

// ---------------------------------------------------------------------------
// Source grammar
// ---------------------------------------------------------------------------

/// Where a bundle is fetched from, once the `<SOURCE>` string is understood.
///
/// Deliberately two variants and not one per forge: a GitHub URL is not a
/// different *kind* of source, it is a git remote with a well-known way of
/// spelling a ref and a subdirectory in the URL path. Recognizing that spelling
/// is a parsing convenience; the fetch is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SporeSource {
    /// A directory (or `spore.toml` file) already on this machine. Copied, not
    /// cloned — the source of truth for `cs spore install ../some/bundle`.
    Local(String),

    /// A git remote, optionally pinned to a ref and narrowed to a subdirectory
    /// of the checkout.
    Git {
        /// The remote as `git` will be handed it (`https://…`, `git@…`, `file://…`).
        remote: String,
        /// The branch, tag, or commit to fetch. `None` means the remote's `HEAD`.
        git_ref: Option<String>,
        /// The bundle's path inside the checkout. `None` means the repo root.
        subdir: Option<String>,
    },
}

/// Why a `<SOURCE>` string could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// The string was empty or whitespace.
    #[error("empty spore source")]
    Empty,

    /// A `github:` shorthand did not carry `owner/repo`.
    #[error("`{0}` is not a valid github: shorthand (expected github:owner/repo[/subdir][@ref])")]
    MalformedGithubShorthand(String),

    /// A `https://github.com/...` URL did not carry `owner/repo`.
    #[error("`{0}` is not a valid GitHub URL (expected https://github.com/owner/repo[/tree/<ref>/<subdir>])")]
    MalformedGithubUrl(String),

    /// The subdirectory encoded in the source escapes the checkout.
    #[error("subdirectory `{path}` is not a safe bundle path: {reason}")]
    UnsafeSubdir {
        /// The offending path as written.
        path: String,
        /// Why it was refused.
        reason: &'static str,
    },
}

/// The GitHub host prefix the URL forms are recognized under.
const GITHUB_HTTPS: &str = "https://github.com/";
/// The `github:owner/repo` shorthand prefix.
const GITHUB_SHORTHAND: &str = "github:";

/// Understand a `<SOURCE>` string.
///
/// Four spellings, in the order they are tried:
///
/// 1. `github:owner/repo[/subdir][@ref]` — the shorthand. Unambiguous, because
///    the prefix rules out the scp-like `git@host:path` form where a bare
///    trailing `@ref` could not be told from the user part of the remote.
/// 2. `https://github.com/owner/repo`, `.../tree/<ref>/<subdir>`, or
///    `.../blob/<ref>/<subdir>/spore.toml` — what a browser puts on the
///    clipboard. The `blob` form drops the trailing file name, so pasting the
///    URL of the manifest itself does the expected thing.
/// 3. Anything else that looks like a git remote (`https://`, `http://`,
///    `ssh://`, `git://`, `file://`, or `user@host:path`) — taken verbatim; its
///    ref and subdirectory come from the caller's flags, never from guessing at
///    a grammar this function cannot disambiguate.
/// 4. Otherwise: a local path.
///
/// `git_ref` and `subdir` from the URL are *defaults*; the shell overrides them
/// with `--git-ref` / `--subdir` when given, so a generic remote stays fully
/// addressable without an ambiguous grammar.
///
/// # Errors
/// [`SourceError`] when a recognized forge spelling is malformed, or when the
/// subdirectory it encodes would escape the checkout.
pub fn parse_source(raw: &str) -> Result<SporeSource, SourceError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(SourceError::Empty);
    }

    if let Some(rest) = raw.strip_prefix(GITHUB_SHORTHAND) {
        return parse_github_shorthand(rest, raw);
    }
    if let Some(rest) = raw.strip_prefix(GITHUB_HTTPS) {
        return parse_github_url(rest, raw);
    }
    if looks_like_remote(raw) {
        return Ok(SporeSource::Git {
            remote: raw.to_string(),
            git_ref: None,
            subdir: None,
        });
    }
    Ok(SporeSource::Local(raw.to_string()))
}

/// `github:owner/repo[/subdir][@ref]`.
fn parse_github_shorthand(rest: &str, raw: &str) -> Result<SporeSource, SourceError> {
    let (path, git_ref) = match rest.rsplit_once('@') {
        Some((p, r)) if !r.is_empty() => (p, Some(r.to_string())),
        _ => (rest, None),
    };
    let mut parts = path.trim_matches('/').splitn(3, '/');
    let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
        return Err(SourceError::MalformedGithubShorthand(raw.to_string()));
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(SourceError::MalformedGithubShorthand(raw.to_string()));
    }
    let subdir = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
    Ok(SporeSource::Git {
        remote: format!("{GITHUB_HTTPS}{owner}/{}", repo.trim_end_matches(".git")),
        git_ref,
        subdir: checked_subdir(subdir)?,
    })
}

/// `owner/repo[.git]`, `owner/repo/tree/<ref>/<subdir>`, or
/// `owner/repo/blob/<ref>/<subdir>/spore.toml` (the `https://github.com/`
/// prefix already stripped).
fn parse_github_url(rest: &str, raw: &str) -> Result<SporeSource, SourceError> {
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let segments: Vec<&str> = rest
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let (Some(owner), Some(repo)) = (segments.first(), segments.get(1)) else {
        return Err(SourceError::MalformedGithubUrl(raw.to_string()));
    };
    let remote = format!("{GITHUB_HTTPS}{owner}/{}", repo.trim_end_matches(".git"));

    let kind = segments
        .get(2)
        .copied()
        .filter(|s| *s == "tree" || *s == "blob");
    let (git_ref, subdir) = if let Some(kind) = kind {
        let Some(git_ref) = segments.get(3).copied() else {
            return Err(SourceError::MalformedGithubUrl(raw.to_string()));
        };
        let mut path: Vec<&str> = segments[4.min(segments.len())..].to_vec();
        // A `blob` URL names a file; the bundle is the directory holding it.
        if kind == "blob" {
            path.pop();
        }
        let joined = path.join("/");
        (
            Some(git_ref.to_string()),
            Some(joined).filter(|s| !s.is_empty()),
        )
    } else {
        // Not a tree/blob URL: everything after `owner/repo` is a path in the
        // default branch (what `https://github.com/o/r/spores/x` would mean).
        let joined = segments[2.min(segments.len())..].join("/");
        (None, Some(joined).filter(|s| !s.is_empty()))
    };

    Ok(SporeSource::Git {
        remote,
        git_ref,
        subdir: checked_subdir(subdir)?,
    })
}

/// Whether a string is a git remote rather than a filesystem path.
fn looks_like_remote(raw: &str) -> bool {
    const SCHEMES: [&str; 5] = ["https://", "http://", "ssh://", "git://", "file://"];
    if SCHEMES.iter().any(|s| raw.starts_with(s)) {
        return true;
    }
    // scp-like `user@host:path` — a colon after an `@`, with no path separator
    // before either, which no ordinary relative path has.
    match raw.split_once('@') {
        Some((user, rest)) => !user.contains('/') && rest.contains(':'),
        None => false,
    }
}

/// Run a URL-derived subdirectory through the same containment grammar as a
/// manifest-declared path, so no spelling of a source can point the fetch at
/// `../../etc`.
fn checked_subdir(subdir: Option<String>) -> Result<Option<String>, SourceError> {
    match subdir {
        None => Ok(None),
        Some(path) => match check_bundle_path(&path) {
            Ok(()) => Ok(Some(path)),
            Err(reason) => Err(SourceError::UnsafeSubdir { path, reason }),
        },
    }
}

// ---------------------------------------------------------------------------
// Path containment
// ---------------------------------------------------------------------------

/// Refuse any relative path that would let a fetched bundle write outside the
/// destination it was given.
///
/// The installer copies files whose names come from a manifest an operator did
/// not write — the whole point of installing from a URL. `Path::join` with an
/// absolute path *replaces* the base and a `..` component walks out of it, so a
/// bundle declaring `path = "../../.ssh/authorized_keys"` would otherwise place
/// a file wherever it liked. This is the single grammar every such path passes
/// through, in the pure core, where it can be tested without a filesystem.
///
/// Accepted: a non-empty relative path of ordinary components. Rejected:
/// absolute paths, any root or prefix component, any `..`, and any `.` — the
/// last one not because it is dangerous but because it means the path was
/// composed, not written, and the installer should not be guessing.
///
/// # Errors
/// A static reason string naming which rule was broken.
pub fn check_bundle_path(rel: &str) -> Result<(), &'static str> {
    if rel.trim().is_empty() {
        return Err("it is empty");
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err("it is absolute");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::ParentDir => return Err("it contains a `..` traversal"),
            Component::CurDir => return Err("it contains a `.` component"),
            Component::RootDir | Component::Prefix(_) => return Err("it is rooted"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Registry planning
// ---------------------------------------------------------------------------

/// What installing one of a bundle's recipes into `.cosmon/formulas/` would do.
///
/// The three cases are kept distinct because they have three different
/// meanings for the operator: a write, a no-op, and a refusal. Collapsing
/// "already there and identical" into "conflict" would make re-installing a
/// bundle require `--force`, which trains an operator to pass `--force` — and
/// then the one real conflict goes through unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FormulaPlacement {
    /// Nothing of that name is in the registry: write it.
    Install,

    /// The registry already holds byte-identical content: nothing to do. This
    /// is what makes `cs spore install` idempotent.
    Identical,

    /// The registry holds a *different* recipe under the same name. Dispatch
    /// resolves the id against the registry, so writing would silently change
    /// the behaviour of every molecule already germinated from it. Refuse
    /// unless the operator overrides.
    Conflicts,
}

/// Decide [`FormulaPlacement`] for one recipe.
///
/// `existing` is the registry's current bytes for that formula name, or `None`
/// when the registry has no such file. Byte comparison and not a semantic one:
/// two recipes that differ only in a comment still differ in what a reader of
/// the registry sees, and the conservative answer is the one that asks.
#[must_use]
pub fn plan_formula(bundle: &str, existing: Option<&str>) -> FormulaPlacement {
    match existing {
        None => FormulaPlacement::Install,
        Some(current) if current == bundle => FormulaPlacement::Identical,
        Some(_) => FormulaPlacement::Conflicts,
    }
}

/// The registry file name a formula is installed under.
///
/// The name comes from the recipe's own `formula = "..."` field and **not** from
/// the file name it had in the bundle, because that is the name `cs tackle`
/// resolves at dispatch. Installing `work.formula.toml` as `work.formula.toml`
/// when it declares `formula = "converge"` would put the file in the registry
/// and leave the pins just as unreachable as before — an install that appears to
/// work and changes nothing.
#[must_use]
pub fn registry_file_name(formula_name: &str) -> String {
    format!("{formula_name}.formula.toml")
}

// ---------------------------------------------------------------------------
// Destination
// ---------------------------------------------------------------------------

/// The directory name a bundle lands in under `<project>/spores/` when the
/// operator names no `--dest`.
///
/// The spore's own name, validated against the node-id slug grammar — the same
/// grammar for the same reason: the string becomes a directory component, and a
/// name like `../..` would place the bundle outside the project. A spore whose
/// name is not a safe slug is not refused outright by this function's caller;
/// it simply has no default, and the operator must say `--dest`.
///
/// # Errors
/// [`SporeError::InvalidNodeId`] when the name is not a safe path slug.
pub fn default_dest_slug(spore_name: &str) -> Result<&str, SporeError> {
    validate_node_id(spore_name)?;
    Ok(spore_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_directory_is_a_local_source() {
        assert_eq!(
            parse_source("./spores/cosmon-dev").expect("a path parses"),
            SporeSource::Local("./spores/cosmon-dev".to_string())
        );
    }

    #[test]
    fn github_shorthand_carries_subdir_and_ref() {
        assert_eq!(
            parse_source("github:noogram/cosmon/spores/cosmon-dev@v0.1.0").expect("shorthand"),
            SporeSource::Git {
                remote: "https://github.com/noogram/cosmon".to_string(),
                git_ref: Some("v0.1.0".to_string()),
                subdir: Some("spores/cosmon-dev".to_string()),
            }
        );
    }

    #[test]
    fn github_shorthand_without_owner_repo_is_refused() {
        assert!(matches!(
            parse_source("github:noogram"),
            Err(SourceError::MalformedGithubShorthand(_))
        ));
    }

    #[test]
    fn a_browser_tree_url_yields_ref_and_subdir() {
        assert_eq!(
            parse_source("https://github.com/noogram/cosmon/tree/main/spores/cosmon-dev")
                .expect("tree url"),
            SporeSource::Git {
                remote: "https://github.com/noogram/cosmon".to_string(),
                git_ref: Some("main".to_string()),
                subdir: Some("spores/cosmon-dev".to_string()),
            }
        );
    }

    #[test]
    fn a_blob_url_of_the_manifest_installs_its_directory() {
        assert_eq!(
            parse_source(
                "https://github.com/noogram/cosmon/blob/main/spores/cosmon-dev/spore.toml"
            )
            .expect("blob url"),
            SporeSource::Git {
                remote: "https://github.com/noogram/cosmon".to_string(),
                git_ref: Some("main".to_string()),
                subdir: Some("spores/cosmon-dev".to_string()),
            }
        );
    }

    #[test]
    fn a_repo_url_installs_the_repository_root() {
        assert_eq!(
            parse_source("https://github.com/noogram/cosmon.git").expect("repo url"),
            SporeSource::Git {
                remote: "https://github.com/noogram/cosmon".to_string(),
                git_ref: None,
                subdir: None,
            }
        );
    }

    #[test]
    fn a_query_string_does_not_leak_into_the_subdir() {
        assert_eq!(
            parse_source("https://github.com/o/r/tree/main/spores/x?plain=1").expect("url"),
            SporeSource::Git {
                remote: "https://github.com/o/r".to_string(),
                git_ref: Some("main".to_string()),
                subdir: Some("spores/x".to_string()),
            }
        );
    }

    #[test]
    fn an_scp_like_remote_is_not_mistaken_for_a_path() {
        assert_eq!(
            parse_source("git@github.com:noogram/cosmon.git").expect("scp remote"),
            SporeSource::Git {
                remote: "git@github.com:noogram/cosmon.git".to_string(),
                git_ref: None,
                subdir: None,
            }
        );
    }

    #[test]
    fn a_traversing_subdir_in_a_url_is_refused() {
        assert!(matches!(
            parse_source("https://github.com/o/r/tree/main/../../etc"),
            Err(SourceError::UnsafeSubdir { .. })
        ));
    }

    #[test]
    fn bundle_paths_that_escape_are_refused() {
        assert!(check_bundle_path("formulas/work.formula.toml").is_ok());
        assert!(check_bundle_path("../outside.toml").is_err());
        assert!(check_bundle_path("nested/../../outside.toml").is_err());
        assert!(check_bundle_path("/etc/passwd").is_err());
        assert!(check_bundle_path("./here.toml").is_err());
        assert!(check_bundle_path("").is_err());
    }

    #[test]
    fn a_reinstall_of_the_same_recipe_is_a_no_op() {
        assert_eq!(plan_formula("body", None), FormulaPlacement::Install);
        assert_eq!(
            plan_formula("body", Some("body")),
            FormulaPlacement::Identical
        );
        assert_eq!(
            plan_formula("body", Some("other")),
            FormulaPlacement::Conflicts
        );
    }

    #[test]
    fn the_registry_name_comes_from_the_declared_formula_name() {
        assert_eq!(registry_file_name("converge"), "converge.formula.toml");
    }

    #[test]
    fn a_hostile_spore_name_has_no_default_destination() {
        assert_eq!(default_dest_slug("cosmon-dev").expect("slug"), "cosmon-dev");
        assert!(default_dest_slug("../escape").is_err());
        assert!(default_dest_slug("").is_err());
    }
}
