// SPDX-License-Identifier: AGPL-3.0-only

//! Protected paths — inputs a molecule declares as ground truth, which its
//! worker must never modify (issue #94).
//!
//! # The pathology this closes
//!
//! A worker handed reference data to validate a fix against found its own
//! output "very close" to the reference and announced it would replace the
//! reference with its own output. The operator happened to be watching. Had
//! they not been, the rewritten reference would have merged at `cs done` and
//! the validation it was meant to support would have become circular: the
//! result matches the expectation because the expectation was copied from the
//! result.
//!
//! # The mechanism
//!
//! `cs nucleate --protect <path>` records repository-relative paths on the
//! molecule. Three layers read that list:
//!
//! 1. the worker's brief states the paths as read-only, with the reason;
//! 2. `cs tackle` clears the write bits of those files in the worktree, so an
//!    accidental write fails at once;
//! 3. `cs done` refuses the merge when the worker branch changed any of them
//!    relative to its merge base, and names each path.
//!
//! The first two are advisory: a worker can ignore the brief and restore the
//! write bit. The third is the gate, because it inspects what would land.
//!
//! # Architectural posture
//!
//! This module is the I/O-free half: validation of a declared path and the
//! match of a change list against the protected set. The git diff, the file
//! permissions and the refusal live in the effect crates, the same split as
//! [`crate::scope_guard`].

use std::fmt;

/// Why a declared protected path was rejected at nucleation.
///
/// Every variant is decidable from the string alone, so a malformed
/// declaration fails before a molecule exists rather than at `cs done`, hours
/// later, as a gate that silently matches nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectedPathError {
    /// The path is empty (or only `./` and slashes).
    Empty,
    /// The path is absolute. Protection is relative to the repository root,
    /// because that is the only frame in which the base branch and the worker
    /// branch can be compared.
    Absolute(String),
    /// The path contains a `..` component, so it may name something outside
    /// the repository.
    ParentComponent(String),
}

impl fmt::Display for ProtectedPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a protected path must not be empty"),
            Self::Absolute(p) => write!(
                f,
                "protected path `{p}` is absolute; give it relative to the repository root"
            ),
            Self::ParentComponent(p) => write!(
                f,
                "protected path `{p}` contains `..`; give it relative to the repository root"
            ),
        }
    }
}

impl std::error::Error for ProtectedPathError {}

/// Validate and normalise one declared protected path.
///
/// Accepts a file or a directory, relative to the repository root. A leading
/// `./`, repeated slashes and a trailing slash are removed, so `./ref/` and
/// `ref` protect the same tree.
///
/// # Errors
///
/// [`ProtectedPathError`] when the path is empty, absolute, or climbs out of
/// the repository with `..`.
pub fn normalize_protected_path(raw: &str) -> Result<String, ProtectedPathError> {
    let trimmed = raw.trim();
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(ProtectedPathError::Absolute(trimmed.to_owned()));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in trimmed.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(ProtectedPathError::ParentComponent(trimmed.to_owned())),
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Err(ProtectedPathError::Empty);
    }
    Ok(parts.join("/"))
}

/// Whether `changed` (a repository-relative path as `git diff --name-only`
/// reports it) falls under the protected entry `protected`.
///
/// A protected entry covers itself and, when it names a directory, every path
/// below it. The match is on whole components: protecting `ref` does not
/// cover `reference.csv`.
#[must_use]
pub fn is_under(changed: &str, protected: &str) -> bool {
    changed == protected
        || changed
            .strip_prefix(protected)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The changed paths that fall under any protected entry, in the order the
/// change list gave them.
///
/// Empty when nothing protected changed, including when `protected` is empty
/// — a molecule that declared nothing is unaffected.
#[must_use]
pub fn modified_protected_paths(changed: &[String], protected: &[String]) -> Vec<String> {
    if protected.is_empty() {
        return Vec::new();
    }
    changed
        .iter()
        .filter(|c| protected.iter().any(|p| is_under(c, p)))
        .cloned()
        .collect()
}

/// The brief section that tells the worker which inputs are ground truth.
///
/// `None` when the molecule declared no protected path, so the brief of every
/// other molecule is unchanged.
#[must_use]
pub fn brief_section(protected: &[String]) -> Option<String> {
    if protected.is_empty() {
        return None;
    }
    let mut out = String::from(
        "## Protected inputs — read-only ground truth\n\n\
         The following paths are reference data declared by the operator. Read \
         them; never modify, regenerate, move or delete them:\n\n",
    );
    for path in protected {
        out.push_str("- `");
        out.push_str(path);
        out.push_str("`\n");
    }
    out.push_str(
        "\nThey are what your result is checked against. If your output differs \
         from them, the difference is a finding about your output: report it, \
         with the numbers, in your evidence or your molecule notes. Replacing the \
         reference with your own output would make the check pass by \
         construction and prove nothing. `cs done` refuses to merge a branch \
         that changes any of these paths.\n\n",
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn normalisation_removes_dot_slashes_and_trailing_slash() {
        assert_eq!(normalize_protected_path("./ref/").unwrap(), "ref");
        assert_eq!(
            normalize_protected_path("tests//golden/a.csv").unwrap(),
            "tests/golden/a.csv"
        );
    }

    #[test]
    fn absolute_parent_and_empty_paths_are_refused() {
        assert_eq!(
            normalize_protected_path("/etc/passwd"),
            Err(ProtectedPathError::Absolute("/etc/passwd".to_owned()))
        );
        assert!(matches!(
            normalize_protected_path("ref/../../x"),
            Err(ProtectedPathError::ParentComponent(_))
        ));
        assert_eq!(
            normalize_protected_path("./"),
            Err(ProtectedPathError::Empty)
        );
    }

    #[test]
    fn a_directory_protects_everything_below_it_and_nothing_beside_it() {
        assert!(is_under("ref/a.csv", "ref"));
        assert!(is_under("ref/deep/b.csv", "ref"));
        assert!(is_under("ref", "ref"));
        assert!(!is_under("reference.csv", "ref"));
        assert!(!is_under("src/ref/a.csv", "ref"));
    }

    #[test]
    fn only_changed_paths_under_a_protected_entry_are_reported() {
        let changed = owned(&["src/fix.rs", "ref/expected.csv", "README.md"]);
        let protected = owned(&["ref", "golden.json"]);
        assert_eq!(
            modified_protected_paths(&changed, &protected),
            owned(&["ref/expected.csv"])
        );
    }

    #[test]
    fn a_molecule_without_protection_is_unaffected() {
        let changed = owned(&["ref/expected.csv"]);
        assert!(modified_protected_paths(&changed, &[]).is_empty());
        assert!(brief_section(&[]).is_none());
    }

    #[test]
    fn the_brief_names_every_path_and_the_reason() {
        let section = brief_section(&owned(&["ref", "golden.json"])).unwrap();
        assert!(section.contains("- `ref`"));
        assert!(section.contains("- `golden.json`"));
        assert!(section.contains("never modify"));
        assert!(section.contains("`cs done` refuses"));
    }
}
